use crate::error::{PkgError, Result};
use crate::security::checksum;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

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

/// How many times a single URL is retried before giving up on it, and how
/// long to back off between attempts. A constrained/mobile network is
/// exactly where a one-shot fetch is most likely to hit a transient
/// failure worth retrying, and exactly where re-running the whole
/// `mitos-pkg` invocation by hand to get the same effect is most
/// annoying.
const RETRY_ATTEMPTS: u32 = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(300);

/// Fetches `url`, retrying up to `RETRY_ATTEMPTS` times with exponential
/// backoff on failure. Returns the last error if every attempt fails.
pub fn fetch_with_retry(fetcher: &dyn Fetcher, url: &str) -> Result<Vec<u8>> {
    let mut last_err = None;
    for attempt in 0..RETRY_ATTEMPTS {
        match fetcher.fetch(url) {
            Ok(data) => return Ok(data),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < RETRY_ATTEMPTS {
                    std::thread::sleep(RETRY_BASE_DELAY * 2u32.pow(attempt));
                }
            }
        }
    }
    Err(last_err.expect("loop runs at least once, so last_err is always set on failure"))
}

/// Tries each URL in `urls` in order (each with its own `fetch_with_retry`
/// retries), returning the first one that succeeds. Used for a
/// `RepoSource` configured with mirrors: a whole mirror being down (not
/// just one request) falls through to the next one instead of failing the
/// entire `update`.
pub fn fetch_with_mirrors(fetcher: &dyn Fetcher, urls: &[&str]) -> Result<Vec<u8>> {
    let mut last_err = None;
    for url in urls {
        match fetch_with_retry(fetcher, url) {
            Ok(data) => return Ok(data),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| PkgError::Network("no repository URL configured".to_string())))
}

/// Downloads `url` (retrying transient failures), verifies it against
/// `expected_sha256` before it ever touches disk as a trusted file, and
/// writes it to `dest` only once verified. `name` is just used to label a
/// checksum-mismatch error.
pub fn download_verified(
    fetcher: &dyn Fetcher,
    url: &str,
    expected_sha256: &str,
    name: &str,
    dest: &Path,
) -> Result<()> {
    let data = fetch_with_retry(fetcher, url)?;
    checksum::verify(&data, expected_sha256, name)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, data)?;
    Ok(())
}
