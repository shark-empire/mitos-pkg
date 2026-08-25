use crate::error::{PkgError, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Hashes raw bytes and returns the lowercase hex digest.
pub fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Hashes the contents of a file on disk.
pub fn hash_file(path: &Path) -> Result<String> {
    let data = std::fs::read(path)?;
    Ok(hash_bytes(&data))
}

/// Verifies `data` hashes to `expected_hex`. `context` is used only to
/// produce a useful error (e.g. the package name being checked).
pub fn verify(data: &[u8], expected_hex: &str, context: &str) -> Result<()> {
    let actual = hash_bytes(data);
    if actual.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(PkgError::ChecksumMismatch {
            name: context.to_string(),
            expected: expected_hex.to_string(),
            actual,
        })
    }
}

/// Hashes an explicit list of files under `root` into one aggregate
/// SHA-256: each file is hashed individually, the (path, hash) pairs are
/// sorted for a reproducible order, joined into a `sha256sum`-style
/// listing, then that listing is hashed. This is what
/// `Manifest::payload_sha256` is — deliberately a hash of *file contents*
/// rather than of a tar/gzip byte stream, since compressed-archive bytes
/// aren't reproducible across encoder runs and would make this fragile.
///
/// Used both by `package::build` (to compute the hash while packaging)
/// and by `install::transaction` (to recompute it after extraction and
/// confirm nothing was corrupted or tampered with in transit).
pub fn hash_files(root: &Path, relative_paths: &[PathBuf]) -> Result<String> {
    let mut entries: Vec<(PathBuf, String)> = Vec::with_capacity(relative_paths.len());
    for rel in relative_paths {
        let hash = hash_file(&root.join(rel))?;
        entries.push((rel.clone(), hash));
    }
    entries.sort();

    let mut listing = String::new();
    for (rel, hash) in &entries {
        listing.push_str(&hash);
        listing.push_str("  ");
        listing.push_str(&rel.to_string_lossy());
        listing.push('\n');
    }
    Ok(hash_bytes(listing.as_bytes()))
}

/// Same as `hash_files`, but walks every file under `dir` recursively
/// instead of taking an explicit list. Used at build time, when `dir` is
/// a package's whole `payload/` directory and so is known to contain
/// exactly (and only) that package's files.
pub fn hash_payload_dir(dir: &Path) -> Result<String> {
    let mut relative_paths = Vec::new();
    collect_relative_paths(dir, dir, &mut relative_paths)?;
    hash_files(dir, &relative_paths)
}

/// Lists every file under `dir`, relative to `dir`, sorted. Used by
/// `package::build` to populate `Manifest::files` for documentation
/// purposes — the list actually enforced at install time always comes
/// from whatever `package::archive::extract_payload` wrote, not this.
pub fn list_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_relative_paths(dir, dir, &mut paths)?;
    paths.sort();
    Ok(paths)
}

fn collect_relative_paths(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_relative_paths(root, &path, out)?;
        } else {
            out.push(
                path.strip_prefix(root)
                    .expect("walked path is always under root")
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_hex_encoded() {
        let digest = hash_bytes(b"mitos");
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(digest, hash_bytes(b"mitos"));
    }

    #[test]
    fn verify_accepts_matching_checksum() {
        let expected = hash_bytes(b"payload bytes");
        assert!(verify(b"payload bytes", &expected, "test-pkg").is_ok());
    }

    #[test]
    fn verify_rejects_mismatched_checksum() {
        let wrong = hash_bytes(b"something else");
        let err = verify(b"payload bytes", &wrong, "test-pkg").unwrap_err();
        assert!(matches!(err, PkgError::ChecksumMismatch { .. }));
    }

    #[test]
    fn payload_hash_matches_between_full_walk_and_explicit_list() {
        let dir = std::env::temp_dir().join("mitos-pkg-test-payload-hash");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/hello"), b"hello world").unwrap();
        std::fs::write(dir.join("readme"), b"a readme").unwrap();

        let via_walk = hash_payload_dir(&dir).unwrap();
        let via_list = hash_files(
            &dir,
            &[PathBuf::from("bin/hello"), PathBuf::from("readme")],
        )
        .unwrap();

        assert_eq!(via_walk, via_list);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
