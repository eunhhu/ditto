use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;
use ulid::Ulid;

pub const DEFAULT_MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
const REFERENCE_PREFIX: &str = "artifact:sha256:";

#[derive(Debug, Error)]
pub enum ArtifactStoreError {
    #[error("artifact store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact metadata is invalid: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("invalid SHA-256 artifact reference: {0}")]
    InvalidReference(String),
    #[error("artifact exceeds the {max_bytes} byte limit")]
    TooLarge { max_bytes: u64 },
    #[error("artifact {0} is not a regular file")]
    NotRegularFile(PathBuf),
    #[error("artifact integrity mismatch: expected {expected}, calculated {actual}")]
    Integrity { expected: String, actual: String },
    #[error("artifact metadata does not match {0}")]
    MetadataMismatch(ArtifactRef),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtifactRef(String);

impl ArtifactRef {
    pub fn new(reference: impl Into<String>) -> Result<Self, ArtifactStoreError> {
        let reference = reference.into();
        let sha256 = reference
            .strip_prefix(REFERENCE_PREFIX)
            .ok_or_else(|| ArtifactStoreError::InvalidReference(reference.clone()))?;
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ArtifactStoreError::InvalidReference(reference));
        }
        Ok(Self(reference))
    }

    pub fn from_sha256(sha256: impl Into<String>) -> Result<Self, ArtifactStoreError> {
        Self::new(format!("{REFERENCE_PREFIX}{}", sha256.into()))
    }

    pub fn sha256(&self) -> &str {
        self.0
            .strip_prefix(REFERENCE_PREFIX)
            .expect("ArtifactRef is validated at construction")
    }
}

impl fmt::Display for ArtifactRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ArtifactRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ArtifactRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let reference = String::deserialize(deserializer)?;
        Self::new(reference).map_err(serde::de::Error::custom)
    }
}

impl FromStr for ArtifactRef {
    type Err = ArtifactStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PutOptions {
    pub mime: Option<String>,
    pub producer_event_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactMetadata {
    pub reference: ArtifactRef,
    pub bytes: u64,
    pub mime: Option<String>,
    pub created_at: DateTime<Utc>,
    pub producer_event_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    object_root: PathBuf,
    metadata_root: PathBuf,
    max_object_bytes: u64,
}

impl ArtifactStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ArtifactStoreError> {
        Self::with_max_object_bytes(root, DEFAULT_MAX_OBJECT_BYTES)
    }

    pub fn with_max_object_bytes(
        root: impl AsRef<Path>,
        max_object_bytes: u64,
    ) -> Result<Self, ArtifactStoreError> {
        let object_root = root.as_ref().join("sha256");
        let metadata_root = root.as_ref().join("metadata");
        fs::create_dir_all(&object_root)?;
        fs::create_dir_all(&metadata_root)?;
        Ok(Self {
            object_root,
            metadata_root,
            max_object_bytes,
        })
    }

    pub fn put(
        &self,
        bytes: &[u8],
        options: PutOptions,
    ) -> Result<ArtifactMetadata, ArtifactStoreError> {
        self.put_reader(bytes, options)
    }

    pub fn put_reader(
        &self,
        mut reader: impl Read,
        options: PutOptions,
    ) -> Result<ArtifactMetadata, ArtifactStoreError> {
        let temporary_path = self.object_root.join(format!(".tmp-{}", Ulid::new()));
        let result = (|| {
            let mut temporary = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary_path)?;
            let mut hasher = Sha256::new();
            let mut total_bytes = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];

            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                total_bytes = total_bytes.saturating_add(read as u64);
                if total_bytes > self.max_object_bytes {
                    return Err(ArtifactStoreError::TooLarge {
                        max_bytes: self.max_object_bytes,
                    });
                }
                hasher.update(&buffer[..read]);
                temporary.write_all(&buffer[..read])?;
            }
            temporary.sync_all()?;
            drop(temporary);

            let reference = ArtifactRef::from_sha256(format!("{:x}", hasher.finalize()))?;
            let object_path = self.object_path(&reference);
            match fs::hard_link(&temporary_path, &object_path) {
                Ok(()) => {
                    fs::remove_file(&temporary_path)?;
                    sync_directory(&self.object_root)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.verify_path(&object_path, &reference)?;
                    fs::remove_file(&temporary_path)?;
                }
                Err(error) => return Err(error.into()),
            }

            let metadata = ArtifactMetadata {
                reference,
                bytes: total_bytes,
                mime: options.mime,
                created_at: Utc::now(),
                producer_event_id: options.producer_event_id,
            };
            self.install_metadata(&metadata)
        })();

        if temporary_path.exists() {
            let _ = fs::remove_file(temporary_path);
        }
        result
    }

    pub fn get(&self, reference: &ArtifactRef) -> Result<Vec<u8>, ArtifactStoreError> {
        let path = self.object_path(reference);
        self.verify_path(&path, reference)?;
        Ok(fs::read(path)?)
    }

    pub fn read_range(
        &self,
        reference: &ArtifactRef,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, ArtifactStoreError> {
        let path = self.object_path(reference);
        let bytes = self.verify_path(&path, reference)?;
        if offset >= bytes || length == 0 {
            return Ok(Vec::new());
        }

        let available = bytes - offset;
        let requested = u64::try_from(length).unwrap_or(u64::MAX);
        let read_length = usize::try_from(available.min(requested)).unwrap_or(length);
        let mut file = open_regular_file(&path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut output = vec![0_u8; read_length];
        file.read_exact(&mut output)?;
        Ok(output)
    }

    pub fn metadata(
        &self,
        reference: &ArtifactRef,
    ) -> Result<ArtifactMetadata, ArtifactStoreError> {
        let path = self.metadata_path(reference);
        let mut file = open_regular_file(&path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let metadata: ArtifactMetadata = serde_json::from_slice(&bytes)?;
        if metadata.reference != *reference {
            return Err(ArtifactStoreError::MetadataMismatch(reference.clone()));
        }
        Ok(metadata)
    }

    fn install_metadata(
        &self,
        metadata: &ArtifactMetadata,
    ) -> Result<ArtifactMetadata, ArtifactStoreError> {
        let path = self.metadata_path(&metadata.reference);
        let temporary_path = self
            .metadata_root
            .join(format!(".tmp-{}.json", Ulid::new()));
        let result = (|| {
            let encoded = serde_json::to_vec(metadata)?;
            let mut temporary = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary_path)?;
            temporary.write_all(&encoded)?;
            temporary.sync_all()?;
            drop(temporary);

            match fs::hard_link(&temporary_path, &path) {
                Ok(()) => {
                    fs::remove_file(&temporary_path)?;
                    sync_directory(&self.metadata_root)?;
                    Ok(metadata.clone())
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    fs::remove_file(&temporary_path)?;
                    self.metadata(&metadata.reference)
                }
                Err(error) => Err(error.into()),
            }
        })();
        if temporary_path.exists() {
            let _ = fs::remove_file(temporary_path);
        }
        result
    }

    fn verify_path(&self, path: &Path, reference: &ArtifactRef) -> Result<u64, ArtifactStoreError> {
        let mut file = open_regular_file(path)?;
        let mut hasher = Sha256::new();
        let mut total_bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total_bytes = total_bytes.saturating_add(read as u64);
            if total_bytes > self.max_object_bytes {
                return Err(ArtifactStoreError::TooLarge {
                    max_bytes: self.max_object_bytes,
                });
            }
            hasher.update(&buffer[..read]);
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != reference.sha256() {
            return Err(ArtifactStoreError::Integrity {
                expected: reference.to_string(),
                actual,
            });
        }
        Ok(total_bytes)
    }

    fn object_path(&self, reference: &ArtifactRef) -> PathBuf {
        self.object_root.join(reference.sha256())
    }

    fn metadata_path(&self, reference: &ArtifactRef) -> PathBuf {
        self.metadata_root
            .join(format!("{}.json", reference.sha256()))
    }
}

fn open_regular_file(path: &Path) -> Result<File, ArtifactStoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(ArtifactStoreError::NotRegularFile(path.to_path_buf()));
    }
    Ok(File::open(path)?)
}

