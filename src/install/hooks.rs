use crate::error::{PkgError, Result};
use std::path::Path;
use std::process::Command;

/// Which lifecycle point a hook runs at. Only used to label errors and
/// pick pass/fail-hard vs. best-effort behavior — which hook script (if
/// any) applies at each point lives in `package::hooks::Hooks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPoint {
    PreInstall,
    PostInstall,
    PreRemove,
    PostRemove,
}

impl HookPoint {
    fn label(self) -> &'static str {
        match self {
            HookPoint::PreInstall => "pre_install",
            HookPoint::PostInstall => "post_install",
            HookPoint::PreRemove => "pre_remove",
            HookPoint::PostRemove => "post_remove",
        }
    }

    /// `post_remove` is best-effort (see `package::hooks::Hooks`) — every
    /// other point fails the whole operation it's part of.
    fn is_best_effort(self) -> bool {
        matches!(self, HookPoint::PostRemove)
    }
}

/// Runs `script_path` as a direct child process — **never through a
/// shell**, so nothing in a package name, version, or path can be
/// interpreted as shell syntax. The script receives `MITOS_PKG_ROOT`
/// (the real install root the package lives in or is headed for),
/// `MITOS_PKG_NAME`, `MITOS_PKG_VERSION`, and `MITOS_PKG_HOOK` (this
/// point's label) in its environment, plus whatever this process's own
/// environment already contains (`Command` inherits by default — the same
/// way a real system service would normally be launched).
///
/// A failing pre_install/post_install/pre_remove hook returns
/// `Err(PkgError::HookFailed)`; a failing post_remove hook is logged to
/// stderr and treated as success (see `HookPoint::is_best_effort`).
pub fn run(
    point: HookPoint,
    script_path: &Path,
    install_root: &Path,
    pkg_name: &str,
    pkg_version: &str,
) -> Result<()> {
    let outcome = Command::new(script_path)
        .env("MITOS_PKG_ROOT", install_root)
        .env("MITOS_PKG_NAME", pkg_name)
        .env("MITOS_PKG_VERSION", pkg_version)
        .env("MITOS_PKG_HOOK", point.label())
        .status();

    let failure_reason = match outcome {
        Ok(status) if status.success() => return Ok(()),
        Ok(status) => format!(
            "{} exited with {}",
            script_path.display(),
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "no exit code (terminated by signal)".to_string())
        ),
        Err(e) => format!("failed to run {}: {e}", script_path.display()),
    };

    if point.is_best_effort() {
        eprintln!(
            "mitos-pkg: warning: {} hook for '{pkg_name}' failed: {failure_reason}",
            point.label()
        );
        Ok(())
    } else {
        Err(PkgError::HookFailed {
            package: pkg_name.to_string(),
            hook: point.label().to_string(),
            reason: failure_reason,
        })
    }
}
