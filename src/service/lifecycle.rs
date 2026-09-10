use crate::config::Config;
use crate::database::files::FileDb;
use crate::database::packages::{InstalledDb, InstalledPackage};
use crate::dependency::resolver::Resolver;
use crate::dependency::version::{parse_pinned, Dependency};
use crate::error::{PkgError, Result};
use crate::install::transaction::Transaction;
use crate::package::{archive, format, manifest::Manifest, signature as pkg_signature};
use crate::repository::download::{download_verified, Fetcher, HttpFetcher};
use crate::repository::index::RepositoryIndex;
use crate::repository::metadata::PackageMetadata;
use crate::security::keys::KeyStore;
use crate::service::lock::Lock;
use crate::service::progress::{Progress, ProgressEvent};
use semver::Version;
use serde::{Deserialize, Serialize};
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
///
/// `Serialize`/`Deserialize` so `mitos-pkgd` can send this straight back
/// to a client over the wire as-is, instead of a hand-mirrored copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoView {
    pub name: String,
    pub version: Version,
    pub description: String,
    pub dependencies: Vec<Dependency>,
    pub provides: Vec<String>,
    pub conflicts: Vec<String>,
    pub installed: bool,
    pub explicit: Option<bool>,
    pub held: Option<bool>,
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
    ///
    /// A repository configured with a `signer` (see `config::RepoSource`)
    /// must also serve `{url}.sig` — a detached signature over the raw
    /// index bytes — or its packages are rejected outright: an index that
    /// claims to be signed but can't produce a valid signature is treated
    /// the same as a tampered one, not silently downgraded to unsigned.
    pub fn update(&mut self) -> Result<()> {
        self.update_with_progress(&mut Progress::none())
    }

    /// Same as `update`, reporting an `Updating` event before fetching
    /// each configured repository's index.
    pub fn update_with_progress(&mut self, progress: &mut Progress) -> Result<()> {
        let fetcher = HttpFetcher;
        let mut merged = RepositoryIndex::default();

        for repo in &self.config.repositories {
            progress.emit(ProgressEvent::Updating {
                repository: repo.url().to_string(),
            });
            let data = fetcher.fetch(repo.url())?;

            if let Some(signer) = repo.signer() {
                let sig_url = format!("{}.sig", repo.url());
                let sig_bytes = fetcher.fetch(&sig_url).map_err(|_| {
                    PkgError::InvalidSignature(format!(
                        "repository '{}' requires a signature from '{signer}' but its signature at {sig_url} could not be fetched",
                        repo.url()
                    ))
                })?;
                let sig_hex = String::from_utf8_lossy(&sig_bytes);
                pkg_signature::verify_index(&data, sig_hex.trim(), signer, &self.keystore)?;
            }

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
        progress: &mut Progress,
    ) -> Result<(PathBuf, Manifest)> {
        let dest = self
            .config
            .download_cache_path(&format::package_filename(&meta.name, &meta.version));
        if !dest.exists() {
            progress.emit(ProgressEvent::Fetching {
                name: meta.name.clone(),
                version: meta.version.to_string(),
            });
            download_verified(fetcher, &meta.url, &meta.sha256, &meta.name, &dest)?;
        }

        progress.emit(ProgressEvent::Verifying {
            name: meta.name.clone(),
            version: meta.version.to_string(),
        });
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

    /// Resolves `spec`'s dependency tree, downloads + verifies + installs
    /// each package that isn't already present, in dependency order.
    /// `spec` is either a bare package name or `name@version` to pin an
    /// exact version (see `dependency::version::parse_pinned`). If any
    /// package in the chain fails to verify or install, packages already
    /// committed earlier in this call stay installed — mitos-pkg does not
    /// currently roll back an entire multi-package plan, only the single
    /// package transaction that failed (see `Transaction::install`).
    pub fn install(&mut self, spec: &str) -> Result<()> {
        self.install_with_progress(spec, &mut Progress::none())
    }

    /// Same as `install`, reporting each resolve/fetch/verify/install step
    /// as it happens.
    pub fn install_with_progress(&mut self, spec: &str, progress: &mut Progress) -> Result<()> {
        let _lock = Lock::acquire(&self.config.db_dir)?;
        let (name, req) = parse_pinned(spec)?;

        if let Some(existing) = self.packages.get(&name) {
            return Err(PkgError::AlreadyInstalled(name, existing.version.to_string()));
        }

        progress.emit(ProgressEvent::Resolving {
            spec: spec.to_string(),
        });
        let plan = {
            let resolver = Resolver::new(&self.index, &self.packages);
            resolver.resolve_install_req(&name, &req)?
        };

        let fetcher = HttpFetcher;
        for meta in &plan.order {
            self.install_one(&fetcher, meta, meta.name == name, progress)?;
        }
        Ok(())
    }

    fn install_one(
        &mut self,
        fetcher: &HttpFetcher,
        meta: &PackageMetadata,
        explicit: bool,
        progress: &mut Progress,
    ) -> Result<()> {
        let (dest, manifest) = self.fetch_and_verify(fetcher, meta, progress)?;
        progress.emit(ProgressEvent::Installing {
            name: meta.name.clone(),
            version: meta.version.to_string(),
        });
        let mut tx = Transaction::new(
            &self.config.install_root,
            &mut self.packages,
            &mut self.files,
        );
        // A freshly-pulled-in package is never held — there's nothing to
        // carry forward yet (see `upgrade_one` for the one path that
        // preserves an existing hold).
        tx.install(&dest, &manifest, explicit, false)
    }

    /// Removes `name`. Refuses if any other installed package still
    /// depends on it, unless `cascade` is set, in which case every such
    /// dependent is removed first (deepest first), then `name` — mitos-
    /// pkg's equivalent of `apt remove` needing `--auto-remove` chained,
    /// or pacman's `-Rs`. Returns every package name actually removed,
    /// in the order they were removed.
    pub fn remove(&mut self, name: &str, cascade: bool) -> Result<Vec<String>> {
        self.remove_with_progress(name, cascade, &mut Progress::none())
    }

    /// Same as `remove`, reporting a `Removing` event for each package
    /// right before it's actually removed.
    pub fn remove_with_progress(
        &mut self,
        name: &str,
        cascade: bool,
        progress: &mut Progress,
    ) -> Result<Vec<String>> {
        let _lock = Lock::acquire(&self.config.db_dir)?;

        if !self.packages.is_installed(name) {
            return Err(PkgError::NotInstalled(name.to_string()));
        }

        let mut removed = Vec::new();
        if cascade {
            self.remove_cascade(name, &mut removed, progress)?;
        } else {
            let dependents = Resolver::new(&self.index, &self.packages).dependents_of(name);
            if !dependents.is_empty() {
                return Err(PkgError::RequiredByOthers(name.to_string(), dependents));
            }
            progress.emit(ProgressEvent::Removing {
                name: name.to_string(),
            });
            self.remove_one(name)?;
            removed.push(name.to_string());
        }
        Ok(removed)
    }

    fn remove_cascade(
        &mut self,
        name: &str,
        removed: &mut Vec<String>,
        progress: &mut Progress,
    ) -> Result<()> {
        let dependents = Resolver::new(&self.index, &self.packages).dependents_of(name);
        for dependent in dependents {
            // A shared dependent may already have been removed earlier in
            // this same cascade (e.g. two packages both pulled in the
            // same thing that itself depended on `name`).
            if self.packages.is_installed(&dependent) {
                self.remove_cascade(&dependent, removed, progress)?;
            }
        }
        progress.emit(ProgressEvent::Removing {
            name: name.to_string(),
        });
        self.remove_one(name)?;
        removed.push(name.to_string());
        Ok(())
    }

    fn remove_one(&mut self, name: &str) -> Result<()> {
        let mut tx = Transaction::new(
            &self.config.install_root,
            &mut self.packages,
            &mut self.files,
        );
        tx.remove(name)
    }

    /// Refuses an upgrade that would leave another installed package's
    /// declared dependency on `name` unsatisfied. Closes a previously
    /// documented gap: only the upgraded package's *own* new dependencies
    /// used to be checked, never whether existing dependents could
    /// tolerate the version bump.
    fn check_upgrade_safe(&self, name: &str, new_version: &Version) -> Result<()> {
        for (dependent_name, dependent) in self.packages.all() {
            if dependent_name == name {
                continue;
            }
            for dep in &dependent.dependencies {
                let names_match = dep.name == name
                    || self
                        .packages
                        .get(name)
                        .map(|p| p.provides.iter().any(|provided| provided == &dep.name))
                        .unwrap_or(false);
                if names_match && !dep.matches(new_version) {
                    return Err(PkgError::DependencyConflict(format!(
                        "upgrading '{name}' to {new_version} would break '{dependent_name}', which requires '{} {}'",
                        dep.name, dep.version_req
                    )));
                }
            }
        }
        Ok(())
    }

    /// Upgrades one named package, or every installed package if `name`
    /// is `None`, to the newest version currently in the repository
    /// index. Packages pinned with `hold` are skipped when upgrading
    /// everything; naming a held package directly is refused unless
    /// `ignore_hold` is set (mirrors `apt-get install <held-pkg>` needing
    /// `--allow-change-held-packages`). Returns each package actually
    /// upgraded as `(name, old_version, new_version)`.
    pub fn upgrade(
        &mut self,
        name: Option<&str>,
        ignore_hold: bool,
    ) -> Result<Vec<(String, Version, Version)>> {
        self.upgrade_with_progress(name, ignore_hold, &mut Progress::none())
    }

    /// Same as `upgrade`, reporting each affected package's
    /// fetch/verify/install steps as it happens.
    pub fn upgrade_with_progress(
        &mut self,
        name: Option<&str>,
        ignore_hold: bool,
        progress: &mut Progress,
    ) -> Result<Vec<(String, Version, Version)>> {
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
            if installed.held && !ignore_hold {
                if name.is_some() {
                    return Err(PkgError::PackageHeld(pkg_name));
                }
                continue;
            }
            let Some(latest) = self.index.latest(&pkg_name) else {
                continue;
            };
            if latest.version <= installed.version {
                continue;
            }

            let from = installed.version.clone();
            let to = latest.version.clone();
            let explicit = installed.explicit;
            let held = installed.held;

            self.check_upgrade_safe(&pkg_name, &to)?;
            self.upgrade_one(&pkg_name, explicit, held, progress)?;
            upgraded.push((pkg_name, from, to));
        }

        Ok(upgraded)
    }

    fn upgrade_one(
        &mut self,
        name: &str,
        explicit: bool,
        held: bool,
        progress: &mut Progress,
    ) -> Result<()> {
        let latest = self
            .index
            .latest(name)
            .cloned()
            .ok_or_else(|| PkgError::PackageNotFound(name.to_string()))?;

        let fetcher = HttpFetcher;
        // Fetch + verify the new archive *before* touching anything
        // currently installed, so a bad download never leaves the
        // package removed with nothing valid to replace it.
        let (dest, manifest) = self.fetch_and_verify(&fetcher, &latest, progress)?;

        // Pull in any dependency the new version introduces that the old
        // one didn't have.
        let new_deps: Vec<String> = manifest
            .dependencies
            .iter()
            .map(|d| d.name.clone())
            .collect();
        for dep_name in new_deps {
            if !self.packages.is_installed(&dep_name) {
                let plan = Resolver::new(&self.index, &self.packages).resolve_install(&dep_name)?;
                for meta in &plan.order {
                    self.install_one(&fetcher, meta, false, progress)?;
                }
            }
        }

        // Swap: remove the old payload, install the freshly-verified new
        // one, carrying over whether it was explicitly requested and
        // whether it was held (a forced upgrade of a held package leaves
        // it held, the same way apt doesn't auto-unhold on override).
        progress.emit(ProgressEvent::Installing {
            name: name.to_string(),
            version: latest.version.to_string(),
        });
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
        tx.install(&dest, &manifest, explicit, held)
    }

    /// Pins `name` so `upgrade` (without `--ignore-hold`) skips it —
    /// mirrors `apt-mark hold` / `dnf versionlock add` / pacman's
    /// `IgnorePkg`.
    pub fn hold(&mut self, name: &str) -> Result<()> {
        let _lock = Lock::acquire(&self.config.db_dir)?;
        let pkg = self
            .packages
            .get_mut(name)
            .ok_or_else(|| PkgError::NotInstalled(name.to_string()))?;
        pkg.held = true;
        self.packages.save()
    }

    /// Reverses `hold`, letting `upgrade` touch `name` again.
    pub fn unhold(&mut self, name: &str) -> Result<()> {
        let _lock = Lock::acquire(&self.config.db_dir)?;
        let pkg = self
            .packages
            .get_mut(name)
            .ok_or_else(|| PkgError::NotInstalled(name.to_string()))?;
        pkg.held = false;
        self.packages.save()
    }

    /// Removes every installed package that isn't explicitly wanted and
    /// that nothing else installed still depends on — mitos-pkg's
    /// equivalent of `apt autoremove` / `pacman -Rns $(pacman -Qtdq)`.
    /// Runs to a fixed point: removing one orphan can turn another
    /// package into an orphan too (its only dependent just disappeared),
    /// so this keeps going until a full pass finds nothing left to
    /// remove.
    pub fn autoremove(&mut self) -> Result<Vec<String>> {
        self.autoremove_with_progress(&mut Progress::none())
    }

    /// Same as `autoremove`, reporting a `Removing` event for each orphan
    /// as it's found and removed.
    pub fn autoremove_with_progress(&mut self, progress: &mut Progress) -> Result<Vec<String>> {
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

            progress.emit(ProgressEvent::Removing { name: name.clone() });
            self.remove_one(&name)?;
            removed.push(name);
        }

        Ok(removed)
    }

    /// Deletes every cached downloaded archive under the configured cache
    /// directory (the index cache is left alone — `update` manages that)
    /// — mitos-pkg's equivalent of `apt clean` / `pacman -Sc`. Safe to
    /// run any time: anything actually needed again is re-downloaded and
    /// re-verified on demand. Returns the number of bytes freed.
    pub fn clean(&self) -> Result<u64> {
        let downloads_dir = self.config.cache_dir.join("downloads");
        if !downloads_dir.exists() {
            return Ok(0);
        }

        let mut freed = 0u64;
        for entry in std::fs::read_dir(&downloads_dir)? {
            let entry = entry?;
            let path = entry.path();
            freed += entry.metadata()?.len();
            if path.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        }
        Ok(freed)
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
                held: Some(pkg.held),
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
            held: None,
        })
    }
}
