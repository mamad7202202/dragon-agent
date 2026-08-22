mod agent;
mod cli;
mod config;
mod memory;
mod presets;
mod provider;
mod session;
mod theme;
mod tui;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Cli::parse();
    match args.command {
        Some(cmd) => cli::dispatch(cmd, args.model).await,
        None => tui::run(args.model).await,
    }
}
