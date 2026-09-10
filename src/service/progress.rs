//! Typed progress reporting for long-running `PackageService` operations
//! (install, upgrade, update, remove, autoremove). Optional and
//! zero-cost-ish when unused: every mutating operation has a
//! `*_with_progress` twin taking a `&mut Progress`, and the original
//! plain methods (`install`, `upgrade`, ...) just pass `Progress::none()`
//! — nothing about existing call sites or their behavior changes.
//!
//! This exists mainly for `mitos-pkgd` (see `crate::daemon`), which
//! streams these events to whatever client asked for the operation, so a
//! non-terminal, GUI-driven install shows "resolving... downloading
//! mitos-shell... verifying... installing..." instead of one long blocked
//! call with no feedback. Any caller can use it, daemon or not — the CLI
//! could just as well print these as it goes.

use serde::{Deserialize, Serialize};

/// One step of a `PackageService` operation. For `install`/`upgrade`,
/// expect these in the order a user would picture them for a single
/// package: resolve once for the whole spec, then fetch -> verify ->
/// install per package pulled in. `remove`/`update` each emit only their
/// own single-purpose variant, once per package/repository respectively.
///
/// Deliberately coarse (per-package phase, not per-byte-downloaded):
/// byte-level progress would mean threading a callback into
/// `repository::download`'s HTTP loop, a bigger change for a use case
/// (an install taking noticeably long enough to want a percentage, not
/// just a phase label) this crate hasn't needed yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// Resolving `spec`'s dependency tree against the repository index.
    Resolving { spec: String },
    /// Downloading a package archive not already in the download cache.
    Fetching { name: String, version: String },
    /// Checking a (possibly just-downloaded, possibly cached) archive's
    /// checksum and signature.
    Verifying { name: String, version: String },
    /// Extracting payload files and recording the package as installed.
    Installing { name: String, version: String },
    /// Removing an installed package's files and database entry.
    Removing { name: String },
    /// Refreshing one configured repository's index.
    Updating { repository: String },
}

/// Carries an optional progress callback through a `*_with_progress` call
/// tree without changing every intermediate method's signature to
/// `Option<...>` and every call site to match on it. `Progress::none()`
/// is what the plain (non-`_with_progress`) methods pass, so a caller who
/// never asked for progress pays one `if let Some` branch per event and
/// nothing else.
pub struct Progress<'a> {
    sink: Option<&'a mut dyn FnMut(ProgressEvent)>,
}

impl<'a> Progress<'a> {
    /// No one's listening — every `emit` becomes a no-op check.
    pub fn none() -> Self {
        Self { sink: None }
    }

    /// Every emitted event is passed to `sink` as it happens (not
    /// batched), so a daemon connection can write it straight to the
    /// client socket while the operation is still running.
    pub fn new(sink: &'a mut dyn FnMut(ProgressEvent)) -> Self {
        Self { sink: Some(sink) }
    }

    pub(crate) fn emit(&mut self, event: ProgressEvent) {
        if let Some(sink) = self.sink.as_mut() {
            sink(event);
        }
    }
}
