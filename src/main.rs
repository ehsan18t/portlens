//! # `PortLens` - entry point
//!
//! Parses CLI arguments, collects socket data, applies filters, and renders
//! output to stdout.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use log::{debug, trace};
use portlens::display::is_broken_pipe;
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
    no_tips: bool,
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
    let args = normalize_args(std::env::args_os().skip(1));
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
                return exit_after_output(write_help(&mut io::stdout().lock()));
            }
            Some("--version" | "-v") => {
                return exit_after_output(write_version(&mut io::stdout().lock()));
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
        Err(e) if is_broken_pipe(&e) => {
            // The reader went away (e.g. `portlens | head -1`). That is a
            // normal way for a pipeline to end, not a failure.
            debug!("output pipe closed early: {e:#}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            debug!("runtime failed: {e:#}");
            eprintln!("error: {e:#}");
            ExitCode::from(EXIT_RUNTIME_ERROR)
        }
    }
}

/// Map the result of writing help or version text to an exit code.
///
/// A closed pipe (`portlens --help | head -1`) is a clean exit; any other
/// write failure is reported as a runtime error.
fn exit_after_output(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            // stderr may be gone too; there is nowhere left to report that.
            writeln!(io::stderr().lock(), "error: failed to write to stdout: {e}").ok();
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
            // Trace output must never abort the process when stderr is a
            // closed pipe, so write errors are deliberately dropped.
            writeln!(
                io::stderr().lock(),
                "[{} {module}] {}",
                record.level(),
                record.args()
            )
            .ok();
        }
    }

    fn flush(&self) {}
}

/// Flags whose next argument is a value rather than another flag.
const VALUE_FLAGS: &[&str] = &["-p", "--port", "--process", "--grep", "--pid"];

/// Subcommand names, matched only in the first non-`--trace` position.
const SUBCOMMANDS: &[&str] = &["update", "kill"];

/// Normalize CLI arguments for case-insensitive flag matching.
///
/// Expects the arguments without argv\[0\]. Flag names (including the key of
/// `--flag=value`) and a subcommand in command position are ASCII-lowercased.
/// Flag values such as the `--grep` or `--process` text are kept as typed:
/// those filters compare case-insensitively on their own. Arguments that are
/// not valid Unicode are passed through untouched.
fn normalize_args(raw: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut normalized: Vec<OsString> = Vec::new();
    let mut expects_value = false;

    for arg in raw {
        let text = match arg.into_string() {
            Ok(text) => text,
            Err(original) => {
                expects_value = false;
                normalized.push(original);
                continue;
            }
        };

        let value = if expects_value {
            expects_value = false;
            text
        } else if text.starts_with('-') {
            let flag = lowercase_flag_name(&text);
            expects_value = VALUE_FLAGS.contains(&flag.as_str());
            flag
        } else if normalized.iter().all(is_trace_flag)
            && SUBCOMMANDS
                .iter()
                .any(|name| text.eq_ignore_ascii_case(name))
        {
            text.to_ascii_lowercase()
        } else {
            text
        };
        normalized.push(OsString::from(value));
    }

    normalized
}

/// Lowercase a flag name, leaving any inline `=value` part as typed.
fn lowercase_flag_name(flag: &str) -> String {
    flag.split_once('=').map_or_else(
        || flag.to_ascii_lowercase(),
        |(name, value)| format!("{}={value}", name.to_ascii_lowercase()),
    )
}

/// Parse CLI arguments into a [`Cli`] struct.
///
/// A subcommand (`update`, `kill`) is recognized only as the first argument,
/// optionally preceded by `--trace`. Anything after it is consumed by the
/// matching subcommand parser. A subcommand name anywhere else is either a
/// flag value (`--grep kill`) or a usage error.
fn parse_cli(args: Vec<OsString>) -> Result<Cli> {
    let (main_args, command) = split_main_args_and_command(args)?;
    parse_main_cli(main_args, command)
}

