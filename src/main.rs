//! # `PortLens` - entry point
//!
//! Parses CLI arguments, collects socket data, applies filters, and renders
//! output to stdout.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use portlens::filter::PortFilter;
use portlens::{collector, display, filter};

/// Exit code for runtime errors (failed to enumerate sockets, write errors).
const EXIT_RUNTIME_ERROR: u8 = 1;
/// Exit code for CLI usage errors (invalid flags, conflicting options).
const EXIT_USAGE_ERROR: u8 = 2;

/// Parsed command-line arguments.
// CLI structs inherently use multiple boolean flags for argument toggling.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct Cli {
    tcp: bool,
    udp: bool,
    listen: bool,
    port: Option<PortFilter>,
    all: bool,
    full: bool,
    compact: bool,
    no_header: bool,
    json: bool,
    no_enrich: bool,
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
        /// Target port: kill TCP listeners or UDP binders on this local port.
        port: Option<u16>,
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
    init_logger();

    let args = normalize_args();

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
            eprintln!("error: {e:#}");
            eprintln!();
            eprintln!("Try 'portlens --help' for more information.");
            return ExitCode::from(EXIT_USAGE_ERROR);
        }
    };

    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(EXIT_RUNTIME_ERROR)
        }
    }
}

/// Initialize stderr logger. Reads `RUST_LOG` (default: off). Safe to call
/// once; `try_init` silently ignores duplicate initialization if it occurs.
fn init_logger() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("off"))
        .target(env_logger::Target::Stderr)
        .try_init();
}

/// Normalize CLI arguments to lowercase for case-insensitive matching.
///
/// Skips argv\[0\] (the program name/path) and returns the rest lowercased.
/// Safe because `PortLens` has no string-valued arguments - only numeric
/// port values, flags, and subcommand names, none of which are affected
/// by lowercasing.
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
    let subcommand_idx = args
        .iter()
        .position(|a| matches!(a.to_str(), Some("update" | "kill")));

    let (main_args, command) = if let Some(idx) = subcommand_idx
        && args[idx] == "update"
    {
        let main: Vec<OsString> = args[..idx].to_vec();
        let sub_args: Vec<OsString> = args[idx + 1..].to_vec();

        let mut sub_pargs = pico_args::Arguments::from_vec(sub_args);
        let check = sub_pargs.contains("--check");
        let remaining = sub_pargs.finish();
        if !remaining.is_empty() {
            bail!("unexpected arguments for 'update' subcommand: {remaining:?}");
        }
        (main, Some(Command::Update { check }))
    } else if let Some(idx) = subcommand_idx {
        // Must be "kill" (the only other value the scan accepts).
        let main: Vec<OsString> = args[..idx].to_vec();
        let sub_args: Vec<OsString> = args[idx + 1..].to_vec();

        let mut sub_pargs = pico_args::Arguments::from_vec(sub_args);
        let port: Option<u16> = sub_pargs
            .opt_value_from_str(["-p", "--port"])
            .context("invalid value for '--port' (expected an integer in 1..=65535)")?;
        validate_port_arg(port)?;
        let pid: Option<u32> = sub_pargs
            .opt_value_from_str("--pid")
            .context("invalid value for '--pid' (expected a non-negative integer)")?;
        let force = sub_pargs.contains(["-f", "--force"]);
        let yes = sub_pargs.contains(["-y", "--yes"]);
        let dry_run = sub_pargs.contains("--dry-run");
        let json = sub_pargs.contains("--json");
        let remaining = sub_pargs.finish();
        if !remaining.is_empty() {
            bail!("unexpected arguments for 'kill' subcommand: {remaining:?}");
        }
        match (port, pid) {
            (None, None) => bail!("'kill' requires exactly one of '--port' or '--pid'"),
            (Some(_), Some(_)) => bail!("'--port' and '--pid' cannot be used together"),
            _ => {}
        }
        (
            main,
            Some(Command::Kill {
                port,
                pid,
                force,
                yes,
                dry_run,
                json,
            }),
        )
    } else {
        (args, None)
    };

    if let Some(command) = command.as_ref()
        && !main_args.is_empty()
    {
        bail!(
            "top-level options cannot be used with the '{}' subcommand: {main_args:?}",
            command.name()
        );
    }

    let mut pargs = pico_args::Arguments::from_vec(main_args);

    let tcp = pargs.contains(["-t", "--tcp"]);
    let udp = pargs.contains(["-u", "--udp"]);
    let listen = pargs.contains(["-l", "--listen"]);
    let port: Option<PortFilter> = pargs
        .opt_value_from_str(["-p", "--port"])
        .context("invalid value for '--port' (expected a port number or range like 3000-4000)")?;
    validate_port_filter(port)?;
    let all = pargs.contains(["-a", "--all"]);
    let full = pargs.contains(["-f", "--full"]);
    let compact = pargs.contains(["-c", "--compact"]);
    let no_header = pargs.contains("--no-header");
    let json = pargs.contains("--json");
    let no_enrich = pargs.contains("--no-enrich");

    // Replicate clap's `conflicts_with` validation.
    if tcp && udp {
        bail!("the argument '--tcp' cannot be used with '--udp'");
    }
    if listen && udp {
        bail!("the argument '--listen' cannot be used with '--udp'");
    }

    let remaining = pargs.finish();
    if !remaining.is_empty() {
        bail!("unexpected arguments: {remaining:?}");
    }

    Ok(Cli {
        tcp,
        udp,
        listen,
        port,
        all,
        full,
        compact,
        no_header,
        json,
        no_enrich,
        command,
    })
}

fn validate_port_arg(port: Option<u16>) -> Result<()> {
    if port == Some(0) {
        bail!("invalid value for '--port' (expected an integer in 1..=65535)");
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
    println!("  -a, --all            Show all ports (disable developer-relevant filter)");
    println!("  -f, --full           Show all columns (adds STATE, USER)");
    println!("  -c, --compact        Use compact borderless table style");
    println!("      --no-header      Suppress the column header row");
    println!("      --json           Output results as a JSON array");
    println!("      --no-enrich      Disable Docker/Podman and project-root enrichment");
    println!("  -h, --help           Print help");
    println!("  -v, --version        Print version");
    println!();
    println!("Subcommand 'update' options:");
    println!("      --check          Only check for a new version; do not install");
    println!();
    println!("Subcommand 'kill' options (exactly one of --port or --pid is required):");
    println!("  -p, --port <PORT>    Kill TCP listeners or UDP binders on this local port");
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
    // Dispatch to subcommand if present
    if let Some(command) = cli.command {
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
                    (Some(p), None) => portlens::kill::KillTarget::Port(p),
                    (None, Some(p)) => portlens::kill::KillTarget::Pid(p),
                    _ => unreachable!("parse_cli enforces exactly one selector"),
                };
                portlens::kill::run(portlens::kill::KillOptions {
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
    let filtered = filter::apply(
        entries,
        &filter::FilterOptions {
            tcp_only: cli.tcp,
            udp_only: cli.udp,
            listen_only: cli.listen,
            port: cli.port,
            show_all: cli.all,
        },
    );

    if cli.json {
        display::print_json(&filtered)?;
    } else {
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
        display::print_tips()?;
    }

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
            format!("{error:#}").contains("expected an integer in 1..=65535"),
            "kill port zero should produce the standard usage error"
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
}
