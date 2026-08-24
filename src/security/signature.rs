use crate::error::{PkgError, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Verifies a raw Ed25519 signature over `message`, produced by the holder
/// of the private key matching `public_key_bytes`. `signer` is only used to
/// label errors (it is not part of the cryptographic check).
///
/// This is a primitive: it knows nothing about packages or manifests. The
/// package-specific meaning of "what gets signed" lives in
/// `package::signature`.
pub fn verify_signature(
    public_key_bytes: &[u8; 32],
    message: &[u8],
    signature_bytes: &[u8; 64],
    signer: &str,
) -> Result<()> {
    let verifying_key = VerifyingKey::from_bytes(public_key_bytes)
        .map_err(|_| PkgError::InvalidSignature(signer.to_string()))?;
    let signature = Signature::from_bytes(signature_bytes);
    verifying_key
        .verify(message, &signature)
        .map_err(|_| PkgError::InvalidSignature(signer.to_string()))
}
