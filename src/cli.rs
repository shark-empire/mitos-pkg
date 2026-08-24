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
    /// Install a package and its unmet dependencies
    Install { package_name: String },
    /// Remove an installed package (fails if other packages depend on it)
    Remove { package_name: String },
    /// List all installed packages
    List,
    /// Search available packages in the local repository index
    Search { query: String },
    /// Refresh the local repository index from configured repositories
    Update,
}
