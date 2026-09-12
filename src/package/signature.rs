use crate::error::{PkgError, Result};
use crate::package::manifest::Manifest;
use crate::security::{checksum, keys::KeyStore, signature as sig};
use std::path::Path;

/// Verifies an archive against the trust chain before anything from it is
/// extracted:
///   1. the archive's own bytes must hash to `expected_archive_sha256` —
///      the checksum published in the repository index
///      (`repository::metadata::PackageMetadata::sha256`), *not* anything
///      inside the archive itself. This also re-checks a cached archive
///      that was verified on a previous run but never re-hashed since.
///   2. if the manifest names a signer, a matching signature over
///      `manifest.payload_sha256` must verify against a key in the local
///      `KeyStore`
///
/// Unsigned packages (`manifest.signer == None`) only get step 1 — that's
/// a deliberate policy choice left to the caller (e.g. `service::lifecycle`
/// could reject unsigned packages entirely for a stricter install mode).
pub fn verify_package(
    archive_path: &Path,
    expected_archive_sha256: &str,
    manifest: &Manifest,
    keystore: &KeyStore,
    signature_hex: Option<&str>,
) -> Result<()> {
    let data = std::fs::read(archive_path)?;
    checksum::verify(&data, expected_archive_sha256, &manifest.name)?;

    if let Some(signer) = &manifest.signer {
        let sig_hex = signature_hex.ok_or_else(|| PkgError::InvalidSignature(signer.clone()))?;
        let public_key = keystore
            .find(signer)
            .ok_or_else(|| PkgError::UntrustedKey(signer.clone()))?;
        let sig_bytes = decode_signature(sig_hex, signer)?;
        sig::verify_signature(
            public_key,
            manifest.payload_sha256.as_bytes(),
            &sig_bytes,
            signer,
        )?;
    }

    Ok(())
}

/// Verifies a repository index's raw bytes against a detached signature,
/// using the same trust model as `verify_package`: the signature covers
/// the hex SHA-256 digest of the content rather than the raw bytes
/// themselves (so signing and verifying both go through one well-tested
/// "hash it, then sign the hash" path), and the signer must be a key
/// already present in the local `KeyStore`. There's no separate "repo
/// key" concept — a signer trusted for packages is trusted for the
/// indexes that list them too.
///
/// Closes a gap plain per-package signing leaves open: without this, an
/// attacker who can replace a repository's index can point an *unsigned*
/// package's metadata at a malicious archive and simply publish that
/// archive's own (correct) checksum alongside it — the checksum check
/// passes because it's checking the archive against a number the
/// attacker also controls. Signing the index itself is the same
/// integrity model real package managers use for their sync metadata
/// (apt's Release/InRelease, pacman's sync db `.sig`, dnf's
/// `repomd.xml.asc`).
pub fn verify_index(
    data: &[u8],
    signature_hex: &str,
    signer: &str,
    keystore: &KeyStore,
) -> Result<()> {
    let public_key = keystore
        .find(signer)
        .ok_or_else(|| PkgError::UntrustedKey(signer.to_string()))?;
    let digest = checksum::hash_bytes(data);
    let sig_bytes = decode_signature(signature_hex, signer)?;
    sig::verify_signature(public_key, digest.as_bytes(), &sig_bytes, signer)
}

/// `pub` (rather than module-private) so `service::lifecycle` can decode a
/// `--signature` value passed on the command line for a local `.mpkg`
/// install the same way an index-published one is decoded here.
pub fn decode_signature(hex_str: &str, signer: &str) -> Result<[u8; 64]> {
    let bytes = hex::decode(hex_str).map_err(|_| PkgError::InvalidSignature(signer.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| PkgError::InvalidSignature(signer.to_string()))
}
