use crate::dependency::version::Dependency;
use crate::error::Result;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A package as recorded in the local install database. `dependencies` and
/// `installed_files` are snapshots taken at install time (from the
/// manifest and from what `package::archive::extract_payload` actually
/// wrote) so that removal and dependency checks never need to re-resolve
/// or re-read an archive that may no longer be cached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: Version,
    pub dependencies: Vec<Dependency>,
    pub installed_files: Vec<PathBuf>,
    /// True if the user asked for this package directly; false if it was
    /// pulled in only to satisfy another package's dependency. Not yet
    /// used for orphan cleanup, but recorded now so that a future
    /// `autoremove` doesn't require a database migration.
    pub explicit: bool,
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
