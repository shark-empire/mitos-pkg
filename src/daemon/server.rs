//! The `mitos-pkgd` background service: owns one `PackageService` behind
//! an `RwLock` and serves it to any number of local clients (a GUI, the
//! CLI, another MITOS component) over a Unix domain socket, so
//! installing, removing, or upgrading a package no longer requires a
//! terminal — or the requesting process itself running as root — see
//! `docs/INTEGRATION.md` for the wire protocol and `daemon::client` for
//! the Rust side of it.
//!
//! Deliberately thread-per-connection over `std::os::unix::net`, not an
//! async runtime: connections are rare (a handful of GUI panels, an
//! occasional CLI invocation) and every mutating operation already
//! serializes through one `RwLock` write lock, so the extra concurrency
//! an async I/O driver buys costs resident memory without buying
//! anything — the same tradeoff `Cargo.toml` documents for `ureq` over
//! `reqwest`/`tokio`.

use crate::daemon::protocol::{read_json_line, write_json_line, Message, Request, ResponsePayload};
use crate::error::PkgError;
use crate::service::{PackageService, Progress, ProgressEvent};
use std::fs;
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::thread;

/// Binds `socket_path`, clearing away a stale socket left behind by a
/// previous unclean shutdown (nothing answers on it anymore) but
/// refusing to touch one that's actually live — two `mitos-pkgd`
/// instances sharing one `PackageService`'s on-disk state without
/// coordinating would corrupt it far worse than just failing to start.
fn bind_socket(socket_path: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }

    if socket_path.exists() {
        match UnixStream::connect(socket_path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "mitos-pkgd is already listening on {}",
                        socket_path.display()
                    ),
                ));
            }
            Err(_) => {
                // Nothing answered: a leftover socket file from a crash
                // or an unclean shutdown, not a live daemon.
                let _ = fs::remove_file(socket_path);
            }
        }
    }

    let listener = UnixListener::bind(socket_path)?;
    // 0o660: the owning user (root, normally) and group can connect;
    // everyone else is refused at the filesystem level before a single
    // byte of protocol is parsed. Which unprivileged desktop-session
    // users are *in* that group — i.e. who's actually allowed to drive
    // installs through this daemon — is a policy decision left to
    // whatever starts it (mitos-services already owns runtime-directory
    // ownership and privilege dropping for the rest of MITOS; duplicating
    // that here would just give this daemon its own, different answer).
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

/// Runs the daemon until the process is killed: binds `socket_path` and
/// serves `service` to every connection, one OS thread per connection.
/// Doesn't return on success — this is meant to be a service's whole
/// `main` (see `src/bin/mitos-pkgd.rs`).
pub fn run(service: PackageService, socket_path: PathBuf) -> std::io::Result<()> {
    let listener = bind_socket(&socket_path)?;
    let service = Arc::new(RwLock::new(service));

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mitos-pkgd: failed to accept connection: {e}");
                continue;
            }
        };
        let service = Arc::clone(&service);
        thread::spawn(move || handle_connection(stream, service));
    }

    Ok(())
}

/// Serves every request on one connection in turn — a client may reuse a
/// connection for several requests — until it disconnects or sends
/// something that doesn't parse as a `Request`.
fn handle_connection(stream: UnixStream, service: Arc<RwLock<PackageService>>) {
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mitos-pkgd: failed to clone connection: {e}");
            return;
        }
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = BufWriter::new(stream);

    loop {
        let request: Request = match read_json_line(&mut reader) {
            Ok(Some(req)) => req,
            Ok(None) => return, // client disconnected
            Err(e) => {
                let _ = write_json_line(
                    &mut writer,
                    &Message::Err {
                        message: format!("malformed request: {e}"),
                    },
                );
                return;
            }
        };

        dispatch(request, &service, &mut writer);
    }
}

/// Runs one request to completion and writes its terminal message.
/// Read-only requests (`List`, `Search`, `Info`, `Clean`) take a read
/// lock, so several can proceed at once even while nothing is
/// installing; every mutating request takes the write lock for the
/// whole operation, streaming `Message::Progress` events to this
/// connection as `PackageService` reports them.
fn dispatch(
    request: Request,
    service: &Arc<RwLock<PackageService>>,
    writer: &mut impl Write,
) {
    // Scoped so the closure's mutable borrow of `writer` provably ends
    // before `writer` is used again below for the terminal message —
    // `outcome` owns no reference back into `writer` once this block
    // returns.
    let outcome: Result<ResponsePayload, PkgError> = {
        let mut emit_progress = |event: ProgressEvent| {
            let _ = write_json_line(writer, &Message::Progress(event));
        };

        match request {
            Request::Ping => Ok(ResponsePayload::Pong),

            Request::Install { spec } => {
                let mut svc = service.write().expect("package service lock poisoned");
                let mut progress = Progress::new(&mut emit_progress);
                svc.install_with_progress(&spec, &mut progress)
                    .map(|()| ResponsePayload::Unit)
            }
            Request::Remove { name, cascade } => {
                let mut svc = service.write().expect("package service lock poisoned");
                let mut progress = Progress::new(&mut emit_progress);
                svc.remove_with_progress(&name, cascade, &mut progress)
                    .map(|names| ResponsePayload::Removed { names })
            }
            Request::Upgrade { name, ignore_hold } => {
                let mut svc = service.write().expect("package service lock poisoned");
                let mut progress = Progress::new(&mut emit_progress);
                svc.upgrade_with_progress(name.as_deref(), ignore_hold, &mut progress)
                    .map(|changes| ResponsePayload::Upgraded { changes })
            }
            Request::Hold { name } => {
                let mut svc = service.write().expect("package service lock poisoned");
                svc.hold(&name).map(|()| ResponsePayload::Unit)
            }
            Request::Unhold { name } => {
                let mut svc = service.write().expect("package service lock poisoned");
                svc.unhold(&name).map(|()| ResponsePayload::Unit)
            }
            Request::Autoremove => {
                let mut svc = service.write().expect("package service lock poisoned");
                let mut progress = Progress::new(&mut emit_progress);
                svc.autoremove_with_progress(&mut progress)
                    .map(|names| ResponsePayload::Removed { names })
            }
            Request::Update => {
                let mut svc = service.write().expect("package service lock poisoned");
                let mut progress = Progress::new(&mut emit_progress);
                svc.update_with_progress(&mut progress)
                    .map(|()| ResponsePayload::Unit)
            }
            Request::Clean => {
                let svc = service.read().expect("package service lock poisoned");
                svc.clean().map(|bytes| ResponsePayload::Freed { bytes })
            }
            Request::List => {
                let svc = service.read().expect("package service lock poisoned");
                let packages = svc
                    .list()
                    .into_iter()
                    .map(|(name, pkg)| (name.clone(), pkg.clone()))
                    .collect();
                Ok(ResponsePayload::Listed { packages })
            }
            Request::Search { query } => {
                let svc = service.read().expect("package service lock poisoned");
                let matches = svc.search(&query).into_iter().cloned().collect();
                Ok(ResponsePayload::Searched { matches })
            }
            Request::Info { name } => {
                let svc = service.read().expect("package service lock poisoned");
                svc.info(&name).map(ResponsePayload::Info)
            }
        }
    };

    let final_message = match outcome {
        Ok(payload) => Message::Ok(payload),
        Err(e) => Message::Err {
            message: e.to_string(),
        },
    };
    let _ = write_json_line(writer, &final_message);
}
