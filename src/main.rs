//! # `PortLens` - entry point
//!
//! Parses CLI arguments, collects socket data, applies filters, and renders
//! output to stdout.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use log::{debug, trace};
use portlens::filter::PortFilter;
use portlens::{collector, display, filter};

/// Exit code for runtime errors (failed to enumerate sockets, write errors).
const EXIT_RUNTIME_ERROR: u8 = 1;
/// Exit code for CLI usage errors (invalid flags, conflicting options).
const EXIT_USAGE_ERROR: u8 = 2;

static STDERR_TRACE_LOGGER: StderrTraceLogger = StderrTraceLogger;

/// Parsed command-line arguments.
// CLI structs inherently use multiple boolean flags for argument toggling.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct Cli {
    tcp: bool,
    udp: bool,
    listen: bool,
    port: Option<PortFilter>,
    process: Option<String>,
    grep: Option<String>,
    all: bool,
    full: bool,
    compact: bool,
    no_header: bool,
    json: bool,
    no_enrich: bool,
    trace: bool,
    command: Option<Command>,
}

/// Subcommand dispatch.
#[derive(Debug)]
enum Command {
    /// Check for updates and optionally self-update the binary.
    Update {
        /// Only check for a new version without downloading or installing.
        check: bool,
    },
    /// Terminate a process by port or PID.
    Kill {
        /// Target port or range: kill TCP listeners or UDP binders on these local ports.
        port: Option<PortFilter>,
        /// Target PID: kill this specific process.
        pid: Option<u32>,
        /// Escalate to forceful termination (SIGKILL on Unix).
        force: bool,
        /// Skip the interactive confirmation prompt.
        yes: bool,
        /// Resolve targets and report them without signaling anything.
        dry_run: bool,
        /// Emit the kill report as JSON.
        json: bool,
    },
}

impl Command {
    const fn name(&self) -> &'static str {
        match self {
            Self::Update { .. } => "update",
            Self::Kill { .. } => "kill",
        }
    }
}

fn main() -> ExitCode {
    let args = normalize_args();
    // Check for --trace early (before parsing) so diagnostic output
    // covers the entire argument validation flow.
    let trace_enabled = args.iter().any(|a| a.to_str() == Some("--trace"));
    init_logger(trace_enabled);
    debug!("startup: trace_enabled={trace_enabled} args={args:?}");

    // Handle --help / --version before the parser so they short-circuit
    // even when combined with otherwise-invalid flags.
    for arg in &args {
        match arg.to_str() {
            Some("--help" | "-h") => {
                print_help();
                return ExitCode::SUCCESS;
            }
            Some("--version" | "-v") => {
                print_version();
                return ExitCode::SUCCESS;
            }
            _ => {}
        }
    }

    let cli = match parse_cli(args) {
        Ok(cli) => cli,
        Err(e) => {
            debug!("cli parsing failed: {e:#}");
            eprintln!("error: {e:#}");
            eprintln!();
            eprintln!("Try 'portlens --help' for more information.");
            return ExitCode::from(EXIT_USAGE_ERROR);
        }
    };

    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            debug!("runtime failed: {e:#}");
            eprintln!("error: {e:#}");
            ExitCode::from(EXIT_RUNTIME_ERROR)
        }
    }
}

