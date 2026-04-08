//! # portview - entry point
//!
//! Parses CLI arguments, collects socket data, applies filters, and renders
//! output to stdout.

use anyhow::Result;
use clap::Parser;
use portview::{collector, display, filter};

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

fn main() -> Result<()> {
    run()
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
        },
    );

    if cli.json {
        display::print_json(&filtered)?;
    } else {
        display::print_table(&filtered, !cli.no_header)?;
    }

    Ok(())
}
