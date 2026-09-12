use crate::database::files::FileDb;
use crate::database::packages::{InstalledDb, InstalledPackage};
use crate::error::{PkgError, Result};
use crate::install::hooks::{self, HookPoint};
use crate::install::{extractor, rollback};
use crate::package::manifest::Manifest;
use crate::security::checksum;
use std::path::{Path, PathBuf};

/// A single install or remove, applied so that on any failure the
/// filesystem and the two on-disk databases (`InstalledDb`, `FileDb`) are
/// left exactly as they were before the transaction started — never
/// half-applied.
pub struct Transaction<'a> {
    install_root: &'a Path,
    packages: &'a mut InstalledDb,
    files: &'a mut FileDb,
}

impl<'a> Transaction<'a> {
    pub fn new(
        install_root: &'a Path,
        packages: &'a mut InstalledDb,
        files: &'a mut FileDb,
    ) -> Self {
        Self {
            install_root,
            packages,
            files,
        }
    }

    /// Installs one already-verified package archive. `manifest` must have
    /// already passed `package::signature::verify_package`; this method
    /// does no trust checks of its own, only filesystem + database
    /// bookkeeping (plus running any `Manifest::hooks` the package
    /// declares — see module docs on why that's safe to do without a
    /// separate trust mechanism). `held` should be `false` for a fresh
    /// install; the one caller that ever passes `true` is
    /// `PackageService::upgrade_one` force-upgrading a held package,
    /// which carries the hold forward rather than silently clearing it
    /// (matching `apt`: overriding a hold once doesn't un-pin the
    /// package for next time).
    ///
    /// Extraction happens into a scratch **staging** directory under
    /// `install_root` first, not straight into `install_root` — three
    /// things fall out of that: `pre_install` has real files to run
    /// against before anything real is touched (see
    /// `package::hooks::Hooks`), a checksum/conflict problem is caught
    /// before the live system is touched at all rather than requiring a
    /// file-by-file undo afterward, and moving the verified files into
    /// place is a same-filesystem `rename` per file rather than a decode-
    /// as-you-go extraction straight onto the target.
    pub fn install(
        &mut self,
        archive_path: &Path,
        manifest: &Manifest,
        explicit: bool,
        held: bool,
    ) -> Result<()> {
        let staging_dir = self.staging_dir(&manifest.name);
        let _ = std::fs::remove_dir_all(&staging_dir); // leftover from a prior crashed attempt

        let written = extractor::extract_into(archive_path, &staging_dir)?;

        if let Err(e) = checksum::hash_files(&staging_dir, &written).and_then(|actual| {
            if actual.eq_ignore_ascii_case(&manifest.payload_sha256) {
                Ok(())
            } else {
                Err(PkgError::ChecksumMismatch {
                    name: manifest.name.clone(),
                    expected: manifest.payload_sha256.clone(),
                    actual,
                })
            }
        }) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(e);
        }