/// Initialize the global stderr logger.
///
/// When `trace` is true, emits all `log::debug!` and above to stderr.
/// When false, logging is completely silent (the `log` facade compiles
/// away to no-ops when no logger is installed).
fn init_logger(trace: bool) {
    if trace && log::set_logger(&STDERR_TRACE_LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
}

/// Minimal logger that writes every enabled record to stderr.
///
/// Format: `[LEVEL module] message`
struct StderrTraceLogger;

impl log::Log for StderrTraceLogger {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let module = record.module_path().unwrap_or("-");
            eprintln!("[{} {module}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

/// Normalize CLI arguments to lowercase for case-insensitive matching.
///
/// Skips argv\[0\] (the program name/path) and returns the rest lowercased.
/// String-valued arguments (`--process`, `--grep`) are lowercased too,
/// which is intentional because process-name matching is always
/// case-insensitive.
fn normalize_args() -> Vec<OsString> {
    std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string().map_or_else(
                |original| original,
                |s| OsString::from(s.to_ascii_lowercase()),
            )
        })
        .collect()
}

/// Parse CLI arguments into a [`Cli`] struct.
///
/// Subcommands are detected by scanning for the first occurrence of a known
/// subcommand token (`update`, `kill`); anything after the token is consumed
/// by the matching subcommand parser. The earliest-occurring token wins so
/// that `portlens kill update` is parsed as `kill` with a stray `update`
/// argument (a usage error), not as `update` with a stray `kill` argument.
fn parse_cli(args: Vec<OsString>) -> Result<Cli> {
    let (main_args, command) = split_main_args_and_command(args)?;
    reject_mixed_main_and_subcommand_args(&main_args, command.as_ref())?;
    parse_main_cli(main_args, command)
}

fn split_main_args_and_command(args: Vec<OsString>) -> Result<(Vec<OsString>, Option<Command>)> {
    let Some(idx) = args
        .iter()
        .position(|arg| matches!(arg.to_str(), Some("update" | "kill")))
    else {
        return Ok((args, None));
    };

    let main_args = args[..idx].to_vec();
    let sub_args = args[idx + 1..].to_vec();
    let command = match args[idx].to_str() {
        Some("update") => parse_update_command(sub_args)?,
        Some("kill") => parse_kill_command(sub_args)?,
        _ => unreachable!("subcommand scan only accepts known commands"),
    };

    Ok((main_args, Some(command)))
}

fn parse_update_command(args: Vec<OsString>) -> Result<Command> {
    let mut pargs = pico_args::Arguments::from_vec(args);
    let check = pargs.contains("--check");
    let remaining = pargs.finish();
    if !remaining.is_empty() {
        bail!("unexpected arguments for 'update' subcommand: {remaining:?}");
    }

    Ok(Command::Update { check })
}

fn parse_kill_command(args: Vec<OsString>) -> Result<Command> {
    let mut pargs = pico_args::Arguments::from_vec(args);
    let port = parse_optional_port_filter(
        &mut pargs,
        "invalid value for '--port' (expected a port or range like 3000-4000)",
    )?;
    let pid: Option<u32> = pargs
        .opt_value_from_str("--pid")
        .context("invalid value for '--pid' (expected a non-negative integer)")?;
    let force = pargs.contains(["-f", "--force"]);
    let yes = pargs.contains(["-y", "--yes"]);
    let dry_run = pargs.contains("--dry-run");
    let json = pargs.contains("--json");
    let remaining = pargs.finish();
    if !remaining.is_empty() {
        bail!("unexpected arguments for 'kill' subcommand: {remaining:?}");
    }

    validate_kill_selector(port, pid)?;

    Ok(Command::Kill {
        port,
        pid,
        force,
        yes,
        dry_run,
        json,
    })
}

fn validate_kill_selector(port: Option<PortFilter>, pid: Option<u32>) -> Result<()> {
    match (port, pid) {
        (None, None) => bail!("'kill' requires exactly one of '--port' or '--pid'"),
        (Some(_), Some(_)) => bail!("'--port' and '--pid' cannot be used together"),
        _ => Ok(()),
    }
}

fn reject_mixed_main_and_subcommand_args(
    main_args: &[OsString],
    command: Option<&Command>,
) -> Result<()> {
    if let Some(command) = command
        && !main_args.is_empty()
    {
        bail!(
            "top-level options cannot be used with the '{}' subcommand: {main_args:?}",
            command.name()
        );
    }

    Ok(())
}

fn parse_main_cli(main_args: Vec<OsString>, command: Option<Command>) -> Result<Cli> {
    let mut pargs = pico_args::Arguments::from_vec(main_args);

    let tcp = pargs.contains(["-t", "--tcp"]);
    let udp = pargs.contains(["-u", "--udp"]);
    let listen = pargs.contains(["-l", "--listen"]);
    let port = parse_optional_port_filter(
        &mut pargs,
        "invalid value for '--port' (expected a port number or range like 3000-4000)",
    )?;
    let process: Option<String> = pargs
        .opt_value_from_str("--process")
        .context("invalid value for '--process' (expected a process name)")?;
    let grep: Option<String> = pargs
        .opt_value_from_str("--grep")
        .context("invalid value for '--grep' (expected a search pattern)")?;
    let all = pargs.contains(["-a", "--all"]);
    let full = pargs.contains(["-f", "--full"]);
    let compact = pargs.contains(["-c", "--compact"]);
    let no_header = pargs.contains("--no-header");
    let json = pargs.contains("--json");
    let no_enrich = pargs.contains("--no-enrich");
    let trace = pargs.contains("--trace");

    validate_main_flag_conflicts(tcp, udp, listen, process.as_deref(), grep.as_deref())?;

    let remaining = pargs.finish();
    if !remaining.is_empty() {
        bail!("unexpected arguments: {remaining:?}");
    }

    Ok(Cli {
        tcp,
        udp,
        listen,
        port,
        process,
        grep,
        all,
        full,
        compact,
        no_header,
        json,
        no_enrich,
        trace,
        command,
    })
}

fn parse_optional_port_filter(
    pargs: &mut pico_args::Arguments,
    error_message: &'static str,
) -> Result<Option<PortFilter>> {
    let port = pargs
        .opt_value_from_str(["-p", "--port"])
        .context(error_message)?;
    validate_port_filter(port)?;
    Ok(port)
}

fn validate_main_flag_conflicts(
    tcp: bool,
    udp: bool,
    listen: bool,
    process: Option<&str>,
    grep: Option<&str>,
) -> Result<()> {
    if tcp && udp {
        bail!("the argument '--tcp' cannot be used with '--udp'");
    }
    if listen && udp {
        bail!("the argument '--listen' cannot be used with '--udp'");
    }
    if process.is_some() && grep.is_some() {
        bail!("the argument '--process' cannot be used with '--grep'");
    }

    Ok(())
}

/// Validate a [`PortFilter`] from `--port`, rejecting port 0 in either variant.
fn validate_port_filter(port: Option<PortFilter>) -> Result<()> {
    if let Some(filter) = port
        && filter.contains_zero()
    {
        bail!("invalid value for '--port' (port numbers must be in 1..=65535)");
    }

    Ok(())
}

fn print_help() {
    let version = env!("CARGO_PKG_VERSION");
    println!("PortLens {version}");
    println!("List open network ports and their associated processes.");
    println!();
    println!("Usage: portlens [OPTIONS] [COMMAND]");
    println!();
    println!("Commands:");
    println!("  update  Check for updates and optionally self-update the binary");
    println!("  kill    Terminate processes by --port or --pid");
    println!();
    println!("Options:");
    println!("  -t, --tcp            Show only TCP sockets");
    println!("  -u, --udp            Show only UDP sockets");
    println!("  -l, --listen         Show only sockets in LISTEN state (TCP only)");
    println!("  -p, --port <PORT>    Filter results to a port or range (e.g. 3000 or 3000-4000)");
    println!("      --process <NAME> Filter by exact process name (without .exe suffix)");
    println!("      --grep <TEXT>    Filter by substring match in process name");
    println!("  -a, --all            Show all ports (disable developer-relevant filter)");
    println!("  -f, --full           Show all columns (adds STATE, USER)");
    println!("  -c, --compact        Use compact borderless table style");
    println!("      --no-header      Suppress the column header row");
    println!("      --json           Output results as a JSON array");
    println!("      --no-enrich      Disable Docker/Podman and project-root enrichment");
    println!("      --trace          Emit diagnostic trace to stderr for debugging");
    println!("  -h, --help           Print help");
    println!("  -v, --version        Print version");
    println!();
    println!("Subcommand 'update' options:");
    println!("      --check          Only check for a new version; do not install");
    println!();
    println!("Subcommand 'kill' options (exactly one of --port or --pid is required):");
    println!("  -p, --port <PORT>    Kill TCP listeners or UDP binders on a local port or range");
    println!("                       (e.g. 3000 or 3000-4000)");
    println!("                       (stops published containers via daemon API, not proxy PID)");
    println!("                       (use --pid if daemon lookup fails or is ambiguous)");
    println!("      --pid <PID>      Kill the given PID");
    println!("  -f, --force          Forceful termination (SIGKILL on Unix)");
    println!("  -y, --yes            Skip interactive confirmation");
    println!("      --dry-run        List targets without killing anything");
    println!("      --json           Emit the kill report or dry-run target list as JSON");
}

fn print_version() {
    println!("PortLens {}", env!("CARGO_PKG_VERSION"));
}

/// Application entry point, separated from `main()` for testability.
///
/// Returns the process exit code as a `u8` so subcommands (notably `kill`)
/// can surface partial-success states (e.g. exit 3 for "nothing to kill").
fn run(cli: Cli) -> Result<u8> {
    debug!(
        "cli parsed: tcp={} udp={} listen={} port={:?} process={:?} grep={:?} all={} full={} compact={} no_header={} json={} no_enrich={} trace={} command={:?}",
        cli.tcp,
        cli.udp,
        cli.listen,
        cli.port,
        cli.process,
        cli.grep,
        cli.all,
        cli.full,
        cli.compact,
        cli.no_header,
        cli.json,
        cli.no_enrich,
        cli.trace,
        cli.command.as_ref().map(Command::name)
    );

    // Dispatch to subcommand if present
    if let Some(command) = cli.command {
        debug!("dispatching subcommand: {}", command.name());
        return match command {
            Command::Update { check } => portlens::update::run(check).map(|()| 0),
            Command::Kill {
                port,
                pid,
                force,
                yes,
                dry_run,
                json,
            } => {
                let target = match (port, pid) {
                    (Some(f), None) => portlens::kill::KillTarget::Port(f),
                    (None, Some(p)) => portlens::kill::KillTarget::Pid(p),
                    _ => unreachable!("parse_cli enforces exactly one selector"),
                };
                portlens::kill::run(&portlens::kill::KillOptions {
                    target,
                    force,
                    yes,
                    dry_run,
                    json,
                })
            }
        };
    }

    let entries = collector::collect_with_options(&collector::CollectOptions {
        deep_enrichment: !cli.no_enrich,
    })?;
    debug!("collected {} raw entries before filtering", entries.len());

    let filtered = filter::apply(
        entries,
        &filter::FilterOptions {
            tcp_only: cli.tcp,
            udp_only: cli.udp,
            listen_only: cli.listen,
            port: cli.port,
            process: cli.process,
            grep: cli.grep,
            show_all: cli.all,
        },
    );
    debug!("filter pass complete: {} entries surviving", filtered.len());

    if cli.json {
        debug!("rendering json output: entries={}", filtered.len());
        display::print_json(&filtered)?;
    } else {
        debug!(
            "rendering table output: entries={} full={} compact={} show_header={}",
            filtered.len(),
            cli.full,
            cli.compact,
            !cli.no_header
        );
        display::print_table(
            &filtered,
            &display::DisplayOptions {
                show_header: !cli.no_header,
                full: cli.full,
                compact: cli.compact,
            },
        )?;
    }

    if std::io::stderr().is_terminal()
        && let Some(warning) = collector::visibility_warning()
    {
        writeln!(std::io::stderr().lock(), "warning: {warning}")
            .context("failed to write visibility warning to stderr")?;
    }

    if !cli.json && std::io::stdout().is_terminal() {
        trace!("printing interactive tips footer");
        display::print_tips()?;
    }

    debug!("run completed successfully");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parse_cli_rejects_global_port_zero() {
        let error = parse_cli(args(&["--port", "0"]))
            .expect_err("top-level --port 0 should be rejected during parsing");

        assert!(
            format!("{error:#}").contains("port numbers must be in 1..=65535"),
            "port zero should produce the standard usage error"
        );
    }

    #[test]
    fn parse_cli_rejects_global_port_range_with_zero() {
        let error = parse_cli(args(&["--port", "0-100"]))
            .expect_err("top-level --port 0-100 should be rejected during parsing");

        assert!(
            format!("{error:#}").contains("port numbers must be in 1..=65535"),
            "port range starting at zero should produce a usage error"
        );
    }

    #[test]
    fn parse_cli_accepts_single_port() {
        let cli = parse_cli(args(&["--port", "8080"])).expect("single port should parse");
        assert_eq!(
            cli.port,
            Some(PortFilter::Single(8080)),
            "single port should be stored as PortFilter::Single"
        );
    }

    #[test]
    fn parse_cli_accepts_port_range() {
        let cli = parse_cli(args(&["--port", "3000-4000"])).expect("port range should parse");
        assert_eq!(
            cli.port,
            Some(PortFilter::Range {
                start: 3000,
                end: 4000
            }),
            "port range should be stored as PortFilter::Range"
        );
    }

    #[test]
    fn parse_cli_rejects_reversed_port_range() {
        let error = parse_cli(args(&["--port", "5000-3000"]))
            .expect_err("reversed range should be rejected");

        assert!(
            format!("{error:#}").contains("must not exceed"),
            "reversed range should report start > end: {error:#}"
        );
    }

    #[test]
    fn parse_cli_rejects_non_numeric_port() {
        let error =
            parse_cli(args(&["--port", "abc"])).expect_err("non-numeric port should be rejected");

        assert!(
            format!("{error:#}").contains("not a valid port number"),
            "non-numeric port should report a parsing failure: {error:#}"
        );
    }

    #[test]
    fn parse_cli_rejects_kill_port_zero() {
        let error = parse_cli(args(&["kill", "--port", "0"]))
            .expect_err("kill --port 0 should be rejected during parsing");

        assert!(
            format!("{error:#}").contains("port numbers must be in 1..=65535"),
            "kill port zero should produce the standard usage error"
        );
    }

    #[test]
    fn parse_cli_accepts_kill_single_port() {
        let cli =
            parse_cli(args(&["kill", "--port", "3000"])).expect("kill single port should parse");
        match cli.command {
            Some(Command::Kill { port, .. }) => {
                assert_eq!(
                    port,
                    Some(PortFilter::Single(3000)),
                    "kill --port 3000 should parse as Single"
                );
            }
            _ => panic!("expected Kill command"),
        }
    }

    #[test]
    fn parse_cli_accepts_kill_port_range() {
        let cli = parse_cli(args(&["kill", "--port", "3000-4000"]))
            .expect("kill port range should parse");
        match cli.command {
            Some(Command::Kill { port, .. }) => {
                assert_eq!(
                    port,
                    Some(PortFilter::Range {
                        start: 3000,
                        end: 4000
                    }),
                    "kill --port 3000-4000 should parse as Range"
                );
            }
            _ => panic!("expected Kill command"),
        }
    }

    #[test]
    fn parse_cli_rejects_kill_reversed_port_range() {
        let error = parse_cli(args(&["kill", "--port", "5000-3000"]))
            .expect_err("kill reversed range should be rejected");

        assert!(
            format!("{error:#}").contains("must not exceed"),
            "kill reversed range should report start > end: {error:#}"
        );
    }

    #[test]
    fn parse_cli_rejects_kill_port_range_with_zero() {
        let error = parse_cli(args(&["kill", "--port", "0-100"]))
            .expect_err("kill --port 0-100 should be rejected");

        assert!(
            format!("{error:#}").contains("port numbers must be in 1..=65535"),
            "kill port range starting at zero should be rejected"
        );
    }

    #[test]
    fn parse_cli_uses_first_subcommand_token() {
        let error = parse_cli(args(&["kill", "update"]))
            .expect_err("kill update should be parsed as kill with a stray argument");

        assert!(
            format!("{error:#}").contains("unexpected arguments for 'kill' subcommand"),
            "the earliest subcommand token should win"
        );
    }

    #[test]
    fn parse_cli_rejects_top_level_flags_before_kill_subcommand() {
        let error = parse_cli(args(&["--json", "kill", "--pid", "1234"]))
            .expect_err("top-level flags must not be silently ignored for kill");

        assert!(
            format!("{error:#}")
                .contains("top-level options cannot be used with the 'kill' subcommand"),
            "kill should reject stray top-level flags before the subcommand"
        );
    }

    #[test]
    fn parse_cli_rejects_top_level_flags_before_update_subcommand() {
        let error = parse_cli(args(&["--port", "3000", "update"]))
            .expect_err("top-level flags must not be silently ignored for update");

        assert!(
            format!("{error:#}")
                .contains("top-level options cannot be used with the 'update' subcommand"),
            "update should reject stray top-level flags before the subcommand"
        );
    }

    #[test]
    fn parse_cli_accepts_process_flag() {
        let cli =
            parse_cli(args(&["--process", "node"])).expect("--process with value should parse");
        assert_eq!(
            cli.process.as_deref(),
            Some("node"),
            "--process should store the process name"
        );
    }

    #[test]
    fn parse_cli_accepts_grep_flag() {
        let cli = parse_cli(args(&["--grep", "docker"])).expect("--grep with value should parse");
        assert_eq!(
            cli.grep.as_deref(),
            Some("docker"),
            "--grep should store the search pattern"
        );
    }

    #[test]
    fn parse_cli_rejects_process_and_grep_together() {
        let error = parse_cli(args(&["--process", "node", "--grep", "docker"]))
            .expect_err("--process and --grep should be mutually exclusive");

        assert!(
            format!("{error:#}").contains("'--process' cannot be used with '--grep'"),
            "--process + --grep should produce a conflict error: {error:#}"
        );
    }

    #[test]
    fn parse_cli_process_combined_with_port() {
        let cli = parse_cli(args(&["--process", "node", "--port", "3000"]))
            .expect("--process combined with --port should parse");
        assert_eq!(
            cli.process.as_deref(),
            Some("node"),
            "process should be set"
        );
        assert_eq!(
            cli.port,
            Some(PortFilter::Single(3000)),
            "port should be set"
        );
    }

    #[test]
    fn parse_cli_grep_combined_with_tcp() {
        let cli = parse_cli(args(&["--grep", "docker", "--tcp"]))
            .expect("--grep combined with --tcp should parse");
        assert_eq!(cli.grep.as_deref(), Some("docker"), "grep should be set");
        assert!(cli.tcp, "tcp flag should be set");
    }
}
