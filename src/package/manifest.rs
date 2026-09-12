use crate::dependency::version::Dependency;
use crate::package::arch::default_arch;
use crate::package::hooks::Hooks;
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
    /// Aggregate SHA-256 over every file in `payload/`, computed by
    /// `security::checksum::hash_payload_dir` at build time and re-checked
    /// after extraction. Deliberately *not* a hash of the whole `.mpkg`
    /// archive: that value would have to include this manifest, which
    /// would have to include that value — self-referential and impossible
    /// to satisfy. The whole-archive checksum used to verify a download
    /// before it's even opened lives in `repository::metadata::PackageMetadata`
    /// instead, published separately (the same split real repositories use:
    /// e.g. a package's own control data vs. its entry in the sync index).
    pub payload_sha256: String,
    /// Name of the signer whose key must be in the local `KeyStore` for
    /// this package to be considered trusted. `None` means unsigned.
    /// What actually gets signed is `payload_sha256` (see
    /// `package::signature::verify_package`).
    #[serde(default)]
    pub signer: Option<String>,
    /// Architectures this package installs on (`"x86_64"`, `"aarch64"`, or
    /// `"any"`). Defaults to `["any"]` so archives built before this field
    /// existed keep installing exactly as before. See `package::arch`.
    #[serde(default = "default_arch")]
    pub arch: Vec<String>,
    /// Core-system package: `remove`/`autoremove` refuse to touch it
    /// without an explicit override (`PkgError::EssentialPackage`) —
    /// mirrors dpkg's `Essential: yes` and pacman's `base`/`base-devel`
    /// group protection. Defaults to `false`.
    #[serde(default)]
    pub essential: bool,
    /// Payload-relative paths to executable scripts run at defined points
    /// in install/remove, if present. See `install::hooks`.
    #[serde(default)]
    pub hooks: Hooks,
    /// Total uncompressed size of `payload/`, in bytes, computed at build
    /// time (`package::build`). Used for the pre-download/pre-extract
    /// free-space check (`install::diskspace`) — checked *before*
    /// spending bandwidth on a download that could never fit, not just
    /// before extraction. `#[serde(default)]` so a manifest built before
    /// this field existed still loads; its install just skips the
    /// early-refusal (the post-extraction disk-full case, while messier,
    /// was already the only behavior before this field existed at all).
    #[serde(default)]
    pub installed_size_bytes: u64,
}
