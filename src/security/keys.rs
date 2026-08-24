use crate::error::Result;
use std::collections::HashMap;
use std::path::Path;

/// Trusted signer public keys, loaded from a directory of `<signer>.pub`
/// files (each containing a hex-encoded 32-byte Ed25519 public key).
///
/// A package is only trusted if it is signed by a key present here — this
/// is intentionally a local allowlist rather than a web-of-trust or CA
/// model, which keeps it auditable and dependency-free.
#[derive(Debug, Default)]
pub struct KeyStore {
    keys: HashMap<String, [u8; 32]>,
}

impl KeyStore {
    pub fn load_dir(dir: &Path) -> Result<Self> {
        let mut keys = HashMap::new();
        if !dir.exists() {
            return Ok(Self { keys });
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("pub") {
                continue;
            }
            let Some(signer) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };

            let hex_str = std::fs::read_to_string(&path)?;
            let Ok(bytes) = hex::decode(hex_str.trim()) else {
                continue;
            };
            let Ok(key) = bytes.try_into() else {
                continue;
            };
            keys.insert(signer.to_string(), key);
        }

        Ok(Self { keys })
    }

    pub fn find(&self, signer: &str) -> Option<&[u8; 32]> {
        self.keys.get(signer)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}
