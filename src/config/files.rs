use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    pub repositories: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            install_root: PathBuf::from("/"),
            db_dir: PathBuf::from("/var/lib/mitos-pkg"),
            cache_dir: PathBuf::from("/var/cache/mitos-pkg"),
            trusted_keys_dir: PathBuf::from("/etc/mitos-pkg/trusted-keys"),
            repositories: vec!["https://packages.mitos-os.org/index.json".to_string()],
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
