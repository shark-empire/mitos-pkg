//! A small Rust client for `mitos-pkgd`'s Unix-socket protocol (see
//! `daemon::protocol` and `docs/INTEGRATION.md`). This is the piece meant
//! for `mitos-gui`, `mitos-settings`, or a future software-center panel:
//! link this crate, connect to the daemon's socket, and drive installs/
//! removals/upgrades as the logged-in user, without that GUI process
//! itself needing to run privileged — the daemon (started by
//! `mitos-services`, typically as root) does the actual filesystem
//! writes on the client's behalf.
//!
//! ```no_run
//! use mitos_pkg::daemon::client::DaemonClient;
//! use std::path::Path;
//!
//! # fn main() -> std::io::Result<()> {
//! let mut client = DaemonClient::connect(Path::new("/run/mitos-pkg/pkgd.sock"))?;
//! client.install("mitos-shell", |event| println!("{event:?}"))
//!     .expect("install failed");
//! # Ok(())
//! # }
//! ```

use crate::daemon::protocol::{read_json_line, write_json_line, Message, Request, ResponsePayload};
use crate::database::history::HistoryEntry;
use crate::database::packages::InstalledPackage;
use crate::repository::metadata::PackageMetadata;
use crate::service::{InfoView, ProgressEvent, VerifyIssue};
use semver::Version;
use std::io::{self, BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Everything a `DaemonClient` call can fail with: either the connection
/// itself broke, or `mitos-pkgd` ran the request and it failed.
/// `Daemon(String)` carries the same text `PkgError::to_string()` would
/// on the daemon side — see `daemon::protocol` for why the wire carries
/// formatted text rather than a typed `PkgError`. A caller that needs to
/// match on *which* `PkgError` variant happened should link the library
/// and call `PackageService` directly instead of going through the
/// daemon.
#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Daemon(String),
    /// The daemon replied with a payload shape that doesn't match what
    /// this request was supposed to return — a protocol mismatch (most
    /// likely a client built against a different `mitos-pkgd` version)
    /// rather than an operation failure.
    UnexpectedResponse,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "connection to mitos-pkgd failed: {e}"),
            ClientError::Daemon(msg) => write!(f, "{msg}"),
            ClientError::UnexpectedResponse => {
                write!(f, "mitos-pkgd sent an unexpected response for this request")
            }
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        ClientError::Io(e)
    }
}

type ClientResult<T> = Result<T, ClientError>;

/// One connection to `mitos-pkgd`. Cheap to hold open for a GUI's whole
/// lifetime and reuse across calls; also cheap to open fresh per call if
/// that's simpler for the caller — nothing here is stateful beyond the
/// socket itself.
pub struct DaemonClient {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

impl DaemonClient {
    pub fn connect(socket_path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(socket_path)?;
        let reader = BufReader::new(stream.try_clone()?);
        let writer = BufWriter::new(stream);
        Ok(Self { reader, writer })
    }

