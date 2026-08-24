use crate::error::{PkgError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Reverse index: installed file path (relative to the install root) ->
/// owning package name. Kept separately from `InstalledDb` so a
/// file-ownership conflict can be checked in O(1) *before* any bytes are
/// written to disk, instead of scanning every installed package's file
/// list on every install.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FileDb {
    owners: HashMap<PathBuf, String>,
    #[serde(skip)]
    path: PathBuf,
}

impl FileDb {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                owners: HashMap::new(),
                path: path.to_path_buf(),
            });
        }
        let data = std::fs::read_to_string(path)?;
        let mut db: FileDb = serde_json::from_str(&data)?;
        db.path = path.to_path_buf();
        Ok(db)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Fails if any of `files` is already owned by a package other than
    /// `owner` (re-installing/upgrading the same package is fine).
    pub fn check_no_conflicts(&self, files: &[PathBuf], owner: &str) -> Result<()> {
        for f in files {
            if let Some(existing) = self.owners.get(f) {
                if existing != owner {
                    return Err(PkgError::FileConflict {
                        path: f.display().to_string(),
                        owner: existing.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn claim(&mut self, files: &[PathBuf], owner: &str) {
        for f in files {
            self.owners.insert(f.clone(), owner.to_string());
        }
    }

    pub fn release(&mut self, files: &[PathBuf]) {
        for f in files {
            self.owners.remove(f);
        }
    }

    pub fn owner_of(&self, file: &Path) -> Option<&String> {
        self.owners.get(file)
    }
}
