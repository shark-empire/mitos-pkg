use crate::config::Config;
use crate::database::files::FileDb;
use crate::database::packages::{InstalledDb, InstalledPackage};
use crate::dependency::resolver::Resolver;
use crate::error::{PkgError, Result};
use crate::install::transaction::Transaction;
use crate::package::{archive, format, signature as pkg_signature};
use crate::repository::download::{download_verified, Fetcher, HttpFetcher};
use crate::repository::index::RepositoryIndex;
use crate::repository::metadata::PackageMetadata;
use crate::security::keys::KeyStore;

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

    /// Resolves `name`'s dependency tree, downloads + verifies + installs
    /// each package that isn't already present, in dependency order. If
    /// any package in the chain fails to verify or install, packages
    /// already committed earlier in this call stay installed — mitos-pkg
    /// does not currently roll back an entire multi-package plan, only the
    /// single package transaction that failed (see `Transaction::install`).
    pub fn install(&mut self, name: &str) -> Result<()> {
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
        let dest = self
            .config
            .download_cache_path(&format::package_filename(&meta.name, &meta.version));
        if !dest.exists() {
            download_verified(fetcher, &meta.url, &meta.sha256, &meta.name, &dest)?;
        }

        let manifest = archive::read_manifest(&dest)?;
        pkg_signature::verify_package(&dest, &manifest, &self.keystore, meta.signature.as_deref())?;

        let mut tx = Transaction::new(
            &self.config.install_root,
            &mut self.packages,
            &mut self.files,
        );
        tx.install(&dest, &manifest, explicit)
    }

    /// Removes `name`, refusing if any other installed package still
    /// depends on it (use a future `remove --cascade` to lift that, not
    /// implemented yet on purpose — silent cascading removal is exactly
    /// the kind of surprise a package manager shouldn't spring on you).
    pub fn remove(&mut self, name: &str) -> Result<()> {
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

    pub fn list(&self) -> Vec<(&String, &InstalledPackage)> {
        let mut items: Vec<_> = self.packages.all().iter().collect();
        items.sort_by(|a, b| a.0.cmp(b.0));
        items
    }

    pub fn search(&self, query: &str) -> Vec<&PackageMetadata> {
        self.index.search(query)
    }
}
