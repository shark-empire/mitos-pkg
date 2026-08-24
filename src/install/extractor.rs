use crate::error::Result;
use crate::package::archive;
use std::path::{Path, PathBuf};

/// Extracts a verified package's payload beneath `install_root`, returning
/// the files written, relative to `install_root`. Thin on purpose: all the
/// archive-format knowledge lives in `package::archive`, this module just
/// pins down *where* things get written.
pub fn extract_into(archive_path: &Path, install_root: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(install_root)?;
    archive::extract_payload(archive_path, install_root)
}
