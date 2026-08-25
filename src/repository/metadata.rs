use crate::dependency::version::Dependency;
use semver::Version;
use serde::{Deserialize, Serialize};

/// One entry in a `RepositoryIndex`: everything the resolver needs to
/// decide whether to install this package version *before* downloading
/// its full archive. This intentionally mirrors the fields of
/// `package::manifest::Manifest` that matter for resolution + verification
/// (name, version, deps, provides, conflicts, checksum, signature) without
/// requiring a round trip to fetch every candidate archive just to compare
/// versions.
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
}
