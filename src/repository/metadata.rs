use crate::dependency::version::Dependency;
use semver::Version;
use serde::{Deserialize, Serialize};

/// One entry in a `RepositoryIndex`: everything the resolver needs to
/// decide whether to install this package version *before* downloading
/// its full archive. This intentionally mirrors the fields of
/// `package::manifest::Manifest` that matter for resolution + verification
/// (name, version, deps, checksum, signature) without requiring a round
/// trip to fetch every candidate archive just to compare versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMetadata {
    pub name: String,
    pub version: Version,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub signature: Option<String>,
    pub size_bytes: u64,
}
