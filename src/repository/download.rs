use crate::error::{PkgError, Result};
use crate::security::checksum;
use std::io::Read;
use std::path::Path;

/// Abstracts "get me the bytes at this URL" so the resolver/service layers
/// never depend on a concrete HTTP client — useful for tests and for
/// swapping transports (e.g. a future local-mirror or USB-drive fetcher
/// for offline installs) without touching call sites.
pub trait Fetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>>;
}

/// Blocking HTTP(S) fetcher backed by `ureq`. Deliberately not an async
/// client: pulling in an async runtime (tokio, etc.) just to do one
/// download at a time would cost far more memory than it buys here, and
/// mitos-pkg installs packages sequentially by design (see
/// `service::lifecycle::PackageService::install`).
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        let mut response = ureq::get(url)
            .call()
            .map_err(|e| PkgError::Network(e.to_string()))?;
        let mut buf = Vec::new();
        response
            .body_mut()
            .as_reader()
            .read_to_end(&mut buf)
            .map_err(PkgError::Io)?;
        Ok(buf)
    }
}

/// Downloads `url`, verifies it against `expected_sha256` before it ever
/// touches disk as a trusted file, and writes it to `dest` only once
/// verified. `name` is just used to label a checksum-mismatch error.
pub fn download_verified(
    fetcher: &dyn Fetcher,
    url: &str,
    expected_sha256: &str,
    name: &str,
    dest: &Path,
) -> Result<()> {
    let data = fetcher.fetch(url)?;
    checksum::verify(&data, expected_sha256, name)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, data)?;
    Ok(())
}
