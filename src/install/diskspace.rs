//! Free-space guard, checked before an install plan writes anything.
//!
//! Uses the `libc` crate's `statvfs` binding rather than hand-rolling the
//! FFI: the raw `struct statvfs` layout is platform/ABI-specific (field
//! widths and padding differ across libc versions), and getting that
//! wrong from scratch is exactly the kind of subtle bug that's hard to
//! catch without a compiler in the loop. `libc` already gets it right
//! per-target, for a dependency that's little more than extern
//! declarations (no allocation, no runtime, no async) — see `Cargo.toml`'s
//! note on this choice.

use crate::error::{PkgError, Result};
use std::path::{Path, PathBuf};

/// Bytes currently free on the filesystem containing `path`. Uses
/// `f_bavail` (available to an unprivileged process), not `f_bfree` (raw
/// free blocks), so this doesn't report space reserved for root that
/// mitos-pkg couldn't actually use.
#[cfg(unix)]
pub fn available_bytes(path: &Path) -> Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // statvfs needs a path that exists; install_root may not yet if this
    // is a first-ever install into a freshly-created chroot/target, so
    // walk up to the nearest existing ancestor.
    let mut probe: PathBuf = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            probe = PathBuf::from(".");
            break;
        }
    }

    let c_path = CString::new(probe.as_os_str().as_bytes())
        .map_err(|e| PkgError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(PkgError::Io(std::io::Error::last_os_error()));
    }
    Ok(stat.f_frsize as u64 * stat.f_bavail as u64)
}

#[cfg(not(unix))]
pub fn available_bytes(_path: &Path) -> Result<u64> {
    // No portable stable-std way to query free space; on a non-Unix host
    // this check is skipped rather than guessed at (see `check` below).
    Ok(u64::MAX)
}

/// Refuses up front if `needed_bytes` clearly won't fit, rather than
/// letting an install run out of space partway through extraction — real
/// package managers (dpkg, dnf, pacman) all check this for the same
/// reason: a mid-write `ENOSPC` is a much messier failure mode than
/// refusing before a single byte is written.
///
/// `needed_bytes` is expected to already be a best-effort figure (see
/// `Manifest::installed_size_bytes`) — this only ever refuses on a clear,
/// unambiguous shortfall.
pub fn check(install_root: &Path, needed_bytes: u64) -> Result<()> {
    let available = available_bytes(install_root)?;
    if needed_bytes > available {
        return Err(PkgError::InsufficientDiskSpace {
            needed: needed_bytes,
            available,
        });
    }
    Ok(())
}