fn sync_directory(path: &Path) -> Result<(), ArtifactStoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{ArtifactRef, ArtifactStore, ArtifactStoreError, PutOptions};

    #[test]
    fn deduplicates_and_reads_ranges() {
        let directory = tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("open store");
        let first = store
            .put(
                b"semantic artifact",
                PutOptions {
                    mime: Some("text/plain".into()),
                    producer_event_id: Some("event-1".into()),
                },
            )
            .expect("put artifact");
        let second = store
            .put(b"semantic artifact", PutOptions::default())
            .expect("deduplicate artifact");

        assert_eq!(first, second);
        assert_eq!(
            store.get(&first.reference).expect("get artifact"),
            b"semantic artifact"
        );
        assert_eq!(
            store
                .read_range(&first.reference, 9, 8)
                .expect("read range"),
            b"artifact"
        );
    }

    #[test]
    fn detects_tampering_and_enforces_size_limit() {
        let directory = tempdir().expect("temporary directory");
        let store = ArtifactStore::with_max_object_bytes(directory.path(), 8).expect("open store");
        let too_large = store
            .put(b"nine-byte", PutOptions::default())
            .expect_err("reject oversized artifact");
        assert!(matches!(too_large, ArtifactStoreError::TooLarge { .. }));

        let metadata = store
            .put(b"original", PutOptions::default())
            .expect("put artifact");
        fs::write(
            directory
                .path()
                .join("sha256")
                .join(metadata.reference.sha256()),
            b"tampered",
        )
        .expect("tamper artifact");
        let error = store
            .get(&metadata.reference)
            .expect_err("detect tampering");
        assert!(matches!(error, ArtifactStoreError::Integrity { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_objects() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("open store");
        let metadata = store
            .put(b"linked", PutOptions::default())
            .expect("put artifact");
        let object = directory
            .path()
            .join("sha256")
            .join(metadata.reference.sha256());
        fs::remove_file(&object).expect("remove object");
        symlink("/dev/null", &object).expect("create symlink");

        let error = store.get(&metadata.reference).expect_err("reject symlink");
        assert!(matches!(error, ArtifactStoreError::NotRegularFile(_)));
    }

    #[test]
    fn rejects_malformed_references_during_deserialization() {
        assert!("../state.db".parse::<ArtifactRef>().is_err());
        assert!(serde_json::from_str::<ArtifactRef>(r#""artifact:sha256:../state.db""#).is_err());
    }
}
