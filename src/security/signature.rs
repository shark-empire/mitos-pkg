use crate::error::{PkgError, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

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

/// Signs `message` with a raw 32-byte Ed25519 seed, returning the 64-byte
/// signature. Only `mitos-pkg build --sign-with` ever calls this — every
/// other code path (install, remove, verify) only ever needs a public key,
/// never a private one.
///
/// Ed25519 signing is deterministic, so unlike key *generation* this needs
/// no RNG — which is why mitos-pkg doesn't carry a `keygen` command: any
/// 32 random bytes (e.g. `openssl rand -hex 32`) are a valid seed.
pub fn sign(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    let signing_key = SigningKey::from_bytes(seed);
    signing_key.sign(message).to_bytes()
}
