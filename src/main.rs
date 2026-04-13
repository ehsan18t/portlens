//! # `PortLens` - entry point
//!
//! Parses CLI arguments, collects socket data, applies filters, and renders
//! output to stdout.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
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
    port: Option<u16>,
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

    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        return ExitCode::from(EXIT_RUNTIME_ERROR);
    }
    ExitCode::SUCCESS
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
/// Subcommands are detected by scanning for the literal token `update`;
/// anything after it is consumed by the subcommand parser.
fn parse_cli(args: Vec<OsString>) -> Result<Cli> {
    let update_idx = args.iter().position(|a| a == "update");

    let (main_args, command) = if let Some(idx) = update_idx {
        let main: Vec<OsString> = args[..idx].to_vec();
        let sub_args: Vec<OsString> = args[idx + 1..].to_vec();

        let mut sub_pargs = pico_args::Arguments::from_vec(sub_args);
        let check = sub_pargs.contains("--check");
        let remaining = sub_pargs.finish();
        if !remaining.is_empty() {
            bail!("unexpected arguments for 'update' subcommand: {remaining:?}");
        }
        (main, Some(Command::Update { check }))
    } else {
        (args, None)
    };

    let mut pargs = pico_args::Arguments::from_vec(main_args);

    let tcp = pargs.contains(["-t", "--tcp"]);
    let udp = pargs.contains(["-u", "--udp"]);
    let listen = pargs.contains(["-l", "--listen"]);
    let port: Option<u16> = pargs
        .opt_value_from_str(["-p", "--port"])
        .context("invalid value for '--port' (expected an integer in 0..=65535)")?;
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

fn print_help() {
    let version = env!("CARGO_PKG_VERSION");
    println!("PortLens {version}");
    println!("List open network ports and their associated processes.");
    println!();
    println!("Usage: portlens [OPTIONS] [COMMAND]");
    println!();
    println!("Commands:");
    println!("  update  Check for updates and optionally self-update the binary");
    println!();
    println!("Options:");
    println!("  -t, --tcp            Show only TCP sockets");
    println!("  -u, --udp            Show only UDP sockets");
    println!("  -l, --listen         Show only sockets in LISTEN state (TCP only)");
    println!("  -p, --port <PORT>    Filter results to a specific port number");
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
}

fn print_version() {
    println!("PortLens {}", env!("CARGO_PKG_VERSION"));
}

/// Application entry point, separated from `main()` for testability.
fn run(cli: Cli) -> Result<()> {
    // Dispatch to subcommand if present
    if let Some(command) = cli.command {
        return match command {
            Command::Update { check } => portlens::update::run(check),
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

    Ok(())
}