    /// Sends `request`, forwarding every `Message::Progress` the daemon
    /// streams back to `on_progress` as it arrives, then returns the
    /// terminal `Message::Ok`'s payload (or the daemon's reported
    /// error). The connection stays open afterward for further calls.
    fn call(
        &mut self,
        request: Request,
        mut on_progress: impl FnMut(ProgressEvent),
    ) -> ClientResult<ResponsePayload> {
        write_json_line(&mut self.writer, &request)?;
        loop {
            let message: Message = read_json_line(&mut self.reader)?.ok_or_else(|| {
                ClientError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "mitos-pkgd closed the connection before replying",
                ))
            })?;
            match message {
                Message::Progress(event) => on_progress(event),
                Message::Ok(payload) => return Ok(payload),
                Message::Err { message } => return Err(ClientError::Daemon(message)),
            }
        }
    }

    /// Convenience wrapper for requests that never stream progress
    /// (`hold`/`unhold`/`list`/`search`/`info`/`verify`/`history`/`clean`/
    /// `ping`, and any dry-run call — a dry run never touches the
    /// filesystem, so there's nothing for a progress phase to report).
    fn call_quiet(&mut self, request: Request) -> ClientResult<ResponsePayload> {
        self.call(request, |_| {})
    }

    /// Liveness/version check — a client can use this to detect "daemon
    /// not running" up front and fall back to linking the library or
    /// shelling out to the CLI instead (see `docs/INTEGRATION.md`).
    pub fn ping(&mut self) -> ClientResult<()> {
        match self.call_quiet(Request::Ping)? {
            ResponsePayload::Pong => Ok(()),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn install(
        &mut self,
        spec: &str,
        on_progress: impl FnMut(ProgressEvent),
    ) -> ClientResult<Vec<(String, Version)>> {
        match self.call(
            Request::Install {
                spec: spec.to_string(),
                dry_run: false,
            },
            on_progress,
        )? {
            ResponsePayload::Installed { packages, .. } => Ok(packages),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Resolves `spec` and reports what `install` would do, without
    /// installing anything.
    pub fn install_dry_run(&mut self, spec: &str) -> ClientResult<Vec<(String, Version)>> {
        match self.call_quiet(Request::Install {
            spec: spec.to_string(),
            dry_run: true,
        })? {
            ResponsePayload::Installed { packages, .. } => Ok(packages),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Installs an already-built `.mpkg` the daemon can read at `path` on
    /// its own filesystem (see `daemon::protocol::Request::InstallLocal`).
    /// `signature` is the same hex string `mitos-pkg install --signature`
    /// takes on the CLI, needed only if the package declares a `signer`.
    pub fn install_local(
        &mut self,
        path: &Path,
        signature: Option<&str>,
        on_progress: impl FnMut(ProgressEvent),
    ) -> ClientResult<Vec<(String, Version)>> {
        match self.call(
            Request::InstallLocal {
                path: path.to_path_buf(),
                signature: signature.map(str::to_string),
                dry_run: false,
            },
            on_progress,
        )? {
            ResponsePayload::Installed { packages, .. } => Ok(packages),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn remove(
        &mut self,
        name: &str,
        cascade: bool,
        force: bool,
        on_progress: impl FnMut(ProgressEvent),
    ) -> ClientResult<Vec<String>> {
        match self.call(
            Request::Remove {
                name: name.to_string(),
                cascade,
                force,
                dry_run: false,
            },
            on_progress,
        )? {
            ResponsePayload::Removed { names, .. } => Ok(names),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Reports what `remove` would do — including refusing the same way a
    /// real call would (dependents in the way, or an essential package
    /// without `force`) — without removing anything.
    pub fn remove_dry_run(&mut self, name: &str, cascade: bool, force: bool) -> ClientResult<Vec<String>> {
        match self.call_quiet(Request::Remove {
            name: name.to_string(),
            cascade,
            force,
            dry_run: true,
        })? {
            ResponsePayload::Removed { names, .. } => Ok(names),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn upgrade(
        &mut self,
        name: Option<&str>,
        ignore_hold: bool,
        on_progress: impl FnMut(ProgressEvent),
    ) -> ClientResult<Vec<(String, Version, Version)>> {
        match self.call(
            Request::Upgrade {
                name: name.map(str::to_string),
                ignore_hold,
                dry_run: false,
            },
            on_progress,
        )? {
            ResponsePayload::Upgraded { changes, .. } => Ok(changes),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Reports what `upgrade` would do without upgrading anything.
    pub fn upgrade_dry_run(
        &mut self,
        name: Option<&str>,
        ignore_hold: bool,
    ) -> ClientResult<Vec<(String, Version, Version)>> {
        match self.call_quiet(Request::Upgrade {
            name: name.map(str::to_string),
            ignore_hold,
            dry_run: true,
        })? {
            ResponsePayload::Upgraded { changes, .. } => Ok(changes),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn hold(&mut self, name: &str) -> ClientResult<()> {
        match self.call_quiet(Request::Hold {
            name: name.to_string(),
        })? {
            ResponsePayload::Unit => Ok(()),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn unhold(&mut self, name: &str) -> ClientResult<()> {
        match self.call_quiet(Request::Unhold {
            name: name.to_string(),
        })? {
            ResponsePayload::Unit => Ok(()),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn autoremove(
        &mut self,
        on_progress: impl FnMut(ProgressEvent),
    ) -> ClientResult<Vec<String>> {
        match self.call(Request::Autoremove, on_progress)? {
            ResponsePayload::Removed { names, .. } => Ok(names),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn update(&mut self, on_progress: impl FnMut(ProgressEvent)) -> ClientResult<()> {
        match self.call(Request::Update, on_progress)? {
            ResponsePayload::Unit => Ok(()),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn clean(&mut self) -> ClientResult<u64> {
        match self.call_quiet(Request::Clean)? {
            ResponsePayload::Freed { bytes } => Ok(bytes),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// A GUI software panel's bread and butter: `list()` for what's
    /// installed, `search()` for what's available. Both safe to call
    /// often (e.g. on every panel refresh) — they take only a read lock
    /// on the daemon side, alongside any other concurrent read.
    pub fn list(&mut self) -> ClientResult<Vec<(String, InstalledPackage)>> {
        match self.call_quiet(Request::List)? {
            ResponsePayload::Listed { packages } => Ok(packages),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn search(&mut self, query: &str) -> ClientResult<Vec<PackageMetadata>> {
        match self.call_quiet(Request::Search {
            query: query.to_string(),
        })? {
            ResponsePayload::Searched { matches } => Ok(matches),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    pub fn info(&mut self, name: &str) -> ClientResult<InfoView> {
        match self.call_quiet(Request::Info {
            name: name.to_string(),
        })? {
            ResponsePayload::Info(info) => Ok(info),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Re-checks installed files against what was recorded at install
    /// time. `name: None` checks every installed package.
    pub fn verify(&mut self, name: Option<&str>) -> ClientResult<Vec<VerifyIssue>> {
        match self.call_quiet(Request::Verify {
            name: name.map(str::to_string),
        })? {
            ResponsePayload::Verified { issues } => Ok(issues),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// The most recent `limit` completed operations, oldest first.
    pub fn history(&mut self, limit: usize) -> ClientResult<Vec<HistoryEntry>> {
        match self.call_quiet(Request::History { limit })? {
            ResponsePayload::History { entries } => Ok(entries),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }
}
