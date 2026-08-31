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

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

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

/// A bounded range read together with the byte count from the verified object.
///
/// Both values come from the same open file descriptor after the complete
/// object has been hashed and checked against its [`ArtifactRef`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifactRange {
    bytes: Vec<u8>,
    total_bytes: u64,
}

impl VerifiedArtifactRange {
    /// Returns the bytes in the requested range.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the verified size of the complete object.
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
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

/// Immutable metadata belonging to the content itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactMetadata {
    pub reference: ArtifactRef,
    pub bytes: u64,
    pub first_seen_at: DateTime<Utc>,
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
        let root = root.as_ref();
        create_private_dir(root)?;
        let object_root = root.join("sha256");
        let metadata_root = root.join("metadata");
        create_private_dir(&object_root)?;
        create_private_dir(&metadata_root)?;
        Ok(Self {
            object_root,
            metadata_root,
            max_object_bytes,
        })
    }

    pub fn put(&self, bytes: &[u8]) -> Result<ArtifactMetadata, ArtifactStoreError> {
        self.put_reader(bytes)
    }

    pub fn put_reader(
        &self,
        mut reader: impl Read,
    ) -> Result<ArtifactMetadata, ArtifactStoreError> {
        let temporary_path = self.object_root.join(format!(".tmp-{}", Ulid::new()));
        let result = (|| {
            let mut temporary = open_private_new(&temporary_path)?;
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
                    let _ = self.open_verified(&reference)?;
                    fs::remove_file(&temporary_path)?;
                }
                Err(error) => return Err(error.into()),
            }

            self.install_metadata(&reference, total_bytes)
        })();

        if temporary_path.exists() {
            let _ = fs::remove_file(temporary_path);
        }
        result
    }

    /// Verifies and reads through the same file descriptor, avoiding replacement races.
    pub fn get(&self, reference: &ArtifactRef) -> Result<Vec<u8>, ArtifactStoreError> {
        Ok(self.read_verified_range(reference, 0, usize::MAX)?.bytes)
    }

    /// Verifies and range-reads through the same file descriptor.
    pub fn read_range(
        &self,
        reference: &ArtifactRef,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, ArtifactStoreError> {
        Ok(self.read_verified_range(reference, offset, length)?.bytes)
    }

    /// Verifies the complete object and reads a bounded range through the
    /// same file descriptor, returning the verified complete-object size.
    pub fn read_verified_range(
        &self,
        reference: &ArtifactRef,
        offset: u64,
        length: usize,
    ) -> Result<VerifiedArtifactRange, ArtifactStoreError> {
        self.read_verified_range_internal(reference, offset, length, |_| {})
    }

    fn read_verified_range_internal<F>(
        &self,
        reference: &ArtifactRef,
        offset: u64,
        length: usize,
        after_verification: F,
    ) -> Result<VerifiedArtifactRange, ArtifactStoreError>
    where
        F: FnOnce(&Path),
    {
        let path = self.object_path(reference);
        let mut file = open_regular_file(&path)?;
        let requested = u64::try_from(length).unwrap_or(u64::MAX);
        let range_end = offset.saturating_add(requested);
        let mut hasher = Sha256::new();
        let mut total_bytes = 0_u64;
        let mut output = Vec::new();
        let mut buffer = [0_u8; 64 * 1024];

        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let chunk_start = total_bytes;
            total_bytes = total_bytes.saturating_add(read as u64);
            if total_bytes > self.max_object_bytes {
                return Err(ArtifactStoreError::TooLarge {
                    max_bytes: self.max_object_bytes,
                });
            }
            hasher.update(&buffer[..read]);

            let capture_start = offset.max(chunk_start);
            let capture_end = range_end.min(total_bytes);
            if capture_start < capture_end {
                let start = usize::try_from(capture_start - chunk_start).unwrap_or(read);
                let end = usize::try_from(capture_end - chunk_start).unwrap_or(read);
                output.extend_from_slice(&buffer[start..end]);
            }
        }

        let actual = format!("{:x}", hasher.finalize());
        if actual != reference.sha256() {
            return Err(ArtifactStoreError::Integrity {
                expected: reference.to_string(),
                actual,
            });
        }
        after_verification(&path);

        Ok(VerifiedArtifactRange {
            bytes: output,
            total_bytes,
        })
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
        reference: &ArtifactRef,
        bytes: u64,
    ) -> Result<ArtifactMetadata, ArtifactStoreError> {
        let path = self.metadata_path(reference);
        let temporary_path = self
            .metadata_root
            .join(format!(".tmp-{}.json", Ulid::new()));
        let metadata = ArtifactMetadata {
            reference: reference.clone(),
            bytes,
            first_seen_at: Utc::now(),
        };
        let result = (|| {
            let encoded = serde_json::to_vec(&metadata)?;
            let mut temporary = open_private_new(&temporary_path)?;
            temporary.write_all(&encoded)?;
            temporary.sync_all()?;
            drop(temporary);

            match fs::hard_link(&temporary_path, &path) {
                Ok(()) => {
                    fs::remove_file(&temporary_path)?;
                    sync_directory(&self.metadata_root)?;
                    Ok(metadata)
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    fs::remove_file(&temporary_path)?;
                    let existing = self.metadata(reference)?;
                    if existing.bytes != bytes {
                        return Err(ArtifactStoreError::MetadataMismatch(reference.clone()));
                    }
                    Ok(existing)
                }
                Err(error) => Err(error.into()),
            }
        })();
        if temporary_path.exists() {
            let _ = fs::remove_file(temporary_path);
        }
        result
    }

    fn open_verified(&self, reference: &ArtifactRef) -> Result<(File, u64), ArtifactStoreError> {
        let path = self.object_path(reference);
        let mut file = open_regular_file(&path)?;
        let bytes = verify_open_file(&mut file, reference, self.max_object_bytes)?;
        file.seek(SeekFrom::Start(0))?;
        Ok((file, bytes))
    }

    fn object_path(&self, reference: &ArtifactRef) -> PathBuf {
        self.object_root.join(reference.sha256())
    }

    fn metadata_path(&self, reference: &ArtifactRef) -> PathBuf {
        self.metadata_root
            .join(format!("{}.json", reference.sha256()))
    }
}

