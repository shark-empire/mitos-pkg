//! Everything for running, and connecting to, `mitos-pkgd` — the
//! background service that lets a non-terminal client (a GUI, chiefly)
//! drive `mitos-pkg` without linking `PackageService` and running
//! privileged itself. See `docs/INTEGRATION.md` for the full picture of
//! how this fits alongside the CLI and the library as a third way to
//! connect to `mitos-pkg`.
//!
//! - [`protocol`] — the NDJSON wire format both sides speak.
//! - [`server`] — `mitos-pkgd`'s implementation (`run`), used by
//!   `src/bin/mitos-pkgd.rs`.
//! - [`client`] — [`client::DaemonClient`], for any Rust component that
//!   wants to talk to a running `mitos-pkgd`.

pub mod client;
pub mod protocol;
pub mod server;

pub use client::{ClientError, DaemonClient};
pub use protocol::{Message, Request, ResponsePayload};
