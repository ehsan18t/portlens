//! # portview - entry point
//!
//! Parses CLI arguments, collects socket data, applies filters, and renders
//! output to stdout.

use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use portview::{collector, display, filter};

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
}

fn main() -> ExitCode {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        return ExitCode::from(EXIT_RUNTIME_ERROR);
    }
    ExitCode::SUCCESS
}

/// Application entry point, separated from `main()` for testability.
fn run() -> Result<()> {
    let cli = Cli::parse();

    let entries = collector::collect()?;
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
        print_tips();
    }

    Ok(())
}

/// Print a one-line version and tips footer when running interactively.
///
/// Only prints when stdout is a terminal so piped output stays clean.
fn print_tips() {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let version = env!("CARGO_PKG_VERSION");
    eprintln!(
        "portview v{version} - Tips: -p PORT (filter) | -a (all) | --full (details) | --json | -h (help)"
    );
}
