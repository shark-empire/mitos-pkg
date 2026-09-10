//! The wire protocol `mitos-pkgd` (server.rs) and `DaemonClient`
//! (client.rs) speak to each other over a Unix domain socket: one JSON
//! object per line (NDJSON), chosen over a length-prefixed binary framing
//! because every message here is small and `serde_json` is already a
//! dependency — no new framing crate earns its keep for this.
//!
//! One request, one or more replies: a client sends one [`Request`] line,
//! the daemon may send zero or more [`Message::Progress`] lines while the
//! operation runs, then exactly one terminal [`Message::Ok`] or
//! [`Message::Err`] line. A connection can be reused for further
//! requests afterward — nothing here is one-shot-per-connection.
//!
//! Errors cross the wire as plain text (`Message::Err { message }`, `PkgError::to_string()`)
//! rather than a serialized `PkgError`. `PkgError` isn't `Serialize` and
//! deliberately stays that way: its variant set is allowed to grow or
//! reshape freely (see `error.rs`) without that being a protocol-breaking
//! change for whatever's on the other end of the socket. A client that
//! needs to branch on *which* error happened, not just report it, should
//! link `mitos-pkg` as a library and call `PackageService` directly
//! instead of going through the daemon — see `docs/INTEGRATION.md`.

use crate::database::packages::InstalledPackage;
use crate::repository::metadata::PackageMetadata;
use crate::service::{InfoView, ProgressEvent};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};

/// Every operation a client can ask `mitos-pkgd` to perform. Mirrors
/// `service::PackageService`'s public surface (see `docs/INTEGRATION.md`
/// for the equivalent library/CLI calls) minus `build`, which is a pure
/// local source -> archive transform with no install-database or network
/// involvement — there's nothing a persistent daemon buys it over just
/// running `mitos-pkg build` directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Cheap liveness/version check — a client can use this to detect
    /// "daemon not running" and fall back to linking the library or
    /// shelling out to the CLI instead.
    Ping,
    Install { spec: String },
    Remove { name: String, cascade: bool },
    Upgrade {
        name: Option<String>,
        ignore_hold: bool,
    },
    Hold { name: String },
    Unhold { name: String },
    Autoremove,
    List,
    Search { query: String },
    Info { name: String },
    Update,
    Clean,
}

/// One line the daemon sends back for a request: any number of
/// `Progress` events while the operation is in flight, followed by
/// exactly one of `Ok`/`Err` to close it out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Progress(ProgressEvent),
    Ok(ResponsePayload),
    Err { message: String },
}

/// The successful result of a `Request`, shaped after whatever
/// `PackageService`'s matching method already returns — see
/// `service::lifecycle` for what each of these corresponds to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponsePayload {
    /// `Ping`'s reply.
    Pong,
    /// `Install`, `Upgrade` (as a no-name-given no-op), `Hold`, `Unhold`,
    /// `Update` — anything whose `PackageService` method returns `()`.
    Unit,
    /// `Remove`, `Autoremove` — every package name actually removed.
    Removed { names: Vec<String> },
    /// `Upgrade` — `(name, old_version, new_version)` per package
    /// actually upgraded.
    Upgraded {
        changes: Vec<(String, Version, Version)>,
    },
    /// `List` — every installed package, name alongside its full record.
    Listed {
        packages: Vec<(String, InstalledPackage)>,
    },
    /// `Search` — every repository-index match.
    Searched { matches: Vec<PackageMetadata> },
    /// `Info`.
    Info(InfoView),
    /// `Clean` — bytes freed.
    Freed { bytes: u64 },
}

/// Serializes `value` as one JSON line and flushes it — flushing matters
/// here because both sides wrap their socket half in a `BufWriter`, which
/// otherwise holds bytes back until its buffer fills, and a reply sitting
/// in a buffer instead of on the wire looks identical to a hung daemon
/// from the other end.
pub fn write_json_line<W: Write, T: Serialize>(w: &mut W, value: &T) -> io::Result<()> {
    serde_json::to_writer(&mut *w, value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Reads and deserializes the next non-blank JSON line. `Ok(None)` means
/// the peer closed the connection (clean EOF), not "the line was
/// invalid" — a genuinely malformed line is `Err`, distinct from either.
pub fn read_json_line<R: BufRead, T: for<'de> Deserialize<'de>>(r: &mut R) -> io::Result<Option<T>> {
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return serde_json::from_str(trimmed)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
    }
}
