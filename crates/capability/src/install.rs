//! Installing a capability package under a registry root.
//!
//! Installation is the one act that adds a package to a registry, and it is
//! pinned: the caller names the manifest digest it expects, and the write only
//! happens when the package on disk hashes to it. A digest the caller did not
//! name never becomes installed, and a package that is already installed at a
//! different digest is a conflict rather than a silent upgrade - upgrading is a
//! separate decision with its own review, not a side effect of re-running an
//! install.
//!
//! Nothing here executes package code or reads its schema: the manifest and the
//! header that pins it are the whole written artifact.

use std::path::{Path, PathBuf};

use crate::package::CapabilityHeader;

const MANIFEST: &str = "capability.toml";
const HEADER: &str = "capability.header.json";

/// A package the registry root now holds, pinned at `manifest_digest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    pub id: String,
    pub version: String,
    pub manifest_digest: String,
    pub path: PathBuf,
}

#[derive(Debug)]
pub enum InstallError {
    /// The manifest, its header, or the package limits refused the bytes.
    Invalid(crate::CapabilityError),
    /// The package hashes to something other than the digest the caller named.
    DigestMismatch {
        expected: String,
        actual: String,
    },
    /// The id is installed at another digest. Upgrading is its own review.
    Conflict {
        id: String,
        installed: String,
        offered: String,
    },
    /// The id is not installed, so there is nothing to remove.
    NotInstalled {
        id: String,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => write!(formatter, "package is not installable: {error}"),
            Self::DigestMismatch { expected, actual } => write!(
                formatter,
                "package digest {actual} is not the pinned {expected}"
            ),
            Self::Conflict {
                id,
                installed,
                offered,
            } => write!(
                formatter,
                "{id} is installed at {installed}; {offered} needs an upgrade decision"
            ),
            Self::NotInstalled { id } => write!(formatter, "{id} is not installed"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<std::io::Error> for InstallError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::CapabilityError> for InstallError {
    fn from(error: crate::CapabilityError) -> Self {
        Self::Invalid(error)
    }
}

/// Install the package whose manifest is `manifest_bytes`, if it hashes to
/// `expected`.
///
/// `root` is a registry root: each package is one directory named after its id,
/// holding the manifest as given and the header that pins it. The manifest
/// bytes are written verbatim so the digest a caller pinned is the digest the
/// registry later reads.
pub fn install(
    root: &Path,
    manifest_bytes: &[u8],
    expected: &str,
) -> Result<InstalledPackage, InstallError> {
    let header = CapabilityHeader::from_manifest_bytes(manifest_bytes)?;
    let actual = header.manifest_sha256.clone();
    if actual != expected {
        return Err(InstallError::DigestMismatch {
            expected: expected.to_owned(),
            actual,
        });
    }
    let directory = root.join(&header.id);
    let header_path = directory.join(HEADER);
    if header_path.is_file() {
        let installed = std::fs::read(&header_path)?;
        let installed: CapabilityHeader = serde_json::from_slice(&installed).map_err(|_| {
            InstallError::Invalid(crate::CapabilityError::PackageInvalid(
                "installed header is malformed",
            ))
        })?;
        if installed.manifest_sha256 == header.manifest_sha256 {
            // The same package, already installed: nothing to write.
            return Ok(InstalledPackage {
                id: installed.id,
                version: installed.version,
                manifest_digest: installed.manifest_sha256,
                path: directory,
            });
        }
        return Err(InstallError::Conflict {
            id: header.id,
            installed: installed.manifest_sha256,
            offered: header.manifest_sha256,
        });
    }
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join(MANIFEST), manifest_bytes)?;
    std::fs::write(&header_path, header.to_json()?.as_bytes())?;
    Ok(InstalledPackage {
        id: header.id,
        version: header.version,
        manifest_digest: header.manifest_sha256,
        path: directory,
    })
}

/// Remove the package installed as `id`, if it is still pinned at `expected`.
///
/// A package that moved on since the caller looked is not removed: the digest
/// the caller named is the only package the removal is about.
pub fn uninstall(root: &Path, id: &str, expected: &str) -> Result<(), InstallError> {
    let directory = root.join(id);
    let header_path = directory.join(HEADER);
    if !header_path.is_file() {
        return Err(InstallError::NotInstalled { id: id.to_owned() });
    }
    let bytes = std::fs::read(&header_path)?;
    let header: CapabilityHeader = serde_json::from_slice(&bytes).map_err(|_| {
        InstallError::Invalid(crate::CapabilityError::PackageInvalid(
            "installed header is malformed",
        ))
    })?;
    if header.manifest_sha256 != expected {
        return Err(InstallError::DigestMismatch {
            expected: expected.to_owned(),
            actual: header.manifest_sha256,
        });
    }
    std::fs::remove_dir_all(&directory)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../capabilities/core/memory-manage/capability.toml"
        ))
        .expect("fixture manifest")
    }

    fn digest(bytes: &[u8]) -> String {
        CapabilityHeader::from_manifest_bytes(bytes)
            .unwrap()
            .manifest_sha256
    }

    #[test]
    fn a_digest_the_caller_did_not_name_never_becomes_installed() {
        let root = tempfile::tempdir().unwrap();
        let bytes = manifest();
        let error = install(root.path(), &bytes, &"0".repeat(64)).unwrap_err();
        assert!(
            matches!(error, InstallError::DigestMismatch { .. }),
            "{error}"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn installing_writes_the_manifest_and_the_header_that_pins_it() {
        let root = tempfile::tempdir().unwrap();
        let bytes = manifest();
        let installed = install(root.path(), &bytes, &digest(&bytes)).unwrap();
        assert_eq!(installed.id, "memory.manage");
        assert_eq!(
            std::fs::read(installed.path.join("capability.toml")).unwrap(),
            bytes
        );
        let header: CapabilityHeader =
            serde_json::from_slice(&std::fs::read(installed.path.join(HEADER)).unwrap()).unwrap();
        assert_eq!(header.manifest_sha256, installed.manifest_digest);
    }

    #[test]
    fn installing_the_same_package_twice_is_the_same_package() {
        let root = tempfile::tempdir().unwrap();
        let bytes = manifest();
        let pinned = digest(&bytes);
        let first = install(root.path(), &bytes, &pinned).unwrap();
        let second = install(root.path(), &bytes, &pinned).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_package_already_installed_at_another_digest_is_a_conflict() {
        let root = tempfile::tempdir().unwrap();
        let bytes = manifest();
        install(root.path(), &bytes, &digest(&bytes)).unwrap();
        let mut edited = bytes.clone();
        edited.extend_from_slice(b"\n# upgraded\n");
        let error = install(root.path(), &edited, &digest(&edited)).unwrap_err();
        assert!(matches!(error, InstallError::Conflict { .. }), "{error}");
        // The installed package is untouched: an upgrade needs its own decision.
        assert_eq!(
            std::fs::read(root.path().join("memory.manage").join("capability.toml")).unwrap(),
            bytes
        );
    }

    #[test]
    fn uninstalling_removes_only_the_digest_the_caller_named() {
        let root = tempfile::tempdir().unwrap();
        let bytes = manifest();
        let pinned = digest(&bytes);
        install(root.path(), &bytes, &pinned).unwrap();
        let error = uninstall(root.path(), "memory.manage", &"0".repeat(64)).unwrap_err();
        assert!(
            matches!(error, InstallError::DigestMismatch { .. }),
            "{error}"
        );
        assert!(root.path().join("memory.manage").is_dir());
        uninstall(root.path(), "memory.manage", &pinned).unwrap();
        assert!(!root.path().join("memory.manage").exists());
        let error = uninstall(root.path(), "memory.manage", &pinned).unwrap_err();
        assert!(
            matches!(error, InstallError::NotInstalled { .. }),
            "{error}"
        );
    }

    #[test]
    fn an_installed_package_loads_into_a_catalogue() {
        let root = tempfile::tempdir().unwrap();
        let bytes = manifest();
        install(root.path(), &bytes, &digest(&bytes)).unwrap();
        let catalog = crate::CapabilityCatalog::load(root.path()).unwrap();
        assert_eq!(catalog.len(), 1);
    }
}
