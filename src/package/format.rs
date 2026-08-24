use semver::Version;

/// A `.mpkg` file is a gzip-compressed tar archive containing:
///   manifest.json     - the package `Manifest`, at the archive root
///   payload/...        - files to be installed, relative to install-root
pub const PACKAGE_EXTENSION: &str = "mpkg";
pub const MANIFEST_FILE_NAME: &str = "manifest.json";
pub const PAYLOAD_DIR: &str = "payload";

/// Canonical archive filename for a given package name + version, e.g.
/// `mitos-libc-0.3.1.mpkg`.
pub fn package_filename(name: &str, version: &Version) -> String {
    format!("{name}-{version}.{PACKAGE_EXTENSION}")
}
