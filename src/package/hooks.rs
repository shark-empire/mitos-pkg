//! Maintainer-script hook *declarations* embedded in a package. Execution
//! lives in `install::hooks`; this module only defines what a manifest can
//! say.
//!
//! Deliberately narrower than dpkg's postinst/postrm or rpm's `%post`: a
//! hook is not an arbitrary inline command string in metadata (which would
//! let a compromised or malicious repository index execute anything —
//! index *signing* only covers the listing, not command strings a client
//! might blindly run) but a **payload-relative path to an executable file
//! shipped inside the same `.mpkg` payload/** that already goes through
//! the full checksum + signature trust chain
//! (`package::signature::verify_package`) before anything is extracted. A
//! hook can't run unless the exact bytes it points at already passed that
//! check — there is no separate trust mechanism to bypass.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hooks {
    /// Run once the payload is staged but before it's moved into place —
    /// see `install::transaction` for why "staged" rather than "already
    /// installed". A non-zero exit aborts the install before the real
    /// install root is touched at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_install: Option<String>,
    /// Run after payload files are in their final place and both
    /// on-disk databases are persisted. A non-zero exit fails the install
    /// and rolls it back, even though the files were already committed —
    /// mitos-pkg treats "ran its post-install step successfully" as part
    /// of what "installed" means, the same way dpkg marks a package
    /// `half-configured` rather than `installed` when `postinst` fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_install: Option<String>,
    /// Run before an installed package's files are removed, while they're
    /// still present. A non-zero exit aborts the removal before any files
    /// are touched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_remove: Option<String>,
    /// Run after an installed package's files are deleted. Best-effort:
    /// failure is logged to stderr but does not fail `remove`, since the
    /// files this hook might want to react to are already gone — there is
    /// nothing left to roll back *to*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_remove: Option<String>,
}

impl Hooks {
    pub fn is_empty(&self) -> bool {
        self.pre_install.is_none()
            && self.post_install.is_none()
            && self.pre_remove.is_none()
            && self.post_remove.is_none()
    }
}
