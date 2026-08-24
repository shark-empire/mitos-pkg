use crate::error::{PkgError, Result};
use crate::package::format::{MANIFEST_FILE_NAME, PAYLOAD_DIR};
use crate::package::manifest::Manifest;
use flate2::read::GzDecoder;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;

/// Reads just the manifest out of a `.mpkg` archive, without touching the
/// payload. Used before install to decide whether a package is even worth
/// downloading further / trusting, so we never extract untrusted payload
/// bytes before the manifest + signature have been checked.
pub fn read_manifest(archive_path: &Path) -> Result<Manifest> {
    let file = File::open(archive_path)?;
    let gz = GzDecoder::new(file);
    let mut archive = Archive::new(gz);

    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() == Path::new(MANIFEST_FILE_NAME) {
            let mut contents = String::new();
            entry.read_to_string(&mut contents)?;
            return serde_json::from_str(&contents)
                .map_err(|e| PkgError::InvalidManifest(e.to_string()));
        }
    }

    Err(PkgError::InvalidManifest(format!(
        "no {} found in {}",
        MANIFEST_FILE_NAME,
        archive_path.display()
    )))
}

/// Extracts the `payload/` directory of a `.mpkg` archive into `dest_root`,
/// returning the list of files written (relative to `dest_root`). Should
/// only be called after `package::signature::verify_package` has passed.
pub fn extract_payload(archive_path: &Path, dest_root: &Path) -> Result<Vec<PathBuf>> {
    let file = File::open(archive_path)?;
    let gz = GzDecoder::new(file);
    let mut archive = Archive::new(gz);
    let mut installed = Vec::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let rel = match path.strip_prefix(PAYLOAD_DIR) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
            _ => continue, // manifest.json and the bare payload/ dir entry
        };

        let dest = dest_root.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&dest)?;
        installed.push(rel);
    }

    Ok(installed)
}
