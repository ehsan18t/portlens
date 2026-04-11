//! # portview - entry point
//!
//! Parses CLI arguments, collects socket data, applies filters, and renders
//! output to stdout.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use portview::{collector, display, filter};
use tracing_subscriber::EnvFilter;

/// Exit code for runtime errors (failed to enumerate sockets, write errors).
/// Usage errors (invalid flags) are handled by clap with exit code 2.
const EXIT_RUNTIME_ERROR: u8 = 1;

/// portview - list open network ports and their associated processes.
// CLI structs inherently use multiple boolean flags for argument toggling.
#[allow(clippy::struct_excessive_bools)]
#[derive(Parser, Debug)]
#[command(name = "portview", version, about, long_about = None)]
struct Cli {
    /// Show only TCP sockets.
    #[arg(short = 't', long = "tcp", conflicts_with = "udp")]
    tcp: bool,

    /// Show only UDP sockets.
    #[arg(short = 'u', long = "udp", conflicts_with = "tcp")]
    udp: bool,

    /// Show only sockets in LISTEN state (TCP only).
    #[arg(short = 'l', long = "listen", conflicts_with = "udp")]
    listen: bool,

    /// Filter results to a specific port number and bypass smart relevance filtering.
    #[arg(short = 'p', long = "port")]
    port: Option<u16>,

    /// Show all ports (disable developer-relevant filter).
    #[arg(short = 'a', long = "all")]
    all: bool,

    /// Show all columns (adds STATE, USER).
    #[arg(short = 'f', long = "full")]
    full: bool,

    /// Use compact borderless table style.
    #[arg(short = 'c', long = "compact")]
    compact: bool,

    /// Suppress the column header row.
    #[arg(long = "no-header")]
    no_header: bool,

    /// Output results as a JSON array.
    #[arg(long = "json")]
    json: bool,

    /// Disable Docker/Podman and project-root enrichment. Combine with --all for the rawest view.
    #[arg(long = "no-enrich")]
    no_enrich: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Check for updates and optionally self-update the binary.
    Update {
        /// Only check for a new version without downloading or installing.
        #[arg(long = "check")]
        check: bool,
    },
}

fn main() -> ExitCode {
    if let Err(e) = init_tracing().and_then(|()| run()) {
        eprintln!("error: {e:#}");
        return ExitCode::from(EXIT_RUNTIME_ERROR);
    }
    ExitCode::SUCCESS
}

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("off"))
        .context("failed to build tracing filter")?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize tracing subscriber: {error}"))?;

    Ok(())
}

/// Normalize CLI arguments to lowercase for case-insensitive matching.
///
/// Skips argv\[0\] (the program name/path). All other arguments are
/// lowercased. This is safe because portview has no string-valued
/// arguments — only numeric port values, flags, and subcommand names,
/// none of which are affected by lowercasing.
fn normalize_args() -> Vec<OsString> {
    std::env::args_os()
        .enumerate()
        .map(|(i, arg)| {
            if i == 0 {
                return arg;
            }
            // Convert to UTF-8 for lowercasing; fall back to original if
            // the argument contains non-UTF-8 bytes (unlikely on any
            // supported platform).
            arg.into_string().map_or_else(
                |original| original,
                |s| OsString::from(s.to_ascii_lowercase()),
            )
        })
        .collect()
}

/// Application entry point, separated from `main()` for testability.
fn run() -> Result<()> {
    let cli = Cli::parse_from(normalize_args());

    // Dispatch to subcommand if present
    if let Some(command) = &cli.command {
        return match command {
            Command::Update { check } => portview::update::run(*check),
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