fn verify_open_file(
    file: &mut File,
    reference: &ArtifactRef,
    max_object_bytes: u64,
) -> Result<u64, ArtifactStoreError> {
    let mut hasher = Sha256::new();
    let mut total_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read as u64);
        if total_bytes > max_object_bytes {
            return Err(ArtifactStoreError::TooLarge {
                max_bytes: max_object_bytes,
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

fn create_private_dir(path: &Path) -> Result<(), ArtifactStoreError> {
    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

fn open_private_new(path: &Path) -> Result<File, ArtifactStoreError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    Ok(options.open(path)?)
}

fn open_regular_file(path: &Path) -> Result<File, ArtifactStoreError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(ArtifactStoreError::NotRegularFile(path.to_path_buf()));
    }
    Ok(file)
}

fn sync_directory(path: &Path) -> Result<(), ArtifactStoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
    };

    use tempfile::tempdir;

    use super::{ArtifactRef, ArtifactStore, ArtifactStoreError};

    #[test]
    fn deduplicates_and_reads_ranges() {
        let directory = tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("open store");
        let first = store.put(b"semantic artifact").expect("put artifact");
        let second = store
            .put(b"semantic artifact")
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
        let verified = store
            .read_verified_range(&first.reference, 9, 8)
            .expect("read verified range");
        assert_eq!(verified.bytes(), b"artifact");
        assert_eq!(verified.total_bytes(), b"semantic artifact".len() as u64);
    }

    #[test]
    fn detects_tampering_and_enforces_size_limit() {
        let directory = tempdir().expect("temporary directory");
        let store = ArtifactStore::with_max_object_bytes(directory.path(), 8).expect("open store");
        let too_large = store
            .put(b"nine-byte")
            .expect_err("reject oversized artifact");
        assert!(matches!(too_large, ArtifactStoreError::TooLarge { .. }));

        let metadata = store.put(b"original").expect("put artifact");
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

    #[test]
    fn verified_range_returns_captured_bytes_after_same_inode_mutation() {
        let directory = tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("open store");
        let metadata = store.put(b"0123456789").expect("put artifact");
        let object = directory
            .path()
            .join("sha256")
            .join(metadata.reference.sha256());

        #[cfg(unix)]
        let inode_before = std::os::unix::fs::MetadataExt::ino(
            &fs::metadata(&object).expect("stat object before mutation"),
        );

        let verified = store
            .read_verified_range_internal(&metadata.reference, 2, 4, |path| {
                let mut writer = OpenOptions::new()
                    .write(true)
                    .open(path)
                    .expect("open same object inode for mutation");
                writer
                    .write_all(b"XXXXXXXXXX")
                    .expect("mutate same object inode");
                writer.sync_all().expect("sync same object inode");
            })
            .expect("read verified range");

        assert_eq!(verified.bytes(), b"2345");
        assert_eq!(verified.total_bytes(), 10);
        #[cfg(unix)]
        assert_eq!(
            inode_before,
            std::os::unix::fs::MetadataExt::ino(
                &fs::metadata(&object).expect("stat object after mutation"),
            )
        );
        assert!(matches!(
            store.read_range(&metadata.reference, 0, 4),
            Err(ArtifactStoreError::Integrity { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_objects() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let store = ArtifactStore::open(directory.path()).expect("open store");
        let metadata = store.put(b"linked").expect("put artifact");
        let object = directory
            .path()
            .join("sha256")
            .join(metadata.reference.sha256());
        fs::remove_file(&object).expect("remove object");
        symlink("/dev/null", &object).expect("create symlink");

        let error = store.get(&metadata.reference).expect_err("reject symlink");
        assert!(matches!(error, ArtifactStoreError::Io(_)));
    }

    #[test]
    fn rejects_malformed_references_during_deserialization() {
        assert!("../state.db".parse::<ArtifactRef>().is_err());
        assert!(serde_json::from_str::<ArtifactRef>(r#""artifact:sha256:../state.db""#).is_err());
    }
}
