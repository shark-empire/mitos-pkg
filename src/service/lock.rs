use crate::error::{PkgError, Result};
use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// An exclusive lock over the package database, held for the duration of
/// any operation that mutates `InstalledDb`/`FileDb` (install, remove,
/// upgrade, autoremove).
///
/// Modeled on dpkg's `/var/lib/dpkg/lock` and pacman's `db.lck`: a lock
/// file created with `create_new`, which is atomic — the OS guarantees
/// only one process can be the one that actually creates it, so this
/// can't race the way a "check if a lock file exists, then create one"
/// pair of steps could. Released automatically when the guard drops,
/// including on an early return via `?`.
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn acquire(db_dir: &Path) -> Result<Self> {
        fs::create_dir_all(db_dir)?;
        let path = db_dir.join(".mitos-pkg.lock");
        match File::options().write(true).create_new(true).open(&path) {
            Ok(_) => Ok(Self { path }),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(PkgError::Locked(path)),
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Best-effort: if this fails there's nothing useful to do with the
        // error from inside a Drop impl, and leaving a stale lock file is
        // recoverable (the next run's error message names its path).
        let _ = fs::remove_file(&self.path);
    }
}
