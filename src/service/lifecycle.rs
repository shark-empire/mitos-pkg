use crate::config::Config;
use crate::database::files::FileDb;
use crate::database::packages::{InstalledDb, InstalledPackage};
use crate::dependency::resolver::Resolver;
use crate::dependency::version::Dependency;
use crate::error::{PkgError, Result};
use crate::install::transaction::Transaction;
use crate::package::{archive, format, manifest::Manifest, signature as pkg_signature};
use crate::repository::download::{download_verified, Fetcher, HttpFetcher};
use crate::repository::index::RepositoryIndex;
use crate::repository::metadata::PackageMetadata;
use crate::security::keys::KeyStore;
use crate::service::lock::Lock;
use semver::Version;
use std::path::PathBuf;

/// The single entry point `main` talks to. Everything below `main.rs`
/// (resolver, transaction, security, repository) is wired together here so
/// the CLI layer stays a thin dispatch table and every subsystem can be
/// exercised on its own in tests without a CLI in the loop.
pub struct PackageService {
    config: Config,
    packages: InstalledDb,
    files: FileDb,
    index: RepositoryIndex,
    keystore: KeyStore,
}

/// What `mitos-pkg info` shows. Populated from the local install DB when
/// the package is installed (the actual state of the system), falling
/// back to the repository index when it's merely available.
pub struct InfoView {
    pub name: String,
    pub version: Version,
    pub description: String,
    pub dependencies: Vec<Dependency>,
    pub provides: Vec<String>,
    pub conflicts: Vec<String>,
    pub installed: bool,
    pub explicit: Option<bool>,
}

impl PackageService {
    /// Opens all local state (install DB, file-ownership DB, cached repo
    /// index, trusted keys). Never touches the network — call `update()`
    /// explicitly to refresh the index.
    pub fn open(config: Config) -> Result<Self> {
        let packages = InstalledDb::load(&config.packages_db_path())?;
        let files = FileDb::load(&config.files_db_path())?;
        let index = RepositoryIndex::load(&config.index_cache_path()).unwrap_or_default();
        let keystore = KeyStore::load_dir(&config.trusted_keys_dir).unwrap_or_default();
        Ok(Self {
            config,
            packages,
            files,
            index,
            keystore,
        })
    }

    /// Refreshes the local index cache from every configured repository
    /// and merges them into one in-memory + on-disk index.
    pub fn update(&mut self) -> Result<()> {
        let fetcher = HttpFetcher;
        let mut merged = RepositoryIndex::default();

        for repo_url in &self.config.repositories {
            let data = fetcher.fetch(repo_url)?;
            let remote: RepositoryIndex = serde_json::from_slice(&data)?;
            for (name, versions) in remote.packages {
                merged.packages.entry(name).or_default().extend(versions);
            }
        }

        merged.save(&self.config.index_cache_path())?;
        self.index = merged;
        Ok(())
    }

    /// Downloads (if not already cached) and verifies one candidate
    /// against the full trust chain — archive checksum against the
    /// index's published `sha256`, then payload signature if the
    /// manifest names a signer — without touching the filesystem or
    /// either database. Split out from `install_one` so `upgrade_one` can
    /// verify a replacement archive *before* removing what it would
    /// replace.
    fn fetch_and_verify(
        &self,
        fetcher: &HttpFetcher,
        meta: &PackageMetadata,
    ) -> Result<(PathBuf, Manifest)> {
        let dest = self
            .config
            .download_cache_path(&format::package_filename(&meta.name, &meta.version));
        if !dest.exists() {
            download_verified(fetcher, &meta.url, &meta.sha256, &meta.name, &dest)?;
        }

        let manifest = archive::read_manifest(&dest)?;
        pkg_signature::verify_package(
            &dest,
            &meta.sha256,
            &manifest,
            &self.keystore,
            meta.signature.as_deref(),
        )?;

        Ok((dest, manifest))
    }

    /// Resolves `name`'s dependency tree, downloads + verifies + installs
    /// each package that isn't already present, in dependency order. If
    /// any package in the chain fails to verify or install, packages
    /// already committed earlier in this call stay installed — mitos-pkg
    /// does not currently roll back an entire multi-package plan, only the
    /// single package transaction that failed (see `Transaction::install`).
    pub fn install(&mut self, name: &str) -> Result<()> {
        let _lock = Lock::acquire(&self.config.db_dir)?;

        if let Some(existing) = self.packages.get(name) {
            return Err(PkgError::AlreadyInstalled(
                name.to_string(),
                existing.version.to_string(),
            ));
        }

        let plan = {
            let resolver = Resolver::new(&self.index, &self.packages);
            resolver.resolve_install(name)?
        };

        let fetcher = HttpFetcher;
        for meta in &plan.order {
            self.install_one(&fetcher, meta, meta.name == name)?;
        }
        Ok(())
    }

    fn install_one(
        &mut self,
        fetcher: &HttpFetcher,
        meta: &PackageMetadata,
        explicit: bool,
    ) -> Result<()> {
        let (dest, manifest) = self.fetch_and_verify(fetcher, meta)?;
        let mut tx = Transaction::new(
            &self.config.install_root,
            &mut self.packages,
            &mut self.files,
        );
        tx.install(&dest, &manifest, explicit)
    }

    /// Removes `name`, refusing if any other installed package still
    /// depends on it (use `autoremove` to clean up orphaned dependencies
    /// instead — cascading a direct `remove` silently is exactly the kind
    /// of surprise a package manager shouldn't spring on you).
    pub fn remove(&mut self, name: &str) -> Result<()> {
        let _lock = Lock::acquire(&self.config.db_dir)?;

        if !self.packages.is_installed(name) {
            return Err(PkgError::NotInstalled(name.to_string()));
        }

        let dependents = Resolver::new(&self.index, &self.packages).dependents_of(name);
        if !dependents.is_empty() {
            return Err(PkgError::RequiredByOthers(name.to_string(), dependents));
        }

        let mut tx = Transaction::new(
            &self.config.install_root,
            &mut self.packages,
            &mut self.files,
        );
        tx.remove(name)
    }

