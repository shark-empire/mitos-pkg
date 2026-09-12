use crate::dependency::version::Dependency;
use crate::package::arch::default_arch;
use crate::package::hooks::Hooks;
use semver::Version;
use serde::{Deserialize, Serialize};

/// What a package maintainer hand-writes as `pkg.json` before running
/// `mitos-pkg build`. Deliberately a separate type from
/// `package::manifest::Manifest`: a spec has no `payload_sha256` or
/// `installed_size_bytes` (both computed *from* the payload directory at
/// build time, not authored) and no `files` list (the built manifest's is
/// filled in from whatever the builder actually finds under `payload/`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSpec {
    pub name: String,
    pub version: Version,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Declares who is expected to sign this package. Build still succeeds
    /// without `--sign-with` if this is set — it just leaves `Manifest`
    /// pointing at a signer with no signature yet published for it, which
    /// `package::signature::verify_package` will correctly refuse to trust.
    #[serde(default)]
    pub signer: Option<String>,
    /// Architectures this package installs on. See `package::arch` and
    /// `Manifest::arch`. Defaults to `["any"]`.
    #[serde(default = "default_arch")]
    pub arch: Vec<String>,
    /// See `Manifest::essential`. Defaults to `false`.
    #[serde(default)]
    pub essential: bool,
    /// See `package::hooks::Hooks`. Every path here is payload-relative —
    /// `mitos-pkg build` doesn't check the named files exist under
    /// `payload/` at build time (a maintainer could add them later without
    /// re-authoring `pkg.json`), but a hook pointing nowhere simply fails
    /// to run with a clear I/O error rather than silently doing nothing.
    #[serde(default)]
    pub hooks: Hooks,
}
