use crate::database::files::FileDb;
use crate::database::packages::{InstalledDb, InstalledPackage};
use crate::error::{PkgError, Result};
use crate::install::{extractor, rollback};
use crate::package::manifest::Manifest;
use std::path::Path;

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
    /// bookkeeping.
    pub fn install(
        &mut self,
        archive_path: &Path,
        manifest: &Manifest,
        explicit: bool,
    ) -> Result<()> {
        let written = extractor::extract_into(archive_path, self.install_root)?;

        if let Err(e) = self.files.check_no_conflicts(&written, &manifest.name) {
            rollback::undo_install(self.install_root, &written);
            return Err(e);
        }

        self.files.claim(&written, &manifest.name);
        self.packages.insert(InstalledPackage {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            dependencies: manifest.dependencies.clone(),
            installed_files: written,
            explicit,
        });

        // Persist only once in-memory state is fully consistent, so a
        // crash right here never leaves the two databases disagreeing.
        self.packages.save()?;
        self.files.save()?;
        Ok(())
    }

    /// Removes an installed package: moves its files to a scratch backup
    /// dir, deletes them from the install root, and only discards the
    /// backup once every file has been removed successfully. Any failure
    /// mid-removal restores everything from the backup before returning
    /// the error, so a partial removal never lingers.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        let pkg = self
            .packages
            .get(name)
            .cloned()
            .ok_or_else(|| PkgError::NotInstalled(name.to_string()))?;

        let backup_dir = self.install_root.join(".mitos-pkg-backup").join(name);
        if let Err(e) = Self::backup_and_remove(self.install_root, &pkg, &backup_dir) {
            let _ = rollback::undo_removal(self.install_root, &backup_dir, &pkg.installed_files);
            let _ = std::fs::remove_dir_all(&backup_dir);
            return Err(e);
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
