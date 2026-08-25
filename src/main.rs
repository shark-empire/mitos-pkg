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
use package::build;
use service::PackageService;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();

    // `build` is a pure source -> archive transform: it never touches the
    // install database, the trusted-keys store, or the repository index,
    // so it's handled entirely separately from everything else below,
    // before any of that state is even opened.
    if let Commands::Build {
        source,
        output,
        sign_with,
    } = &cli.command
    {
        return run_build(source, output.as_deref(), sign_with.as_deref());
    }

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
        Commands::Upgrade { package_name } => {
            service.upgrade(package_name.as_deref()).map(|upgraded| {
                if upgraded.is_empty() {
                    println!("mitos-pkg: nothing to upgrade");
                }
                for (name, from, to) in upgraded {
                    println!("{name}: {from} -> {to}");
                }
            })
        }
        Commands::Autoremove => service.autoremove().map(|removed| {
            if removed.is_empty() {
                println!("mitos-pkg: nothing to remove");
            }
            for name in removed {
                println!("removed: {name}");
            }
        }),
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
        Commands::Info { package_name } => service.info(package_name).map(|info| {
            println!("name: {}", info.name);
            println!("version: {}", info.version);
            if !info.description.is_empty() {
                println!("description: {}", info.description);
            }
            println!("installed: {}", if info.installed { "yes" } else { "no" });
            if let Some(explicit) = info.explicit {
                println!(
                    "explicitly installed: {}",
                    if explicit { "yes" } else { "no" }
                );
            }
            if !info.dependencies.is_empty() {
                let deps: Vec<String> = info
                    .dependencies
                    .iter()
                    .map(|d| format!("{} {}", d.name, d.version_req))
                    .collect();
                println!("dependencies: {}", deps.join(", "));
            }
            if !info.provides.is_empty() {
                println!("provides: {}", info.provides.join(", "));
            }
            if !info.conflicts.is_empty() {
                println!("conflicts: {}", info.conflicts.join(", "));
            }
        }),
        Commands::Update => service.update(),
        Commands::Build { .. } => unreachable!("handled before service setup above"),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mitos-pkg: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_build(source: &Path, output: Option<&Path>, sign_with: Option<&Path>) -> ExitCode {
    let output_dir = output.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));

    let sign_seed = match sign_with {
        Some(path) => match load_seed(path) {
            Ok(seed) => Some(seed),
            Err(e) => {
                eprintln!("mitos-pkg: failed to read signing key: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    match build::build_package(source, &output_dir, sign_seed.as_ref()) {
        Ok(out) => {
            println!(
                "mitos-pkg: built {} (payload sha256: {})",
                out.archive_path.display(),
                out.manifest.payload_sha256
            );
            if let Some(sig) = out.signature_hex {
                println!("mitos-pkg: signature (publish this in your repo index): {sig}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("mitos-pkg: build failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Reads a hex-encoded 32-byte Ed25519 seed from a file. There's no
/// `mitos-pkg keygen`: signing is deterministic and needs no RNG, so any
/// 32 random bytes work — e.g. `openssl rand -hex 32 > seed.hex`.
fn load_seed(path: &Path) -> std::result::Result<[u8; 32], String> {
    let hex_str = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let bytes = hex::decode(hex_str.trim()).map_err(|e| e.to_string())?;
    bytes
        .try_into()
        .map_err(|_| "seed must be exactly 32 bytes".to_string())
}
