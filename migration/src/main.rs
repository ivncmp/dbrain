mod cli;
mod config;
mod dashboard;
mod db;
mod mcp;
mod memory;
mod server;
mod utils;

/// Single source of truth for the crate version.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() {
  if let Err(error) = cli::run().await {
    eprintln!("{error}");
    std::process::exit(1);
  }
}
