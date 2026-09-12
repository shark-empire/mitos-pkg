use crate::dependency::version::Dependency;
use crate::error::Result;
use crate::package::arch::default_arch;
use crate::package::hooks::Hooks;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A package as recorded in the local install database. Most fields are
/// snapshots taken at install time (from the manifest and from what
/// `package::archive::extract_payload` actually wrote), so that removal,
/// `info`, and dependency/conflict checks never need to re-resolve or
/// re-read an archive that may no longer be cached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: Version,
    #[serde(default)]
    pub description: String,
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    pub installed_files: Vec<PathBuf>,
    /// True if the user asked for this package directly; false if it was
    /// pulled in only to satisfy another package's dependency. Read by
    /// `PackageService::autoremove` to find orphaned dependencies.
    pub explicit: bool,
    /// True if the user pinned this package with `mitos-pkg hold`,
    /// exempting it from `upgrade` (mirrors `apt-mark hold` / `dnf
    /// versionlock` / pacman's `IgnorePkg`). `#[serde(default)]` so a
    /// database written before this field existed still loads cleanly,
    /// with every pre-existing package treated as unheld.
    #[serde(default)]
    pub held: bool,
    /// See `package::arch`. `#[serde(default)]` so a database written
    /// before this field existed loads cleanly, treating every
    /// pre-existing installed package as arch-independent (the closest
    /// approximation of "unknown" that doesn't spuriously block anything
    /// already on the system).
    #[serde(default = "default_arch")]
    pub arch: Vec<String>,
    /// See `Manifest::essential`. `#[serde(default)]` = not essential,
    /// for the same backward-compatibility reason as `arch`.
    #[serde(default)]
    pub essential: bool,
    /// Snapshot of `Manifest::hooks` at install time, so `remove` can run
    /// `pre_remove`/`post_remove` without needing the original archive
    /// still cached. `#[serde(default)]` = no hooks, for packages
    /// installed before this field existed.
    #[serde(default)]
    pub hooks: Hooks,
    /// Snapshot of `Manifest::payload_sha256` at install time — what
    /// `PackageService::verify` re-derives the on-disk files' aggregate
    /// hash against to detect drift/corruption/tampering after install.
    /// `#[serde(default)]` (empty string) for packages installed before
    /// this field existed; `verify` treats an empty expected hash as
    /// "nothing recorded to check against" rather than a mismatch.
    #[serde(default)]
    pub payload_sha256: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InstalledDb {
    packages: HashMap<String, InstalledPackage>,
    #[serde(skip)]
    path: PathBuf,
}

impl InstalledDb {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                packages: HashMap::new(),
                path: path.to_path_buf(),
            });
        }
        let data = std::fs::read_to_string(path)?;
        let mut db: InstalledDb = serde_json::from_str(&data)?;
        db.path = path.to_path_buf();
        Ok(db)
    }

    /// Writes via a temp file + rename so a crash mid-write can never leave
    /// a half-written, corrupt database on disk (rename is atomic on the
    /// same filesystem).
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&InstalledPackage> {
        self.packages.get(name)
    }

    /// Mutable access to an installed package's record — used by `hold`/
    /// `unhold` to flip `InstalledPackage::held` in place without a
    /// clone-mutate-reinsert round trip.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut InstalledPackage> {
        self.packages.get_mut(name)
    }

    pub fn all(&self) -> &HashMap<String, InstalledPackage> {
        &self.packages
    }

    pub fn insert(&mut self, pkg: InstalledPackage) {
        self.packages.insert(pkg.name.clone(), pkg);
    }

    pub fn remove(&mut self, name: &str) -> Option<InstalledPackage> {
        self.packages.remove(name)
    }

    pub fn is_installed(&self, name: &str) -> bool {
        self.packages.contains_key(name)
    }
}