        if let Err(e) = self.files.check_no_conflicts(&written, &manifest.name) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(e);
        }

        if let Some(script) = &manifest.hooks.pre_install {
            if let Err(e) = hooks::run(
                HookPoint::PreInstall,
                &staging_dir.join(script),
                self.install_root,
                &manifest.name,
                &manifest.version.to_string(),
            ) {
                let _ = std::fs::remove_dir_all(&staging_dir);
                return Err(e);
            }
        }

        if let Err(e) = self.activate(&staging_dir, &written) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(e);
        }
        let _ = std::fs::remove_dir_all(&staging_dir);

        if let Some(script) = &manifest.hooks.post_install {
            if let Err(e) = hooks::run(
                HookPoint::PostInstall,
                &self.install_root.join(script),
                self.install_root,
                &manifest.name,
                &manifest.version.to_string(),
            ) {
                // The payload is already live at this point — a failing
                // post_install fails the install as a whole (see
                // `package::hooks::Hooks::post_install`), so it's undone
                // the same way a post-extraction checksum failure always
                // has been.
                rollback::undo_install(self.install_root, &written);
                return Err(e);
            }
        }

        self.files.claim(&written, &manifest.name);
        self.packages.insert(InstalledPackage {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            dependencies: manifest.dependencies.clone(),
            provides: manifest.provides.clone(),
            conflicts: manifest.conflicts.clone(),
            installed_files: written,
            explicit,
            held,
            arch: manifest.arch.clone(),
            essential: manifest.essential,
            hooks: manifest.hooks.clone(),
            payload_sha256: manifest.payload_sha256.clone(),
        });

        // Persist only once in-memory state is fully consistent, so a
        // crash right here never leaves the two databases disagreeing.
        self.packages.save()?;
        self.files.save()?;
        Ok(())
    }

    /// Moves every staged file into its final place under `install_root`,
    /// same-filesystem `rename` per file. On a partial failure, whatever
    /// already made it into `install_root` is removed again (via
    /// `rollback::undo_install`) before the error is returned — the
    /// files still sitting in `staging_dir` are left for the caller to
    /// discard along with the rest of the staging directory.
    fn activate(&self, staging_dir: &Path, written: &[PathBuf]) -> Result<()> {
        let mut moved: Vec<PathBuf> = Vec::with_capacity(written.len());
        for rel in written {
            let dest = self.install_root.join(rel);
            if let Some(parent) = dest.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    rollback::undo_install(self.install_root, &moved);
                    return Err(e.into());
                }
            }
            if let Err(e) = std::fs::rename(staging_dir.join(rel), &dest) {
                rollback::undo_install(self.install_root, &moved);
                return Err(e.into());
            }
            moved.push(rel.clone());
        }
        Ok(())
    }

    fn staging_dir(&self, package_name: &str) -> PathBuf {
        self.install_root.join(".mitos-pkg-staging").join(package_name)
    }

    /// Removes an installed package: runs `pre_remove` (if any) while
    /// files are still present, moves its files to a scratch backup dir
    /// and deletes them from the install root, runs `post_remove` (if
    /// any, best-effort — see `package::hooks::Hooks`), and only discards
    /// the backup once every file has been removed successfully. Any
    /// failure during the file-moving phase restores everything from the
    /// backup before returning the error, so a partial removal never
    /// lingers.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        let pkg = self
            .packages
            .get(name)
            .cloned()
            .ok_or_else(|| PkgError::NotInstalled(name.to_string()))?;

        if let Some(script) = &pkg.hooks.pre_remove {
            hooks::run(
                HookPoint::PreRemove,
                &self.install_root.join(script),
                self.install_root,
                &pkg.name,
                &pkg.version.to_string(),
            )?;
        }

        let backup_dir = self.install_root.join(".mitos-pkg-backup").join(name);
        if let Err(e) = Self::backup_and_remove(self.install_root, &pkg, &backup_dir) {
            let _ = rollback::undo_removal(self.install_root, &backup_dir, &pkg.installed_files);
            let _ = std::fs::remove_dir_all(&backup_dir);
            return Err(e);
        }

        if let Some(script) = &pkg.hooks.post_remove {
            // Best-effort by construction (`HookPoint::PostRemove` never
            // returns `Err`) — the script's bytes only still exist in
            // `backup_dir` at this point (its real location was just
            // deleted), so it runs from there, told the real root via
            // `MITOS_PKG_ROOT` for any cleanup it wants to do elsewhere.
            let _ = hooks::run(
                HookPoint::PostRemove,
                &backup_dir.join(script),
                self.install_root,
                &pkg.name,
                &pkg.version.to_string(),
            );
        }
        let _ = std::fs::remove_dir_all(&backup_dir);

        self.files.release(&pkg.installed_files);
        self.packages.remove(name);
        self.packages.save()?;
        self.files.save()?;
        Ok(())
    }

    fn backup_and_remove(
        install_root: &Path,
        pkg: &InstalledPackage,
        backup_dir: &Path,
    ) -> Result<()> {
        for rel in &pkg.installed_files {
            let src = install_root.join(rel);
            if !src.exists() {
                continue;
            }
            let backup_path = backup_dir.join(rel);
            if let Some(parent) = backup_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&src, &backup_path)?;
        }
        Ok(())
    }
}
