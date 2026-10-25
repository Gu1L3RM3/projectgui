mod cli;
mod config;
mod domain;
#[cfg(feature = "gui")]
mod gui;
#[cfg(feature = "mcp")]
mod mcp;
mod repo;
mod service;
#[cfg(feature = "update")]
mod update;

fn main() {
    std::process::exit(cli::run());
}
