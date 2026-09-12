use crate::config::{Config, RepoSource};
use crate::database::files::FileDb;
use crate::database::history::{self, HistoryEntry};
use crate::database::packages::{InstalledDb, InstalledPackage};
use crate::dependency::resolver::Resolver;
use crate::dependency::version::{parse_pinned, Dependency};
use crate::error::{PkgError, Result};
use crate::install::diskspace;
use crate::install::transaction::Transaction;
use crate::package::{arch, archive, format, manifest::Manifest, signature as pkg_signature};
use crate::repository::download::{self, download_verified, HttpFetcher};
use crate::repository::index::RepositoryIndex;
use crate::repository::metadata::PackageMetadata;
use crate::security::checksum;
use crate::security::keys::KeyStore;
use crate::service::lock::Lock;
use crate::service::progress::{Progress, ProgressEvent};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The single entry point `main` (and `mitos-pkgd`) talks to. Everything
/// below this layer (resolver, transaction, security, repository) is
/// wired together here so the CLI/daemon layers stay thin dispatch and
/// every subsystem can be exercised on its own in tests without either
/// in the loop.
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
    pub arch: Vec<String>,
    pub essential: bool,
}

/// One problem `PackageService::verify` found comparing an installed
/// package's files against what was recorded at install time.
/// `path: None` means the problem is about the package's aggregate
/// checksum as a whole rather than one specific file (see `verify`'s
/// doc comment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyIssue {
    pub package: String,
    pub path: Option<PathBuf>,
    pub problem: String,
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

    /// The architecture every install/upgrade/resolve call checks
    /// packages against (see `package::arch`): `Config::target_arch` if
    /// set, otherwise whatever CPU architecture this process itself is
    /// running on.
    fn effective_arch(&self) -> &str {
        self.config
            .target_arch
            .as_deref()
            .unwrap_or_else(arch::host_arch)
    }

    /// Refreshes the local index cache from every configured repository
    /// and merges them into one in-memory + on-disk index.
    ///
    /// A repository configured with a `signer` (see `config::RepoSource`)
    /// must also serve `{url}.sig` — a detached signature over the raw
    /// index bytes — or its packages are rejected outright: an index that
    /// claims to be signed but can't produce a valid signature is treated
    /// the same as a tampered one, not silently downgraded to unsigned.
    /// Each configured mirror (`RepoSource::mirrors`) is tried in turn if
    /// an earlier one is unreachable, and every fetch retries transient
    /// failures (see `repository::download::fetch_with_retry`).
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

            let (data, sig) = Self::fetch_repo_index(&fetcher, repo)?;
            if let Some(signer) = repo.signer() {
                let sig_hex = sig.expect(
                    "fetch_repo_index only returns Ok with Some(signature) when repo.signer() is set",
                );
                pkg_signature::verify_index(&data, &sig_hex, signer, &self.keystore)?;
            }

            let remote: RepositoryIndex = serde_json::from_slice(&data)?;
            for (name, versions) in remote.packages {
                let mut versions = versions;
                for v in &mut versions {
                    v.priority = repo.priority();
                }
                merged.packages.entry(name).or_default().extend(versions);
            }
        }

        merged.save(&self.config.index_cache_path())?;
        self.index = merged;
        Ok(())
    }

    /// Tries every URL configured for `repo` (primary, then mirrors, in
    /// order) until one serves the index — and, if `repo` requires a
    /// signature, a valid-looking `{url}.sig` alongside it. A mirror that
    /// serves the index but not a fetchable signature is treated as a
    /// miss and the next mirror is tried, rather than silently trusting
    /// an unsigned copy just because it happened to answer first.
    fn fetch_repo_index(
        fetcher: &HttpFetcher,
        repo: &RepoSource,
    ) -> Result<(Vec<u8>, Option<String>)> {
        let mut last_err = None;
        for url in repo.urls() {
            match download::fetch_with_retry(fetcher, url) {
                Ok(data) => {
                    let sig = if repo.signer().is_some() {
                        let sig_url = format!("{url}.sig");
                        download::fetch_with_retry(fetcher, &sig_url)
                            .ok()
                            .map(|b| String::from_utf8_lossy(&b).trim().to_string())
                    } else {
                        None
                    };
                    if repo.signer().is_some() && sig.is_none() {
                        last_err = Some(PkgError::InvalidSignature(format!(
                            "repository requires a signature from '{}' but {url}.sig could not be fetched",
                            repo.signer().unwrap_or_default()
                        )));
                        continue;
                    }
                    return Ok((data, sig));
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            PkgError::Network(format!("no reachable URL for repository '{}'", repo.url()))
        }))
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
    /// exact version (see `dependency::version::parse_pinned`) — or a
    /// filesystem path to an existing `.mpkg` file, in which case this is
    /// exactly `install_local(path, None, dry_run)` (no way to pass a
    /// `--signature` through this entry point; call `install_local`
    /// directly for a signed local package).
    ///
    /// If `dry_run` is set, resolves and returns the plan without
    /// downloading, verifying, or installing anything.
    ///
    /// If any package in the chain fails to verify or install after
    /// others in the same call already committed, everything this call
    /// installed is rolled back before the error is returned — `install`
    /// is all-or-nothing for the whole plan, not just for each package's
    /// own transaction.
    ///
    /// `signature_hex` is only used when `spec` turns out to be a local
    /// file path (see `install_local`); it's ignored for a repository-
    /// resolved install, which verifies against the index's published
    /// signature instead.
    pub fn install(
        &mut self,
        spec: &str,
        signature_hex: Option<&str>,
        dry_run: bool,
    ) -> Result<Vec<(String, Version)>> {
        self.install_with_progress(spec, signature_hex, dry_run, &mut Progress::none())
    }

    /// Same as `install`, reporting each resolve/fetch/verify/install step
    /// as it happens.
    pub fn install_with_progress(
        &mut self,
        spec: &str,
        signature_hex: Option<&str>,
        dry_run: bool,
        progress: &mut Progress,
    ) -> Result<Vec<(String, Version)>> {
        let path = Path::new(spec);
        if path.extension().and_then(|e| e.to_str()) == Some(format::PACKAGE_EXTENSION) && path.is_file() {
            return self.install_local_with_progress(path, signature_hex, dry_run, progress);
        }

        let _lock = Lock::acquire(&self.config.db_dir)?;
        let (name, req) = parse_pinned(spec)?;

        if let Some(existing) = self.packages.get(&name) {
            return Err(PkgError::AlreadyInstalled(name, existing.version.to_string()));
        }

        progress.emit(ProgressEvent::Resolving {
            spec: spec.to_string(),
        });
        let target_arch = self.effective_arch().to_string();
        let plan = {
            let resolver = Resolver::new(&self.index, &self.packages, &target_arch);
            resolver.resolve_install_req(&name, &req)?
        };

        if dry_run {
            return Ok(plan
                .order
                .iter()
                .map(|m| (m.name.clone(), m.version.clone()))
                .collect());
        }

        let needed: u64 = plan.order.iter().map(|m| m.installed_size_bytes).sum();
        diskspace::check(&self.config.install_root, needed)?;

        let fetcher = HttpFetcher;
        let mut installed_this_call: Vec<String> = Vec::new();
        for meta in &plan.order {
            if let Err(e) = self.install_one(&fetcher, meta, meta.name == name, progress) {
                // Whole-plan rollback: undo everything this call already
                // committed, in reverse order, before surfacing the
                // failure — `install` is all-or-nothing for the plan as a
                // whole, not just for each package's own transaction.
                for already in installed_this_call.iter().rev() {
                    if let Err(undo_err) = self.remove_one(already) {
                        eprintln!(
                            "mitos-pkg: warning: failed to roll back '{already}' after install failure: {undo_err}"
                        );
                    }
                }
                return Err(e);
            }
            installed_this_call.push(meta.name.clone());
        }

        for meta in &plan.order {
            history::append(
                &self.config.history_path(),
                &HistoryEntry::new("install", &meta.name)
                    .with_versions(None, Some(meta.version.to_string())),
            );
        }

        Ok(plan
            .order
            .iter()
            .map(|m| (m.name.clone(), m.version.clone()))
            .collect())
    }

    /// Installs a single already-built `.mpkg` file directly, without
    /// resolving anything against a configured repository — mitos-pkg's
    /// equivalent of `dpkg -i`/`rpm -i`/`pacman -U`. Dependencies are
    /// *not* pulled in automatically (there is no repository context to
    /// resolve them against); an unmet dependency is left for the caller
    /// to install separately.
    ///
    /// A local file has no repository-published archive checksum to
    /// check against (that only exists once something is published in an
    /// index) — the trust question here is entirely "does this
    /// manifest's own signature check out" against `signature_hex`, not
    /// "does this match what a repo claims it should be". A package with
    /// no declared `signer` installs unsigned either way, the same
    /// opt-in-by-default posture `Manifest::signer` has everywhere else.
    pub fn install_local(
        &mut self,
        archive_path: &Path,
        signature_hex: Option<&str>,
        dry_run: bool,
    ) -> Result<Vec<(String, Version)>> {
        self.install_local_with_progress(archive_path, signature_hex, dry_run, &mut Progress::none())
    }

    /// Same as `install_local`, reporting verify/install steps as they
    /// happen.
    pub fn install_local_with_progress(
        &mut self,
        archive_path: &Path,
        signature_hex: Option<&str>,
        dry_run: bool,
        progress: &mut Progress,
    ) -> Result<Vec<(String, Version)>> {
        let _lock = Lock::acquire(&self.config.db_dir)?;

        let manifest = archive::read_manifest(archive_path)?;

        if let Some(existing) = self.packages.get(&manifest.name) {
            return Err(PkgError::AlreadyInstalled(
                manifest.name.clone(),
                existing.version.to_string(),
            ));
        }

        let target_arch = self.effective_arch().to_string();
        if !arch::is_compatible(&manifest.arch, &target_arch) {
            return Err(PkgError::ArchMismatch {
                name: manifest.name.clone(),
                package_arch: manifest.arch.clone(),
                host_arch: target_arch,
            });
        }

        progress.emit(ProgressEvent::Verifying {
            name: manifest.name.clone(),
            version: manifest.version.to_string(),
        });
        if let Some(signer) = &manifest.signer {
            let sig_hex = signature_hex.ok_or_else(|| {
                PkgError::InvalidSignature(format!(
                    "'{}' is signed by '{signer}' — pass its signature to verify, or install from a configured repository instead",
                    manifest.name
                ))
            })?;
            let public_key = self
                .keystore
                .find(signer)
                .ok_or_else(|| PkgError::UntrustedKey(signer.clone()))?;
            let sig_bytes = pkg_signature::decode_signature(sig_hex, signer)?;
            crate::security::signature::verify_signature(
                public_key,
                manifest.payload_sha256.as_bytes(),
                &sig_bytes,
                signer,
            )?;
        }

        if dry_run {
            return Ok(vec![(manifest.name.clone(), manifest.version.clone())]);
        }

        diskspace::check(&self.config.install_root, manifest.installed_size_bytes)?;

        progress.emit(ProgressEvent::Installing {
            name: manifest.name.clone(),
            version: manifest.version.to_string(),
        });
        {
            let mut tx = Transaction::new(
                &self.config.install_root,
                &mut self.packages,
                &mut self.files,
            );
            tx.install(archive_path, &manifest, true, false)?;
        }

        history::append(
            &self.config.history_path(),
            &HistoryEntry::new("install-local", &manifest.name)
                .with_versions(None, Some(manifest.version.to_string())),
        );

        Ok(vec![(manifest.name.clone(), manifest.version.clone())])
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
    /// or pacman's `-Rs`. Also refuses to remove a package marked
    /// `essential` (see `Manifest::essential`) unless `force` is set.
    /// `dry_run` returns exactly what would be removed (and still
    /// surfaces the dependents/essential errors a real run would) without
    /// touching anything. Returns every package name actually removed, in
    /// the order they were removed.
    pub fn remove(
        &mut self,
        name: &str,
        cascade: bool,
        force: bool,
        dry_run: bool,
    ) -> Result<Vec<String>> {
        self.remove_with_progress(name, cascade, force, dry_run, &mut Progress::none())
    }

    /// Same as `remove`, reporting a `Removing` event for each package
    /// right before it's actually removed.
    pub fn remove_with_progress(
        &mut self,
        name: &str,
        cascade: bool,
        force: bool,
        dry_run: bool,
        progress: &mut Progress,
    ) -> Result<Vec<String>> {
        let _lock = Lock::acquire(&self.config.db_dir)?;

        if !self.packages.is_installed(name) {
            return Err(PkgError::NotInstalled(name.to_string()));
        }

        let mut to_remove = Vec::new();
        if cascade {
            self.plan_cascade(name, &mut to_remove);
        } else {
            let target_arch = self.effective_arch().to_string();
            let dependents =
                Resolver::new(&self.index, &self.packages, &target_arch).dependents_of(name);
            if !dependents.is_empty() {
                return Err(PkgError::RequiredByOthers(name.to_string(), dependents));
            }
            to_remove.push(name.to_string());
        }

        if !force {
            for n in &to_remove {
                if let Some(pkg) = self.packages.get(n) {
                    if pkg.essential {
                        return Err(PkgError::EssentialPackage(n.clone()));
                    }
                }
            }
        }

        if dry_run {
            return Ok(to_remove);
        }

        let mut removed = Vec::new();
        for n in &to_remove {
            if self.packages.is_installed(n) {
                progress.emit(ProgressEvent::Removing { name: n.clone() });
                self.remove_one(n)?;
                history::append(&self.config.history_path(), &HistoryEntry::new("remove", n));
                removed.push(n.clone());
            }
        }
        Ok(removed)
    }

    /// Computes the full cascade-removal set for `name` (every installed
    /// dependent, deepest first, then `name` itself) without removing
    /// anything — shared by the real removal path and `dry_run`/essential
    /// pre-checks so both see exactly the same plan.
    fn plan_cascade(&self, name: &str, out: &mut Vec<String>) {
        let target_arch = self.effective_arch().to_string();
        let dependents =
            Resolver::new(&self.index, &self.packages, &target_arch).dependents_of(name);
        for dependent in dependents {
            // A shared dependent may already be queued from another
            // branch of the same cascade (e.g. two packages both pulled
            // in the same thing that itself depended on `name`).
            if self.packages.is_installed(&dependent) && !out.contains(&dependent) {
                self.plan_cascade(&dependent, out);
            }
        }
        if !out.contains(&name.to_string()) {
            out.push(name.to_string());
        }
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
    /// is `None`, to the newest version currently in the repository index
    /// that's installable on this host's architecture. Packages pinned
    /// with `hold` are skipped when upgrading everything; naming a held
    /// package directly is refused unless `ignore_hold` is set (mirrors
    /// `apt-get install <held-pkg>` needing `--allow-change-held-packages`).
    /// `dry_run` returns exactly what would be upgraded, still checked
    /// for hold/safety, without changing anything. Returns each package
    /// actually upgraded as `(name, old_version, new_version)`.
    pub fn upgrade(
        &mut self,
        name: Option<&str>,
        ignore_hold: bool,
        dry_run: bool,
    ) -> Result<Vec<(String, Version, Version)>> {
        self.upgrade_with_progress(name, ignore_hold, dry_run, &mut Progress::none())
    }

    /// Same as `upgrade`, reporting each affected package's
    /// fetch/verify/install steps as it happens.
    pub fn upgrade_with_progress(
        &mut self,
        name: Option<&str>,
        ignore_hold: bool,
        dry_run: bool,
        progress: &mut Progress,
    ) -> Result<Vec<(String, Version, Version)>> {
        let _lock = Lock::acquire(&self.config.db_dir)?;
        let target_arch = self.effective_arch().to_string();

        let targets: Vec<String> = match name {
            Some(n) => vec![n.to_string()],
            None => self.packages.all().keys().cloned().collect(),
        };

        let mut planned: Vec<(String, Version, Version)> = Vec::new();
        for pkg_name in &targets {
            let Some(installed) = self.packages.get(pkg_name) else {
                if name.is_some() {
                    return Err(PkgError::NotInstalled(pkg_name.clone()));
                }
                continue;
            };
            if installed.held && !ignore_hold {
                if name.is_some() {
                    return Err(PkgError::PackageHeld(pkg_name.clone()));
                }
                continue;
            }
            let Some(latest) = self.index.latest_for_arch(pkg_name, &target_arch) else {
                continue;
            };
            if latest.version <= installed.version {
                continue;
            }

            self.check_upgrade_safe(pkg_name, &latest.version)?;
            planned.push((pkg_name.clone(), installed.version.clone(), latest.version.clone()));
        }

        if dry_run || planned.is_empty() {
            return Ok(planned);
        }

        let mut upgraded = Vec::new();
        for (pkg_name, from, _to) in &planned {
            let explicit = self
                .packages
                .get(pkg_name)
                .map(|p| p.explicit)
                .unwrap_or(true);
            let held = self.packages.get(pkg_name).map(|p| p.held).unwrap_or(false);

            self.upgrade_one(pkg_name, explicit, held, progress)?;

            let new_version = self
                .packages
                .get(pkg_name)
                .map(|p| p.version.clone())
                .unwrap_or_else(|| from.clone());
            history::append(
                &self.config.history_path(),
                &HistoryEntry::new("upgrade", pkg_name)
                    .with_versions(Some(from.to_string()), Some(new_version.to_string())),
            );
            upgraded.push((pkg_name.clone(), from.clone(), new_version));
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
        let target_arch = self.effective_arch().to_string();
        let latest = self
            .index
            .latest_for_arch(name, &target_arch)
            .cloned()
            .ok_or_else(|| PkgError::PackageNotFound(name.to_string()))?;

        let fetcher = HttpFetcher;
        // Fetch + verify the new archive *before* touching anything
        // currently installed, so a bad download never leaves the
        // package removed with nothing valid to replace it.
        let (dest, manifest) = self.fetch_and_verify(&fetcher, &latest, progress)?;

        diskspace::check(&self.config.install_root, manifest.installed_size_bytes)?;

        // Pull in any dependency the new version introduces that the old
        // one didn't have.
        let new_deps: Vec<String> = manifest
            .dependencies
            .iter()
            .map(|d| d.name.clone())
            .collect();
        for dep_name in new_deps {
            if !self.packages.is_installed(&dep_name) {
                let plan = {
                    let resolver = Resolver::new(&self.index, &self.packages, &target_arch);
                    resolver.resolve_install(&dep_name)?
                };
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

    /// Removes every installed package that isn't explicitly wanted, that
    /// nothing else installed still depends on, and that isn't marked
    /// `essential` — mitos-pkg's equivalent of `apt autoremove` / `pacman
    /// -Rns $(pacman -Qtdq)`. An essential orphan (unusual, but possible
    /// if something depended on it only transiently) is left alone rather
    /// than erroring the whole pass, the same way `--force` isn't implied
    /// just because a removal is automatic.
    ///
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
        let target_arch = self.effective_arch().to_string();

        let mut removed = Vec::new();
        loop {
            let orphan = {
                let resolver = Resolver::new(&self.index, &self.packages, &target_arch);
                self.packages
                    .all()
                    .iter()
                    .find(|(name, pkg)| {
                        !pkg.explicit && !pkg.essential && resolver.dependents_of(name).is_empty()
                    })
                    .map(|(name, _)| name.clone())
            };

            let Some(name) = orphan else { break };

            progress.emit(ProgressEvent::Removing { name: name.clone() });
            self.remove_one(&name)?;
            history::append(&self.config.history_path(), &HistoryEntry::new("autoremove", &name));
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
                arch: pkg.arch.clone(),
                essential: pkg.essential,
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
            arch: meta.arch.clone(),
            essential: meta.essential,
        })
    }

    /// Re-derives each checked package's installed files' aggregate
    /// SHA-256 (the same computation `install::transaction` does right
    /// after extraction) and compares it against what was recorded at
    /// install time, reporting any drift — mitos-pkg's equivalent of
    /// `debsums`/`rpm -Va`. `name` limits the check to one package;
    /// `None` checks everything installed.
    ///
    /// Checks each file's existence individually before folding it into
    /// the aggregate: a missing or unreadable file is reported as its own
    /// issue (so the caller knows exactly *which* file), and only once
    /// every file was readable is the aggregate hash compared — folding a
    /// missing file straight into an opaque "checksum mismatch" would
    /// tell the caller strictly less for no benefit.
    pub fn verify(&self, name: Option<&str>) -> Result<Vec<VerifyIssue>> {
        let targets: Vec<&InstalledPackage> = match name {
            Some(n) => vec![self
                .packages
                .get(n)
                .ok_or_else(|| PkgError::NotInstalled(n.to_string()))?],
            None => self.packages.all().values().collect(),
        };

        let mut issues = Vec::new();
        for pkg in targets {
            let mut entries: Vec<(PathBuf, String)> = Vec::with_capacity(pkg.installed_files.len());
            for rel in &pkg.installed_files {
                let full = self.config.install_root.join(rel);
                if !full.exists() {
                    issues.push(VerifyIssue {
                        package: pkg.name.clone(),
                        path: Some(rel.clone()),
                        problem: "missing".to_string(),
                    });
                    continue;
                }
                match checksum::hash_file(&full) {
                    Ok(hash) => entries.push((rel.clone(), hash)),
                    Err(e) => issues.push(VerifyIssue {
                        package: pkg.name.clone(),
                        path: Some(rel.clone()),
                        problem: format!("unreadable: {e}"),
                    }),
                }
            }

            // A package installed before `payload_sha256` was recorded
            // (see `InstalledPackage::payload_sha256`) has nothing to
            // compare the aggregate against — the per-file checks above
            // still ran and are still reported, this just skips the
            // whole-package comparison rather than reporting every such
            // package as corrupt.
            if pkg.payload_sha256.is_empty() {
                continue;
            }

            if entries.len() == pkg.installed_files.len() {
                let aggregate = checksum::aggregate_hash(&entries);
                if !aggregate.eq_ignore_ascii_case(&pkg.payload_sha256) {
                    issues.push(VerifyIssue {
                        package: pkg.name.clone(),
                        path: None,
                        problem: "one or more files modified since install (checksum mismatch)"
                            .to_string(),
                    });
                }
            }
        }

        Ok(issues)
    }

    /// The most recent `limit` completed operations (install, remove,
    /// upgrade, autoremove) across this system's whole history —
    /// mitos-pkg's equivalent of `apt history`/`dnf history`. See
    /// `database::history` for the on-disk format.
    pub fn history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        history::read_recent(&self.config.history_path(), limit)
    }
}
