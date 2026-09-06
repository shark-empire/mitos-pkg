use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One configured package repository. Deserializes from either a bare
/// URL string (every config written before repository signing existed)
/// or an object naming a required signer — `#[serde(untagged)]` tries
/// each shape in turn, so old `"repositories": ["https://..."]` configs
/// keep loading unmodified.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RepoSource {
    Url(String),
    Detailed {
        url: String,
        /// If set, `PackageService::update` refuses this repository's
        /// index unless `{url}.sig` holds a valid Ed25519 signature —
        /// over the index bytes' SHA-256 digest, by this signer — from a
        /// key already present in the trusted-keys store. `None` accepts
        /// an unsigned index, the same opt-in-by-default posture
        /// `Manifest::signer` takes for individual packages.
        #[serde(default)]
        signer: Option<String>,
    },
}

impl RepoSource {
    pub fn url(&self) -> &str {
        match self {
            RepoSource::Url(url) => url,
            RepoSource::Detailed { url, .. } => url,
        }
    }

    pub fn signer(&self) -> Option<&str> {
        match self {
            RepoSource::Url(_) => None,
            RepoSource::Detailed { signer, .. } => signer.as_deref(),
        }
    }
}

/// mitos-pkg's runtime configuration. Loaded once at startup from a JSON
/// file (see `DEFAULT_PATH`); every other module receives paths derived
/// from this rather than hardcoding them, so the whole tool can be pointed
/// at a chroot or test root with a single `--root` flag (see `cli::Cli`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Filesystem root packages are installed relative to. Normally "/",
    /// but overridable for chroots, image builds, or tests so nothing here
    /// ever needs real root on the host to be exercised.
    pub install_root: PathBuf,
    pub db_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub trusted_keys_dir: PathBuf,
    #[serde(default)]
    pub repositories: Vec<RepoSource>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            install_root: PathBuf::from("/"),
            db_dir: PathBuf::from("/var/lib/mitos-pkg"),
            cache_dir: PathBuf::from("/var/cache/mitos-pkg"),
            trusted_keys_dir: PathBuf::from("/etc/mitos-pkg/trusted-keys"),
            repositories: vec![RepoSource::Url(
                "https://packages.mitos-os.org/index.json".to_string(),
            )],
        }
    }
}

impl Config {
    pub const DEFAULT_PATH: &'static str = "/etc/mitos-pkg/config.json";

    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn packages_db_path(&self) -> PathBuf {
        self.db_dir.join("packages.json")
    }

    pub fn files_db_path(&self) -> PathBuf {
        self.db_dir.join("files.json")
    }

    pub fn index_cache_path(&self) -> PathBuf {
        self.cache_dir.join("index.json")
    }

    pub fn download_cache_path(&self, filename: &str) -> PathBuf {
        self.cache_dir.join("downloads").join(filename)
    }
}
