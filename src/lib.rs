//! Library surface for `mitos-pkg`.
//!
//! The `mitos-pkg` binary (`main.rs`) is a thin CLI wrapper around
//! [`service::PackageService`]. Other MITOS components written in Rust —
//! `mitos-installer`, `mitos-gui`, `mitos-settings`, `mitos-recovery` — can
//! depend on this crate directly and call `PackageService` the same way
//! `main.rs` does, instead of shelling out to the `mitos-pkg` binary and
//! parsing its human-readable text output. See `docs/INTEGRATION.md` for
//! the full surface, the on-disk JSON schemas it reads and writes, and
//! guidance on when to link this crate directly versus shell out to the
//! CLI.
//!
//! ```no_run
//! use mitos_pkg::config::Config;
//! use mitos_pkg::service::PackageService;
//!
//! # fn main() -> mitos_pkg::error::Result<()> {
//! let config = Config::load_or_default(std::path::Path::new(Config::DEFAULT_PATH))?;
//! let mut service = PackageService::open(config)?;
//! service.update()?;
//! service.install("mitos-shell", None, false)?;
//! # Ok(())
//! # }
//! ```
//!
//! A third option alongside "shell out to the CLI" and "link the
//! library": talk to `mitos-pkgd`, the background service in
//! [`daemon`], over a Unix socket via [`daemon::client::DaemonClient`].
//! This is what a GUI wants — installs/removals/upgrades happen in a
//! privileged daemon process instead of the GUI's own (unprivileged, as
//! it should be) process, and progress streams back live instead of the
//! call just blocking until it's done. See [`daemon`]'s module docs and
//! `docs/INTEGRATION.md`.

pub mod cli;
pub mod config;
pub mod daemon;
pub mod database;
pub mod dependency;
pub mod error;
pub mod install;
pub mod package;
pub mod repository;
pub mod security;
pub mod service;

pub use config::Config;
pub use error::{PkgError, Result};
pub use service::{InfoView, PackageService};
