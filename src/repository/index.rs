use crate::error::Result;
use crate::repository::metadata::PackageMetadata;
use semver::VersionReq;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// The merged, locally-cached view of every configured repository:
/// package name -> every version known to be available. Refreshed by
/// `PackageService::update`, read by `Resolver` on every install/search.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RepositoryIndex {
    pub packages: HashMap<String, Vec<PackageMetadata>>,
}

impl RepositoryIndex {
    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// The newest known version of `name`, regardless of any requirement.
    pub fn latest(&self, name: &str) -> Option<&PackageMetadata> {
        self.packages
            .get(name)?
            .iter()
            .max_by_key(|m| m.version.clone())
    }

    /// The newest version of `name` that satisfies `req`.
    pub fn best_match(&self, name: &str, req: &VersionReq) -> Option<&PackageMetadata> {
        self.packages
            .get(name)?
            .iter()
            .filter(|m| req.matches(&m.version))
            .max_by_key(|m| m.version.clone())
    }

    /// Case-insensitive substring search over name and description,
    /// returning each matching package's newest version.
    pub fn search(&self, query: &str) -> Vec<&PackageMetadata> {
        let q = query.to_lowercase();
        self.packages
            .values()
            .filter_map(|versions| versions.iter().max_by_key(|m| m.version.clone()))
            .filter(|m| {
                m.name.to_lowercase().contains(&q) || m.description.to_lowercase().contains(&q)
            })
            .collect()
    }
}
