use crate::error::{PkgError, Result};
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

/// Parses a CLI package argument of the form `name` or `name@version`
/// (e.g. `mitos-libc@0.3.5`) into a package name and the version
/// requirement it should be resolved against. A bare name resolves
/// against `VersionReq::STAR` (any version — i.e. whatever the resolver
/// would otherwise pick as latest). `name@version` pins an *exact*
/// version, the same way `apt install pkg=1.2.3` or `dnf install
/// pkg-1.2.3` do — not a range, so it always installs precisely what was
/// asked for or fails.
pub fn parse_pinned(spec: &str) -> Result<(String, VersionReq)> {
    match spec.split_once('@') {
        None => Ok((spec.to_string(), VersionReq::STAR)),
        Some((name, version_str)) if !name.is_empty() => {
            let version = Version::parse(version_str)
                .map_err(|_| PkgError::InvalidPackageSpec(spec.to_string()))?;
            let req = VersionReq::parse(&format!("={version}"))
                .expect("an exact Version always parses back as a VersionReq");
            Ok((name.to_string(), req))
        }
        Some(_) => Err(PkgError::InvalidPackageSpec(spec.to_string())),
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

    #[test]
    fn parse_pinned_bare_name_is_unconstrained() {
        let (name, req) = parse_pinned("mitos-libc").unwrap();
        assert_eq!(name, "mitos-libc");
        assert_eq!(req, VersionReq::STAR);
    }

    #[test]
    fn parse_pinned_name_at_version_pins_exact_version() {
        let (name, req) = parse_pinned("mitos-libc@0.3.5").unwrap();
        assert_eq!(name, "mitos-libc");
        assert!(req.matches(&Version::parse("0.3.5").unwrap()));
        assert!(!req.matches(&Version::parse("0.3.6").unwrap()));
    }

    #[test]
    fn parse_pinned_rejects_malformed_specs() {
        assert!(matches!(
            parse_pinned("mitos-libc@not-a-version"),
            Err(PkgError::InvalidPackageSpec(_))
        ));
        assert!(matches!(
            parse_pinned("@0.3.5"),
            Err(PkgError::InvalidPackageSpec(_))
        ));
    }
}
