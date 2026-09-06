use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "mitos-pkg")]
#[command(about = "MITOS Package Manager", version)]
pub struct Cli {
    /// Override the config file location (default: /etc/mitos-pkg/config.json)
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Install/remove relative to this root instead of "/" (chroot-style;
    /// mainly for testing without needing real root on the host)
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Install a package and its unmet dependencies. Accepts `name` or
    /// `name@version` to pin an exact version.
    Install { package_name: String },
    /// Remove an installed package (fails if other packages depend on it,
    /// unless --cascade is given)
    Remove {
        package_name: String,
        /// Also remove every installed package that depends on this one
        #[arg(long)]
        cascade: bool,
    },
    /// Upgrade one package, or every installed package if none is named
    Upgrade {
        package_name: Option<String>,
        /// Upgrade even a package pinned with `hold`
        #[arg(long)]
        ignore_hold: bool,
    },
    /// Pin an installed package so `upgrade` skips it until unheld
    Hold { package_name: String },
    /// Reverse `hold`, letting `upgrade` touch the package again
    Unhold { package_name: String },
    /// Remove installed packages nothing depends on that weren't installed explicitly
    Autoremove,
    /// List all installed packages
    List,
    /// Search available packages in the local repository index
    Search { query: String },
    /// Show details for a package (installed, or available in the index)
    Info { package_name: String },
    /// Refresh the local repository index from configured repositories
    Update,
    /// Delete cached downloaded archives (they're re-fetched and
    /// re-verified on demand if needed again)
    Clean,
    /// Package a source directory (pkg.json + payload/) into a .mpkg archive
    Build {
        /// Directory containing pkg.json and a payload/ directory
        source: PathBuf,
        /// Directory to write the resulting .mpkg into (default: current directory)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Path to a file holding a hex-encoded 32-byte Ed25519 seed to sign with
        #[arg(long)]
        sign_with: Option<PathBuf>,
    },
}
