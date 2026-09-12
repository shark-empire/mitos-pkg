use crate::error::Result;
use crate::package::arch;
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
    /// Ties (identical version published by more than one configured
    /// repository) are broken by `PackageMetadata::priority`.
    pub fn latest(&self, name: &str) -> Option<&PackageMetadata> {
        self.packages
            .get(name)?
            .iter()
            .max_by_key(|m| (m.version.clone(), m.priority))
    }

    /// Same as `latest`, but only considers versions installable on
    /// `target_arch` (see `package::arch`). Used everywhere resolution or
    /// upgrade planning actually picks a version to install — `latest`
    /// itself is kept arch-unaware for callers (tests, generic listings)
    /// that only want "what's the newest thing published", not "what can
    /// I actually install here".
    pub fn latest_for_arch(&self, name: &str, target_arch: &str) -> Option<&PackageMetadata> {
        self.packages
            .get(name)?
            .iter()
            .filter(|m| arch::is_compatible(&m.arch, target_arch))
            .max_by_key(|m| (m.version.clone(), m.priority))
    }

    /// The newest version of `name` that satisfies `req`.
    pub fn best_match(&self, name: &str, req: &VersionReq) -> Option<&PackageMetadata> {
        self.packages
            .get(name)?
            .iter()
            .filter(|m| req.matches(&m.version))
            .max_by_key(|m| (m.version.clone(), m.priority))
    }

    /// Same as `best_match`, filtered to versions installable on
    /// `target_arch`. This is what `Resolver` actually calls — see
    /// `latest_for_arch` for why the arch-unaware version still exists
    /// separately.
    pub fn best_match_for_arch(
        &self,
        name: &str,
        req: &VersionReq,
        target_arch: &str,
    ) -> Option<&PackageMetadata> {
        self.packages
            .get(name)?
            .iter()
            .filter(|m| req.matches(&m.version) && arch::is_compatible(&m.arch, target_arch))
            .max_by_key(|m| (m.version.clone(), m.priority))
    }

    /// The newest package that declares `virtual_name` in its `provides`
    /// list — used when a dependency names a capability rather than a
    /// concrete package (e.g. depending on "mitos-libc" being satisfiable
    /// by either "mitos-libc-musl" or "mitos-libc-glibc"). Deliberately
    /// unversioned, matching how most real package managers treat
    /// `provides`/`Provides:` entries.
    pub fn find_provider(&self, virtual_name: &str) -> Option<&PackageMetadata> {
        self.packages
            .values()
            .flatten()
            .filter(|m| m.provides.iter().any(|p| p == virtual_name))
            .max_by_key(|m| (m.version.clone(), m.priority))
    }

    /// Same as `find_provider`, filtered to providers installable on
    /// `target_arch`.
    pub fn find_provider_for_arch(
        &self,
        virtual_name: &str,
        target_arch: &str,
    ) -> Option<&PackageMetadata> {
        self.packages
            .values()
            .flatten()
            .filter(|m| {
                m.provides.iter().any(|p| p == virtual_name)
                    && arch::is_compatible(&m.arch, target_arch)
            })
            .max_by_key(|m| (m.version.clone(), m.priority))
    }

    /// Case-insensitive substring search over name and description,
    /// returning each matching package's newest version.
    pub fn search(&self, query: &str) -> Vec<&PackageMetadata> {
        let q = query.to_lowercase();
        self.packages
            .values()
            .filter_map(|versions| {
                versions
                    .iter()
                    .max_by_key(|m| (m.version.clone(), m.priority))
            })
            .filter(|m| {
                m.name.to_lowercase().contains(&q) || m.description.to_lowercase().contains(&q)
            })
            .collect()
    }
}
