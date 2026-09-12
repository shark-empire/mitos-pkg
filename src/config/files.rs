use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One configured package repository. Deserializes from either a bare
/// URL string (every config written before repository signing existed)
/// or an object naming a required signer, mirrors, and/or priority —
/// `#[serde(untagged)]` tries each shape in turn, so old
/// `"repositories": ["https://..."]` configs keep loading unmodified.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RepoSource {
    Url(String),
    Detailed {
        url: String,
        /// If set, `PackageService::update` refuses this repository's
        /// index unless one of its URLs (see `urls()`) serves a valid
        /// Ed25519 signature — over the index bytes' SHA-256 digest, by
        /// this signer — from a key already present in the trusted-keys
        /// store. `None` accepts an unsigned index, the same opt-in-by-
        /// default posture `Manifest::signer` takes for individual
        /// packages.
        #[serde(default)]
        signer: Option<String>,
        /// Additional URLs serving an identical copy of this
        /// repository's index, tried in order after `url` if it can't be
        /// reached (see `repository::download::fetch_with_mirrors`).
        /// Existing configs with no mirrors keep working — this defaults
        /// to empty, meaning "just `url`, no fallback".
        #[serde(default)]
        mirrors: Vec<String>,
        /// Higher wins when the *same version* of a package is listed by
        /// more than one configured repository (see
        /// `repository::index::RepositoryIndex`'s tie-breaking). A newer
        /// version from anywhere still always wins over an older one
        /// regardless of priority — this only disambiguates an exact
        /// tie. Repositories with no explicit priority default to `0`,
        /// so a freshly added repo with no stated opinion never silently
        /// outranks ones an admin already tuned.
        #[serde(default)]
        priority: i32,
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

    pub fn mirrors(&self) -> &[String] {
        match self {
            RepoSource::Url(_) => &[],
            RepoSource::Detailed { mirrors, .. } => mirrors,
        }
    }

    pub fn priority(&self) -> i32 {
        match self {
            RepoSource::Url(_) => 0,
            RepoSource::Detailed { priority, .. } => *priority,
        }
    }

    /// The primary URL followed by every configured mirror, in the order
    /// they should be tried.
    pub fn urls(&self) -> Vec<&str> {
        let mut all = vec![self.url()];
        all.extend(self.mirrors().iter().map(String::as_str));
        all
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
    /// Overrides `package::arch::host_arch()` as the architecture
    /// installs are resolved/checked against. `None` (the default) means
    /// "whatever CPU architecture this `mitos-pkg`/`mitos-pkgd` binary
    /// itself is running on" — set this when that's *not* the right
    /// answer, e.g. `mitos-installer` cross-building an AArch64 target
    /// image from an x86_64 CI runner.
    #[serde(default)]
    pub target_arch: Option<String>,
    /// Where `mitos-pkgd` (see `crate::daemon`) listens, and where
    /// `DaemonClient::connect` looks by default. `#[serde(default = ...)]`
    /// so a `config.json` written before the daemon existed still loads
    /// unmodified, picking up the standard path.
    #[serde(default = "Config::default_daemon_socket")]
    pub daemon_socket: PathBuf,
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
            target_arch: None,
            daemon_socket: Config::default_daemon_socket(),
        }
    }
}

impl Config {
    pub const DEFAULT_PATH: &'static str = "/etc/mitos-pkg/config.json";
    /// `/run` (not `/var/run`) per the FHS layout the rest of MITOS
    /// targets — a `tmpfs`-backed runtime directory that's expected to
    /// be recreated on every boot, which is exactly the lifetime a
    /// socket file should have.
    pub const DEFAULT_SOCKET: &'static str = "/run/mitos-pkg/pkgd.sock";

    fn default_daemon_socket() -> PathBuf {
        PathBuf::from(Self::DEFAULT_SOCKET)
    }

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

    /// Where `PackageService::history` reads from and every mutating
    /// operation appends to. Lives beside `packages.json`/`files.json`
    /// under `db_dir` rather than `cache_dir`, since — unlike the index
    /// cache or downloaded archives — history isn't something `clean`
    /// should ever delete.
    pub fn history_path(&self) -> PathBuf {
        self.db_dir.join("history.jsonl")
    }

    pub fn index_cache_path(&self) -> PathBuf {
        self.cache_dir.join("index.json")
    }

    pub fn download_cache_path(&self, filename: &str) -> PathBuf {
        self.cache_dir.join("downloads").join(filename)
    }
}