fn split_main_args_and_command(args: Vec<OsString>) -> Result<(Vec<OsString>, Option<Command>)> {
    let idx = args.iter().take_while(|arg| is_trace_flag(arg)).count();
    let parse_command = match args.get(idx).and_then(|arg| arg.to_str()) {
        Some("update") => parse_update_command,
        Some("kill") => parse_kill_command,
        _ => return Ok((args, None)),
    };

    // `--trace` is global: it may sit before or after the subcommand. Hand
    // every occurrence to the top-level parser so the subcommand parsers
    // never see it.
    let mut main_args = args[..idx].to_vec();
    let (trace_flags, sub_args): (Vec<OsString>, Vec<OsString>) =
        args[idx + 1..].iter().cloned().partition(is_trace_flag);
    main_args.extend(trace_flags);

    Ok((main_args, Some(parse_command(sub_args)?)))
}

fn is_trace_flag(arg: &OsString) -> bool {
    arg.to_str() == Some("--trace")
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
    let no_tips = pargs.contains("--no-tips");
    let trace = pargs.contains("--trace");

    validate_main_flag_conflicts(tcp, udp, listen, process.as_deref(), grep.as_deref())?;

    let remaining = pargs.finish();
    if let Some(name) = remaining
        .first()
        .and_then(|arg| arg.to_str())
        .filter(|arg| SUBCOMMANDS.contains(arg))
    {
        bail!(
            "'{name}' must be the first argument; top-level options cannot be combined with a subcommand"
        );
    }
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
        no_tips,
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

/// Help text printed after the `PortLens <version>` line.
const HELP_BODY: &str = "\
List open network ports and their associated processes.

Usage: portlens [OPTIONS] [COMMAND]

Commands:
  update  Check for updates and optionally self-update the binary
  kill    Terminate processes by --port or --pid

Options:
  -t, --tcp            Show only TCP sockets
  -u, --udp            Show only UDP sockets
  -l, --listen         Show only sockets in LISTEN state (TCP only)
  -p, --port <PORT>    Filter results to a port or range (e.g. 3000 or 3000-4000)
      --process <NAME> Filter by exact process name (without .exe suffix)
      --grep <TEXT>    Filter by substring match in process name
  -a, --all            Show all ports (disable developer-relevant filter)
  -f, --full           Show all columns (adds STATE, USER)
  -c, --compact        Use compact borderless table style
      --no-header      Suppress the column header row
      --json           Output results as a JSON array
      --no-enrich      Disable Docker/Podman and project-root enrichment
      --no-tips        Hide the tips panel (or set PORTLENS_NO_TIPS=1)
      --trace          Emit diagnostic trace to stderr for debugging
  -h, --help           Print help
  -v, --version        Print version

Subcommand 'update' options:
      --check          Only check for a new version; do not install

Subcommand 'kill' options (exactly one of --port or --pid is required):
  -p, --port <PORT>    Kill TCP listeners or UDP binders on a local port or range
                       (e.g. 3000 or 3000-4000)
                       (stops published containers via daemon API, not proxy PID)
                       (use --pid if daemon lookup fails or is ambiguous)
      --pid <PID>      Kill the given PID
  -f, --force          Forceful termination (SIGKILL on Unix)
  -y, --yes            Skip interactive confirmation
                       (required when stdin is not a terminal)
      --dry-run        List targets without killing anything
      --json           Emit the kill report or dry-run target list as JSON
";

/// Write the `--help` text.
fn write_help(out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "PortLens {}", env!("CARGO_PKG_VERSION"))?;
    out.write_all(HELP_BODY.as_bytes())?;
    out.flush()
}

/// Write the `--version` text.
fn write_version(out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "PortLens {}", env!("CARGO_PKG_VERSION"))?;
    out.flush()
}

/// Environment variable that hides the tips panel when set to any non-empty value.
const NO_TIPS_ENV: &str = "PORTLENS_NO_TIPS";

