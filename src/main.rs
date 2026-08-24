mod cli;
mod config;
mod database;
mod dependency;
mod error;
mod install;
mod package;
mod repository;
mod security;
mod service;

use clap::Parser;
use cli::{Cli, Commands};
use config::Config;
use service::PackageService;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();

    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(Config::DEFAULT_PATH));
    let mut config = match Config::load_or_default(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mitos-pkg: failed to load config: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(root) = cli.root {
        config.install_root = root;
    }

    let mut service = match PackageService::open(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mitos-pkg: failed to initialize: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = match &cli.command {
        Commands::Install { package_name } => service.install(package_name),
        Commands::Remove { package_name } => service.remove(package_name),
        Commands::List => {
            for (name, pkg) in service.list() {
                println!("{name} {}", pkg.version);
            }
            Ok(())
        }
        Commands::Search { query } => {
            for meta in service.search(query) {
                println!("{} {} - {}", meta.name, meta.version, meta.description);
            }
            Ok(())
        }
        Commands::Update => service.update(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mitos-pkg: {e}");
            ExitCode::FAILURE
        }
    }
}
