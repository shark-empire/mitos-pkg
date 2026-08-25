use crate::error::{PkgError, Result};
use crate::package::format::{self, MANIFEST_FILE_NAME, PAYLOAD_DIR};
use crate::package::manifest::Manifest;
use crate::package::spec::PackageSpec;
use crate::security::{checksum, signature};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::File;
use std::path::{Path, PathBuf};
use tar::{Builder, Header};

/// Everything `mitos-pkg build` produces. `signature_hex`, when present,
/// is *not* written into the archive — signatures live in
/// `repository::metadata::PackageMetadata`, published separately from the
/// package itself, so the maintainer has to carry this value over into
/// their repository index by hand (or their own index-generation
/// tooling).
pub struct BuildOutput {
    pub archive_path: PathBuf,
    pub manifest: Manifest,
    pub signature_hex: Option<String>,
}

/// Packages `source_dir` — which must contain `pkg.json` (a `PackageSpec`)
/// and a `payload/` directory laid out exactly as it should land under
/// the install root — into a `.mpkg` archive written to `output_dir`.
///
/// If `sign_seed` is given, also signs the computed `payload_sha256` with
/// it. The spec must already declare a `signer` name in that case: a
/// signature with nobody named to attribute it to verifies nothing, so
/// build refuses rather than producing one silently.
pub fn build_package(
    source_dir: &Path,
    output_dir: &Path,
    sign_seed: Option<&[u8; 32]>,
) -> Result<BuildOutput> {
    let spec_path = source_dir.join("pkg.json");
    let payload_dir = source_dir.join(PAYLOAD_DIR);

    let spec_data = std::fs::read_to_string(&spec_path)?;
    let spec: PackageSpec = serde_json::from_str(&spec_data)?;

    if !payload_dir.is_dir() {
        return Err(PkgError::InvalidManifest(format!(
            "no {PAYLOAD_DIR}/ directory found in {}",
            source_dir.display()
        )));
    }

    let payload_sha256 = checksum::hash_payload_dir(&payload_dir)?;
    let files = checksum::list_files(&payload_dir)?
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let manifest = Manifest {
        name: spec.name.clone(),
        version: spec.version.clone(),
        description: spec.description.clone(),
        dependencies: spec.dependencies.clone(),
        provides: spec.provides.clone(),
        conflicts: spec.conflicts.clone(),
        files,
        payload_sha256: payload_sha256.clone(),
        signer: spec.signer.clone(),
    };

    let signature_hex = match sign_seed {
        Some(seed) => {
            if spec.signer.is_none() {
                return Err(PkgError::InvalidManifest(
                    "pkg.json has no \"signer\" set — add one before using --sign-with"
                        .to_string(),
                ));
            }
            let sig = signature::sign(seed, payload_sha256.as_bytes());
            Some(hex::encode(sig))
        }
        None => None,
    };

    std::fs::create_dir_all(output_dir)?;
    let archive_path =
        output_dir.join(format::package_filename(&manifest.name, &manifest.version));
    write_archive(&archive_path, &manifest, &payload_dir)?;

    Ok(BuildOutput {
        archive_path,
        manifest,
        signature_hex,
    })
}

fn write_archive(archive_path: &Path, manifest: &Manifest, payload_dir: &Path) -> Result<()> {
    let file = File::create(archive_path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut tar = Builder::new(encoder);

    let manifest_json = serde_json::to_vec_pretty(manifest)?;
    let mut header = Header::new_gnu();
    header.set_path(MANIFEST_FILE_NAME)?;
    header.set_size(manifest_json.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, &manifest_json[..])?;

    tar.append_dir_all(PAYLOAD_DIR, payload_dir)?;

    // `into_inner` finalizes the tar end-of-archive marker and hands back
    // the gzip encoder; `finish` on *that* flushes the gzip trailer.
    // Skipping either (e.g. by just letting things drop) can produce an
    // archive that happens to read back fine with one tool and not
    // another — better to finalize both explicitly.
    let encoder = tar.into_inner()?;
    encoder.finish()?;
    Ok(())
}
