//! Architecture targeting for `.mpkg` packages.
//!
//! MITOS ships on more than one CPU architecture (x86_64 and AArch64), so a
//! package built for one is meaningless — or actively broken — installed on
//! the other. Every `PackageSpec`/`Manifest`/`PackageMetadata` carries an
//! `arch` list; `mitos-pkg` refuses to install anything that doesn't name
//! the target architecture (or `"any"`, for arch-independent payloads:
//! shell scripts, fonts, translation files, ...).

/// Sentinel meaning "installs on every architecture" — for payloads with
/// no compiled/arch-specific content.
pub const ANY: &str = "any";

/// What `pkg.json`/`Manifest`/`PackageMetadata` default to when they don't
/// mention `arch` at all: unconstrained, so every package built before
/// this field existed keeps installing exactly as it did before.
pub fn default_arch() -> Vec<String> {
    vec![ANY.to_string()]
}

/// The architecture this running `mitos-pkg` binary is on, using Rust's
/// own normalized target names (`"x86_64"`, `"aarch64"`) — the same
/// spelling MITOS's own repos already use. Used as the install target
/// unless `Config::target_arch` overrides it (e.g. `mitos-installer`
/// building an AArch64 image from an x86_64 CI runner).
pub fn host_arch() -> &'static str {
    std::env::consts::ARCH
}

/// True if a package declaring `pkg_arch` can be installed on `target`:
/// either it's arch-independent (`"any"`, or an empty list — treated the
/// same as `["any"]` for archives built before this field existed), or
/// `target` is literally in the list. Comparison is case-insensitive
/// since `pkg.json` is hand-authored and "AArch64"/"aarch64" is an easy
/// typo to make either way.
pub fn is_compatible(pkg_arch: &[String], target: &str) -> bool {
    pkg_arch.is_empty()
        || pkg_arch
            .iter()
            .any(|a| a.eq_ignore_ascii_case(ANY) || a.eq_ignore_ascii_case(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_matches_every_target() {
        assert!(is_compatible(&[ANY.to_string()], "x86_64"));
        assert!(is_compatible(&[ANY.to_string()], "aarch64"));
    }

    #[test]
    fn empty_list_is_treated_as_any_for_pre_existing_archives() {
        assert!(is_compatible(&[], "x86_64"));
    }

    #[test]
    fn exact_arch_matches_case_insensitively() {
        assert!(is_compatible(&["x86_64".to_string()], "x86_64"));
        assert!(is_compatible(&["AArch64".to_string()], "aarch64"));
    }

    #[test]
    fn mismatched_arch_is_rejected() {
        assert!(!is_compatible(&["aarch64".to_string()], "x86_64"));
    }

    #[test]
    fn multi_arch_list_matches_any_member() {
        assert!(is_compatible(
            &["x86_64".to_string(), "aarch64".to_string()],
            "aarch64"
        ));
    }
}
