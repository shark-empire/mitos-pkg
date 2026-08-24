use crate::error::{PkgError, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

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