    /// Upgrades one named package, or every installed package if `name`
    /// is `None`, to the newest version currently in the repository
    /// index. Returns each package actually upgraded as
    /// `(name, old_version, new_version)`.
    ///
    /// Known limitation: this only checks that the *new* version's own
    /// dependencies are met (pulling in any that are missing) — it does
    /// not check whether upgrading breaks another installed package's
    /// version requirement on the one being upgraded. A future version of
    /// this could walk `Resolver::dependents_of` and re-validate their
    /// `Dependency::matches` before committing.
    pub fn upgrade(&mut self, name: Option<&str>) -> Result<Vec<(String, Version, Version)>> {
        let _lock = Lock::acquire(&self.config.db_dir)?;

        let targets: Vec<String> = match name {
            Some(n) => vec![n.to_string()],
            None => self.packages.all().keys().cloned().collect(),
        };

        let mut upgraded = Vec::new();
        for pkg_name in targets {
            let Some(installed) = self.packages.get(&pkg_name) else {
                if name.is_some() {
                    return Err(PkgError::NotInstalled(pkg_name));
                }
                continue;
            };
            let Some(latest) = self.index.latest(&pkg_name) else {
                continue;
            };
            if latest.version <= installed.version {
                continue;
            }

            let from = installed.version.clone();
            let to = latest.version.clone();
            let explicit = installed.explicit;

            self.upgrade_one(&pkg_name, explicit)?;
            upgraded.push((pkg_name, from, to));
        }

        Ok(upgraded)
    }

    fn upgrade_one(&mut self, name: &str, explicit: bool) -> Result<()> {
        let latest = self
            .index
            .latest(name)
            .cloned()
            .ok_or_else(|| PkgError::PackageNotFound(name.to_string()))?;

        let fetcher = HttpFetcher;
        // Fetch + verify the new archive *before* touching anything
        // currently installed, so a bad download never leaves the
        // package removed with nothing valid to replace it.
        let (dest, manifest) = self.fetch_and_verify(&fetcher, &latest)?;

        // Pull in any dependency the new version introduces that the old
        // one didn't have.
        let new_deps: Vec<String> = manifest.dependencies.iter().map(|d| d.name.clone()).collect();
        for dep_name in new_deps {
            if !self.packages.is_installed(&dep_name) {
                let plan = Resolver::new(&self.index, &self.packages).resolve_install(&dep_name)?;
                for meta in &plan.order {
                    self.install_one(&fetcher, meta, false)?;
                }
            }
        }

        // Swap: remove the old payload, install the freshly-verified new
        // one, carrying over whether it was explicitly requested.
        {
            let mut tx = Transaction::new(
                &self.config.install_root,
                &mut self.packages,
                &mut self.files,
            );
            tx.remove(name)?;
        }
        let mut tx = Transaction::new(
            &self.config.install_root,
            &mut self.packages,
            &mut self.files,
        );
        tx.install(&dest, &manifest, explicit)
    }

    /// Removes every installed package that isn't explicitly wanted and
    /// that nothing else installed still depends on — mitos-pkg's
    /// equivalent of `apt autoremove` / `pacman -Rns $(pacman -Qtdq)`.
    /// Runs to a fixed point: removing one orphan can turn another
    /// package into an orphan too (its only dependent just disappeared),
    /// so this keeps going until a full pass finds nothing left to
    /// remove.
    pub fn autoremove(&mut self) -> Result<Vec<String>> {
        let _lock = Lock::acquire(&self.config.db_dir)?;

        let mut removed = Vec::new();
        loop {
            let orphan = {
                let resolver = Resolver::new(&self.index, &self.packages);
                self.packages
                    .all()
                    .iter()
                    .find(|(name, pkg)| !pkg.explicit && resolver.dependents_of(name).is_empty())
                    .map(|(name, _)| name.clone())
            };

            let Some(name) = orphan else { break };

            let mut tx = Transaction::new(
                &self.config.install_root,
                &mut self.packages,
                &mut self.files,
            );
            tx.remove(&name)?;
            removed.push(name);
        }

        Ok(removed)
    }

    pub fn list(&self) -> Vec<(&String, &InstalledPackage)> {
        let mut items: Vec<_> = self.packages.all().iter().collect();
        items.sort_by(|a, b| a.0.cmp(b.0));
        items
    }

    pub fn search(&self, query: &str) -> Vec<&PackageMetadata> {
        self.index.search(query)
    }

    /// Shows what's known about a package: the installed record if it's
    /// installed (the actual state of the system), otherwise the
    /// repository index's entry if it's merely available.
    pub fn info(&self, name: &str) -> Result<InfoView> {
        if let Some(pkg) = self.packages.get(name) {
            return Ok(InfoView {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                description: pkg.description.clone(),
                dependencies: pkg.dependencies.clone(),
                provides: pkg.provides.clone(),
                conflicts: pkg.conflicts.clone(),
                installed: true,
                explicit: Some(pkg.explicit),
            });
        }

        let meta = self
            .index
            .latest(name)
            .ok_or_else(|| PkgError::PackageNotFound(name.to_string()))?;

        Ok(InfoView {
            name: meta.name.clone(),
            version: meta.version.clone(),
            description: meta.description.clone(),
            dependencies: meta.dependencies.clone(),
            provides: meta.provides.clone(),
            conflicts: meta.conflicts.clone(),
            installed: false,
            explicit: None,
        })
    }
}
