use crate::error::Result;
use std::path::{Path, PathBuf};

/// Deletes every file in `written` (relative to `install_root`). Used to
/// undo a partially-applied install after a failure mid-transaction.
///
/// Best-effort by design: rollback runs *because* something already went
/// wrong, so it must not itself be able to abort the process — a file that
/// can't be removed is logged to stderr and skipped rather than returned
/// as an error.
pub fn undo_install(install_root: &Path, written: &[PathBuf]) {
    for rel in written {
        let full = install_root.join(rel);
        if let Err(e) = std::fs::remove_file(&full) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "mitos-pkg: rollback: failed to remove {}: {e}",
                    full.display()
                );
            }
        }
    }
}

/// Restores files from `backup_dir` back into `install_root`, undoing a
/// failed removal. Unlike `undo_install`, this one does return `Result`:
/// a failed *restore* is serious enough that the caller should surface it
/// rather than silently leaving the system without those files.
pub fn undo_removal(install_root: &Path, backup_dir: &Path, files: &[PathBuf]) -> Result<()> {
    for rel in files {
        let src = backup_dir.join(rel);
        if !src.exists() {
            continue;
        }
        let dest = install_root.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&src, &dest)?;
    }
    Ok(())
}
