use crate::error::{PkgError, Result};
use crate::package::manifest::Manifest;
use crate::security::{checksum, keys::KeyStore, signature as sig};
use std::path::Path;

/// Verifies an archive against the trust chain before anything from it is
/// extracted:
///   1. the archive's bytes must hash to the checksum the manifest claims
///   2. if the manifest names a signer, a matching signature over that
///      checksum must verify against a key in the local `KeyStore`
///
/// Unsigned packages (`manifest.signer == None`) only get step 1 — that's
/// a deliberate policy choice left to the caller (e.g. `service::lifecycle`
/// could reject unsigned packages entirely for a stricter install mode).
pub fn verify_package(
    archive_path: &Path,
    manifest: &Manifest,
    keystore: &KeyStore,
    signature_hex: Option<&str>,
) -> Result<()> {
    let data = std::fs::read(archive_path)?;
    checksum::verify(&data, &manifest.sha256, &manifest.name)?;

    if let Some(signer) = &manifest.signer {
        let sig_hex = signature_hex.ok_or_else(|| PkgError::InvalidSignature(signer.clone()))?;
        let public_key = keystore
            .find(signer)
            .ok_or_else(|| PkgError::UntrustedKey(signer.clone()))?;
        let sig_bytes = decode_signature(sig_hex, signer)?;
        sig::verify_signature(public_key, manifest.sha256.as_bytes(), &sig_bytes, signer)?;
    }

    Ok(())
}

fn decode_signature(hex_str: &str, signer: &str) -> Result<[u8; 64]> {
    let bytes =
        hex::decode(hex_str).map_err(|_| PkgError::InvalidSignature(signer.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| PkgError::InvalidSignature(signer.to_string()))
}
