//! `mitos-pkgd` — the background service half of `mitos-pkg`. A thin
//! wrapper the same way `main.rs` is: real work happens in
//! `mitos_pkg::daemon::server`, this just parses args, opens a
//! `PackageService`, and hands it to `server::run`.
//!
//! Typically started once at boot by `mitos-services` (as root, so it
//! can actually write to `/usr`, `/var`, etc.), not run interactively —
//! see `docs/INTEGRATION.md` for how a client (a GUI, or `mitos-pkg`
//! itself) is meant to reach it.

use clap::Parser;
use mitos_pkg::config::Config;
use mitos_pkg::daemon::server;
use mitos_pkg::service::PackageService;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "mitos-pkgd")]
#[command(
    about = "Background service for mitos-pkg — lets unprivileged clients (mitos-gui, mitos-settings, mitos-pkg itself) install/remove/search/query packages over a local socket instead of needing a terminal and root access directly"
)]
struct Args {
    /// Override the config file location (default: /etc/mitos-pkg/config.json)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Override the socket path (default: from config, normally /run/mitos-pkg/pkgd.sock)
    #[arg(long)]
    socket: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let config_path = args
        .config
        .unwrap_or_else(|| PathBuf::from(Config::DEFAULT_PATH));
    let config = match Config::load_or_default(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mitos-pkgd: failed to load config: {e}");
            return ExitCode::FAILURE;
        }
    };

    let socket_path = args.socket.unwrap_or_else(|| config.daemon_socket.clone());

    let service = match PackageService::open(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mitos-pkgd: failed to initialize: {e}");
            return ExitCode::FAILURE;
        }
    };

    eprintln!("mitos-pkgd: listening on {}", socket_path.display());
    match server::run(service, socket_path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mitos-pkgd: {e}");
            ExitCode::FAILURE
        }
    }
}
