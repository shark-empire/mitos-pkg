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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_respects_version_requirement() {
        let dep = Dependency {
            name: "mitos-libc".to_string(),
            version_req: VersionReq::parse(">=0.3.0, <0.4.0").unwrap(),
        };

        assert!(dep.matches(&Version::parse("0.3.5").unwrap()));
        assert!(!dep.matches(&Version::parse("0.4.0").unwrap()));
        assert!(!dep.matches(&Version::parse("0.2.9").unwrap()));
    }
}
