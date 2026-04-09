//! # portview - entry point
//!
//! Parses CLI arguments, collects socket data, applies filters, and renders
//! output to stdout.

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
    #[arg(short = 'l', long = "listen")]
    listen: bool,

    /// Filter results to a specific port number.
    #[arg(short = 'p', long = "port")]
    port: Option<u16>,

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
        &entries,
        &filter::FilterOptions {
            tcp_only: cli.tcp,
            udp_only: cli.udp,
            listen_only: cli.listen,
            port: cli.port,
            show_all: true, // TODO: wire to --all flag in Task 9
        },
    );

    if cli.json {
        display::print_json(&filtered)?;
    } else {
        display::print_table(
            &filtered,
            &display::DisplayOptions {
                show_header: !cli.no_header,
                full: false,    // TODO: wire to --full flag in Task 9
                compact: false, // TODO: wire to --compact flag in Task 9
            },
        )?;
    }

    Ok(())
}