/// Whether the `PORTLENS_NO_TIPS` value (if any) opts out of the tips panel.
fn tips_disabled_by_env(value: Option<OsString>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// Application entry point, separated from `main()` for testability.
///
/// Returns the process exit code as a `u8` so subcommands (notably `kill`)
/// can surface partial-success states (e.g. exit 3 for "nothing to kill").
fn run(cli: Cli) -> Result<u8> {
    debug!(
        "cli parsed: tcp={} udp={} listen={} port={:?} process={:?} grep={:?} all={} full={} compact={} no_header={} json={} no_enrich={} no_tips={} trace={} command={:?}",
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
        cli.no_tips,
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

    let filter_options = filter::FilterOptions {
        tcp_only: cli.tcp,
        udp_only: cli.udp,
        listen_only: cli.listen,
        port: cli.port,
        process: cli.process,
        grep: cli.grep,
        show_all: cli.all,
    };
    let filtered = filter::apply(entries, &filter_options);
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

        // JSON stays a bare `[]`; only a person at a terminal gets the hint.
        if filtered.is_empty() && std::io::stderr().is_terminal() {
            display::print_empty_hint(filter_options.relevance_filter_active())?;
        }
    }

    if std::io::stderr().is_terminal()
        && let Some(warning) = collector::visibility_warning()
    {
        writeln!(std::io::stderr().lock(), "warning: {warning}")
            .context("failed to write visibility warning to stderr")?;
    }

    if !cli.json
        && !cli.no_tips
        && !tips_disabled_by_env(std::env::var_os(NO_TIPS_ENV))
        && std::io::stdout().is_terminal()
    {
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

    /// A writer whose reader has gone away, like stdout piped into `head -1`.
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn help_and_version_surface_broken_pipe_instead_of_panicking() {
        let help = write_help(&mut ClosedPipe).expect_err("closed pipe should fail");
        assert_eq!(help.kind(), io::ErrorKind::BrokenPipe);

        let version = write_version(&mut ClosedPipe).expect_err("closed pipe should fail");
        assert_eq!(version.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn help_text_lists_every_command() {
        let mut buffer = Vec::new();
        write_help(&mut buffer).expect("writing help to a buffer should succeed");
        let help = String::from_utf8(buffer).expect("help should be UTF-8");

        assert!(
            help.starts_with("PortLens "),
            "help should start with the name"
        );
        assert!(
            help.contains("  kill "),
            "help should list the kill command"
        );
        assert!(
            help.contains("  update "),
            "help should list the update command"
        );
    }

    #[test]
    fn is_broken_pipe_detects_io_error_behind_context() {
        let error = writeln!(ClosedPipe, "row")
            .context("failed to write table to stdout")
            .expect_err("closed pipe should fail");

        assert!(
            is_broken_pipe(&error),
            "context must not hide the pipe error"
        );
    }

    #[test]
    fn is_broken_pipe_detects_serde_json_io_error() {
        let error = serde_json::to_writer(ClosedPipe, &[1, 2, 3])
            .context("failed to serialize kill report")
            .expect_err("closed pipe should fail");

        assert!(
            is_broken_pipe(&error),
            "serde_json must not hide the pipe error"
        );
    }

    #[test]
    fn is_broken_pipe_ignores_other_errors() {
        let io_error = anyhow::Error::new(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(
            !is_broken_pipe(&io_error),
            "only BrokenPipe is a clean exit"
        );

        let plain = anyhow::anyhow!("failed to enumerate sockets");
        assert!(!is_broken_pipe(&plain), "non-io errors are real failures");
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
            format!("{error:#}").contains("'kill' must be the first argument"),
            "kill should reject stray top-level flags before the subcommand"
        );
    }

    #[test]
    fn parse_cli_rejects_top_level_flags_before_update_subcommand() {
        let error = parse_cli(args(&["--port", "3000", "update"]))
            .expect_err("top-level flags must not be silently ignored for update");

        assert!(
            format!("{error:#}").contains("'update' must be the first argument"),
            "update should reject stray top-level flags before the subcommand"
        );
    }

    #[test]
    fn parse_cli_accepts_trace_before_subcommands() {
        let cli = parse_cli(args(&["--trace", "update", "--check"]))
            .expect("--trace before update should parse");
        assert!(cli.trace, "leading --trace should enable tracing");
        assert!(
            matches!(cli.command, Some(Command::Update { check: true })),
            "update --check should still parse"
        );

        let cli = parse_cli(args(&["--trace", "kill", "--pid", "1234", "--dry-run"]))
            .expect("--trace before kill should parse");
        assert!(cli.trace, "leading --trace should enable tracing");
        assert!(
            matches!(
                cli.command,
                Some(Command::Kill {
                    pid: Some(1234),
                    dry_run: true,
                    ..
                })
            ),
            "kill options should still parse"
        );
    }

    #[test]
    fn parse_cli_accepts_trace_after_subcommands() {
        let cli = parse_cli(args(&["update", "--check", "--trace"]))
            .expect("--trace after update should parse");
        assert!(cli.trace, "trailing --trace should enable tracing");
        assert!(
            matches!(cli.command, Some(Command::Update { check: true })),
            "update --check should still parse"
        );

        let cli = parse_cli(args(&["kill", "--trace", "--port", "3000", "--dry-run"]))
            .expect("--trace inside kill options should parse");
        assert!(
            cli.trace,
            "--trace among kill options should enable tracing"
        );
        assert!(
            matches!(
                cli.command,
                Some(Command::Kill {
                    port: Some(PortFilter::Single(3000)),
                    ..
                })
            ),
            "kill --port should still parse"
        );
    }

    #[test]
    fn parse_cli_still_rejects_other_flags_before_subcommand_with_trace() {
        let error = parse_cli(args(&["--trace", "--json", "update"]))
            .expect_err("--json is not a valid top-level option for update");

        assert!(
            format!("{error:#}").contains("'update' must be the first argument"),
            "only --trace may accompany a subcommand: {error:#}"
        );
    }

    #[test]
    fn parse_cli_treats_subcommand_names_after_value_flags_as_values() {
        let cli = parse_cli(args(&["--grep", "kill"])).expect("--grep kill should parse");
        assert!(cli.command.is_none(), "'kill' is the grep pattern here");
        assert_eq!(cli.grep.as_deref(), Some("kill"));

        let cli =
            parse_cli(args(&["--process", "update", "-t"])).expect("--process update should parse");
        assert!(cli.command.is_none(), "'update' is the process name here");
        assert_eq!(cli.process.as_deref(), Some("update"));
        assert!(cli.tcp, "flags after the value should still parse");
    }

    #[test]
    fn parse_cli_rejects_subcommand_after_top_level_flag() {
        let error = parse_cli(args(&["-a", "kill", "--pid", "1234"]))
            .expect_err("a subcommand is only recognized in first position");

        assert!(
            format!("{error:#}").contains("must be the first argument"),
            "a misplaced subcommand should get a targeted hint: {error:#}"
        );
    }

    #[test]
    fn normalize_args_lowercases_flags_but_keeps_values() {
        let normalized = normalize_args(args(&[
            "--GREP",
            "MyApp",
            "-A",
            "--Process=Node",
            "--PORT",
            "3000",
        ]));

        assert_eq!(
            normalized,
            args(&["--grep", "MyApp", "-a", "--process=Node", "--port", "3000"]),
            "flag names fold to lowercase, values stay as typed"
        );
    }

    #[test]
    fn normalize_args_lowercases_subcommand_only_in_command_position() {
        assert_eq!(
            normalize_args(args(&["--TRACE", "KILL", "--PID", "42", "--Dry-Run"])),
            args(&["--trace", "kill", "--pid", "42", "--dry-run"]),
            "a leading subcommand is case-insensitive"
        );
        assert_eq!(
            normalize_args(args(&["-a", "Update"])),
            args(&["-a", "Update"]),
            "a later positional token is not a subcommand"
        );
    }

    #[test]
    fn mixed_case_grep_value_reaches_the_filter_unchanged() {
        let cli = parse_cli(normalize_args(args(&["--Grep", "VSCode"])))
            .expect("mixed-case grep should parse");
        assert_eq!(cli.grep.as_deref(), Some("VSCode"));
        assert!(cli.command.is_none(), "no subcommand expected");
    }

    #[test]
    fn parse_cli_accepts_no_tips_flag() {
        let cli = parse_cli(args(&["--no-tips", "-a"])).expect("--no-tips should parse");
        assert!(cli.no_tips, "--no-tips should be recorded");
        assert!(!parse_cli(args(&[])).expect("no args").no_tips);
    }

    #[test]
    fn tips_env_opt_out_requires_a_non_empty_value() {
        assert!(!tips_disabled_by_env(None), "unset keeps tips");
        assert!(
            !tips_disabled_by_env(Some(OsString::new())),
            "an empty value keeps tips"
        );
        assert!(tips_disabled_by_env(Some(OsString::from("1"))));
        assert!(
            tips_disabled_by_env(Some(OsString::from("0"))),
            "any non-empty value opts out"
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
