use thiserror::Error;

/// Every error mitos-pkg can produce. Kept as one flat enum (rather than
/// per-module error types) so callers up in `service` and `main` can match
/// on it without threading `Box<dyn Error>` through every layer.
#[derive(Debug, Error)]
pub enum PkgError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("(de)serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("network error: {0}")]
    Network(String),

    #[error("checksum mismatch for '{name}': expected {expected}, got {actual}")]
    ChecksumMismatch {
        name: String,
        expected: String,
        actual: String,
    },

    #[error("signature verification failed for '{0}'")]
    InvalidSignature(String),

    #[error("no trusted key found for signer '{0}'")]
    UntrustedKey(String),

    #[error("package '{0}' not found in any configured repository")]
    PackageNotFound(String),

    #[error("no version of '{name}' satisfies '{requirement}'")]
    NoMatchingVersion { name: String, requirement: String },

    #[error("invalid package specifier '{0}' (expected 'name' or 'name@version')")]
    InvalidPackageSpec(String),

    #[error("package '{0}' is already installed (version {1})")]
    AlreadyInstalled(String, String),

    #[error("package '{0}' is not installed")]
    NotInstalled(String),

    #[error("'{0}' is held — run `mitos-pkg unhold` first, or pass --ignore-hold")]
    PackageHeld(String),

    #[error("dependency conflict: {0}")]
    DependencyConflict(String),

    #[error("circular dependency detected involving '{0}'")]
    CircularDependency(String),

    #[error("cannot remove '{0}': required by {1:?}")]
    RequiredByOthers(String, Vec<String>),

    #[error("file conflict: '{path}' is already owned by package '{owner}'")]
    FileConflict { path: String, owner: String },

    #[error("invalid package manifest: {0}")]
    InvalidManifest(String),

    #[error("another mitos-pkg operation appears to be in progress (lock file: {0})")]
    Locked(std::path::PathBuf),

    #[error("package '{name}' is built for {package_arch:?}, but this host targets '{host_arch}'")]
    ArchMismatch {
        name: String,
        package_arch: Vec<String>,
        host_arch: String,
    },

    #[error(
        "not enough free space at the install root: need {needed} bytes, {available} available"
    )]
    InsufficientDiskSpace { needed: u64, available: u64 },

    #[error("'{0}' is an essential package — pass --force to remove it anyway")]
    EssentialPackage(String),

    #[error("{hook} hook for '{package}' failed: {reason}")]
    HookFailed {
        package: String,
        hook: String,
        reason: String,
    },
}

pub type Result<T> = std::result::Result<T, PkgError>;
