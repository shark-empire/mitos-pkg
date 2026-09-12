use crate::dependency::version::Dependency;
use crate::package::arch::default_arch;
use semver::Version;
use serde::{Deserialize, Serialize};

/// One entry in a `RepositoryIndex`: everything the resolver needs to
/// decide whether to install this package version *before* downloading
/// its full archive. This intentionally mirrors the fields of
/// `package::manifest::Manifest` that matter for resolution + verification
/// (name, version, deps, provides, conflicts, checksum, signature, arch,
/// size) without requiring a round trip to fetch every candidate archive
/// just to compare versions. It does *not* mirror `Manifest::hooks` —
/// resolution and verification never need to know a hook exists, only
/// installation does, and by then the real `Manifest` has already been
/// read out of the downloaded archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMetadata {
    pub name: String,
    pub version: Version,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    pub url: String,
    /// SHA-256 of the whole `.mpkg` file, published here rather than
    /// inside the archive (see `package::manifest::Manifest::payload_sha256`
    /// for why that split exists).
    pub sha256: String,
    #[serde(default)]
    pub signature: Option<String>,
    pub size_bytes: u64,
    /// See `package::arch`. Defaults to `["any"]` so index entries written
    /// before this field existed keep resolving on every host.
    #[serde(default = "default_arch")]
    pub arch: Vec<String>,
    /// See `Manifest::essential`. Defaults to `false`.
    #[serde(default)]
    pub essential: bool,
    /// See `Manifest::installed_size_bytes`. Defaults to `0`, which
    /// disables the pre-download free-space check for index entries
    /// written before this field existed rather than falsely refusing
    /// every install against them.
    #[serde(default)]
    pub installed_size_bytes: u64,
    /// Stamped locally by `PackageService::update` from whichever
    /// configured `RepoSource` this entry was fetched from — **not**
    /// meaningful if set by whatever a repository server publishes, and
    /// overwritten on every `update()` regardless of what a served
    /// `index.json` claims. Only used to break a tie when the *same*
    /// version of a package is listed by more than one configured
    /// repository (see `RepositoryIndex::best_match`); it does not let a
    /// lower-priority repo's older version outrank a newer one from
    /// elsewhere.
    #[serde(default)]
    pub priority: i32,
}
