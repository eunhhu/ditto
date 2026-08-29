//! Content-addressed object storage for large tool and model outputs.

use std::{
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

const REFERENCE_PREFIX: &str = "artifact:sha256:";
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct ArtifactStore {
    object_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Artifact {
    pub reference: String,
    pub sha256: String,
    pub bytes: usize,
}

#[derive(Debug, Error)]
pub enum ArtifactStoreError {
    #[error("invalid artifact reference: {0}")]
    InvalidReference(String),
    #[error("artifact content failed integrity check: {0}")]
    Integrity(String),
    #[error("artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

impl ArtifactStore {
    /// Opens or creates the SHA-256 object directory.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactStoreError::Io`] when the directory cannot be created.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ArtifactStoreError> {
        let object_root = root.as_ref().join("sha256");
        fs::create_dir_all(&object_root)?;
        Ok(Self { object_root })
    }

    /// Persists bytes under their SHA-256 digest, deduplicating equal content.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactStoreError::Io`] when atomic persistence fails.
    pub fn put(&self, content: &[u8]) -> Result<Artifact, ArtifactStoreError> {
        let digest = digest_hex(content);
        let destination = self.object_root.join(&digest);

        if !destination.exists() {
            let suffix = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let temporary =
                self.object_root
                    .join(format!(".{digest}.{}.{}.tmp", std::process::id(), suffix));
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(content)?;
            file.sync_all()?;
            match fs::rename(&temporary, &destination) {
                Ok(()) => {}
                Err(error) if destination.exists() => {
                    fs::remove_file(&temporary)?;
                    let _ = error;
                }
                Err(error) => return Err(error.into()),
            }
        }

        Ok(Artifact {
            reference: format!("{REFERENCE_PREFIX}{digest}"),
            sha256: digest,
            bytes: content.len(),
        })
    }

    /// Persists UTF-8 text as a content-addressed artifact.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactStoreError::Io`] when atomic persistence fails.
    pub fn put_text(&self, content: &str) -> Result<Artifact, ArtifactStoreError> {
        self.put(content.as_bytes())
    }

    /// Reads and verifies content referenced by an artifact handle.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed references, I/O failures, or hash mismatch.
    pub fn get(&self, reference: &str) -> Result<Vec<u8>, ArtifactStoreError> {
        let digest = parse_reference(reference)?;
        let content = fs::read(self.object_root.join(digest))?;
        if digest_hex(&content) != digest {
            return Err(ArtifactStoreError::Integrity(reference.to_owned()));
        }
        Ok(content)
    }
}

fn parse_reference(reference: &str) -> Result<&str, ArtifactStoreError> {
    let digest = reference
        .strip_prefix(REFERENCE_PREFIX)
        .ok_or_else(|| ArtifactStoreError::InvalidReference(reference.to_owned()))?;
    let valid = digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !valid {
        return Err(ArtifactStoreError::InvalidReference(reference.to_owned()));
    }
    Ok(digest)
}

fn digest_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn equal_content_deduplicates_to_same_reference() {
        let directory = tempdir().unwrap();
        let store = ArtifactStore::open(directory.path()).unwrap();
        let first = store.put_text("build output").unwrap();
        let second = store.put_text("build output").unwrap();

        assert_eq!(first.reference, second.reference);
        assert_eq!(store.get(&first.reference).unwrap(), b"build output");
    }

    #[test]
    fn malformed_references_are_rejected() {
        let directory = tempdir().unwrap();
        let store = ArtifactStore::open(directory.path()).unwrap();
        let error = store.get("artifact:sha256:nope").unwrap_err();
        assert!(matches!(error, ArtifactStoreError::InvalidReference(_)));
    }
}
