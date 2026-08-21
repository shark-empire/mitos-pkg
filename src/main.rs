use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mitos-pkg")]
#[command(about = "MITOS Package Manager - Handles software installation", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install a new package
    Install {
        /// The name of the package to install
        package_name: String,
    },
    /// Remove an installed package
    Remove {
        /// The name of the package to remove
        package_name: String,
    },
    /// List all installed packages
    List,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Install { package_name } => {
            println!("mitos-pkg: Resolving dependencies for '{}'...", package_name);
            // TODO: Download package tarball, verify cryptographic signature, extract to /
            println!("mitos-pkg: Successfully installed {}.", package_name);
        }
        Commands::Remove { package_name } => {
            println!("mitos-pkg: Locating installed files for '{}'...", package_name);
            // TODO: Read package manifest from /var/lib/mitos-pkg/, delete files, update DB
            println!("mitos-pkg: Successfully removed {}.", package_name);
        }
        Commands::List => {
            println!("mitos-pkg: Installed packages:");
            // TODO: Parse the local database and print installed packages
            println!("  - mitos-init (v0.1.0)");
            println!("  - mitos-shell (v0.1.0)");
            println!("  - mitos-utils (v0.1.0)");
        }
    }
}
