//! The `nerevar-cli` binary: parse, run, exit. Everything else is the library
//! (`lib.rs`), so the integration tests drive the same code this does.

use clap::Parser;

use nerevar_cli::cli::Cli;

#[tokio::main]
async fn main() {
    nerevar_cli::install_logger();
    let cli = Cli::parse();
    std::process::exit(nerevar_cli::run(cli).await);
}
