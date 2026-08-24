use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

/// A named dependency plus the version range that satisfies it, e.g.
/// `mitos-libc >= 0.3, < 0.4`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    #[serde(rename = "version")]
    pub version_req: VersionReq,
}

impl Dependency {
    pub fn matches(&self, version: &Version) -> bool {
        self.version_req.matches(version)
    }
}
