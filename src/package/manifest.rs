use crate::dependency::version::Dependency;
use semver::Version;
use serde::{Deserialize, Serialize};

/// The `manifest.json` embedded at the root of every `.mpkg` archive.
/// This is the single source of truth for what a package is, what it
/// needs, and how to verify it — the repository index (`repository::metadata`)
/// carries a lighter copy of the same facts so packages can be resolved
/// without downloading every archive first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: Version,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    /// Virtual capabilities this package satisfies, for other packages'
    /// dependencies to name without pinning an exact implementation.
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Payload-relative paths this package installs, for documentation and
    /// pre-install sanity checks (the authoritative list used at install
    /// time is whatever `package::archive::extract_payload` actually wrote).
    #[serde(default)]
    pub files: Vec<String>,
    pub size_bytes: u64,
    /// SHA-256 of the whole `.mpkg` archive, hex-encoded.
    pub sha256: String,
    /// Name of the signer whose key must be in the local `KeyStore` for
    /// this package to be considered trusted. `None` means unsigned.
    #[serde(default)]
    pub signer: Option<String>,
}
