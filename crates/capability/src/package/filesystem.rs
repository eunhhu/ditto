//! Descriptor-relative metadata access. No directory or metadata symlink is
//! followed, including when a path is replaced between discovery and paging.

use crate::CapabilityError;
use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use super::super::{
        MAX_DISCOVERY_DEPTH, MAX_DISCOVERY_ENTRIES, MAX_PACKAGE_COUNT, PACKAGE_HEADER_FILENAME,
        charge, ensure_limit, invalid,
    };
    use super::*;
    use std::{
        collections::BTreeSet,
        ffi::{CStr, CString, OsStr},
        fs::File,
        io::Read,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
        path::Component,
    };

    fn name(value: &OsStr) -> io::Result<CString> {
        CString::new(value.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in package path"))
    }

    fn open_at(parent: i32, value: &OsStr, directory: bool) -> io::Result<File> {
        let name = name(value)?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if directory {
                libc::O_DIRECTORY
            } else {
                libc::O_NONBLOCK
            };
        // SAFETY: name is NUL terminated; a successful openat returns an owned fd.
        let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this function is the sole owner of the newly opened descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_directory(path: &Path) -> io::Result<File> {
        let mut directory = open_at(
            libc::AT_FDCWD,
            OsStr::new(if path.is_absolute() { "/" } else { "." }),
            true,
        )?;
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(part) => directory = open_at(directory.as_raw_fd(), part, true)?,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "parent or prefix in package path",
                    ));
                }
            }
        }
        Ok(directory)
    }

    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            // SAFETY: the non-null stream was returned by fdopendir and is closed once.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }

    unsafe fn errno_ptr() -> *mut libc::c_int {
        #[cfg(target_os = "linux")]
        {
            unsafe { libc::__errno_location() }
        }
        #[cfg(target_os = "macos")]
        {
            unsafe { libc::__error() }
        }
    }

    fn entries(
        directory: &File,
        mut visit: impl FnMut(&OsStr) -> Result<(), CapabilityError>,
    ) -> Result<(), CapabilityError> {
        // SAFETY: fcntl duplicates a valid fd with close-on-exec set atomically.
        let fd = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: fdopendir consumes fd on success. On failure we close it below.
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(error.into());
        }
        let stream = DirectoryStream(raw);
        loop {
            // SAFETY: errno is thread-local and the stream stays live for this loop.
            let entry = unsafe {
                *errno_ptr() = 0;
                libc::readdir(stream.0)
            };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error.into());
                }
                break;
            }
            // SAFETY: readdir's entry is live until the next readdir; d_name is NUL terminated.
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            visit(OsStr::from_bytes(bytes))?;
        }
        Ok(())
    }

    pub(super) fn discover(root: &Path) -> Result<Vec<PathBuf>, CapabilityError> {
        // Resolve the caller's parent location once (e.g. macOS /var ->
        // /private/var). The configured root itself and every descendant are
        // subsequently opened without following links. Store resolved paths so
        // later paging never depends on cwd or the original parent aliases.
        let normalized: PathBuf = root.components().collect();
        let root = match normalized.file_name() {
            Some(name) => {
                let parent = normalized
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                match std::fs::canonicalize(parent) {
                    Ok(parent) => parent.join(name),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                    Err(error) => return Err(error.into()),
                }
            }
            None if normalized.as_os_str().is_empty()
                || normalized == Path::new("/")
                || normalized == Path::new(".") =>
            {
                std::fs::canonicalize(root)?
            }
            None => return Err(invalid("invalid package root")),
        };
        let directory = match open_directory(&root) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut packages = BTreeSet::new();
        let mut count = 0;
        walk(&directory, &root, 0, &mut count, &mut packages)?;
        Ok(packages.into_iter().collect())
    }

    fn walk(
        directory: &File,
        path: &Path,
        depth: usize,
        count: &mut usize,
        packages: &mut BTreeSet<PathBuf>,
    ) -> Result<(), CapabilityError> {
        entries(directory, |name| {
            charge(count, 1, MAX_DISCOVERY_ENTRIES, "discovery entries")?;
            let c_name = self::name(name)?;
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: valid directory/name and writable stat storage, with no-follow semantics.
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    c_name.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(io::Error::last_os_error().into());
            }
            // SAFETY: successful fstatat initialized the complete stat value.
            let mode = unsafe { metadata.assume_init() }.st_mode;

            let kind = mode & libc::S_IFMT;
            if kind == libc::S_IFLNK {
                return Err(invalid("symlink in package tree"));
            }
            if kind == libc::S_IFDIR {
                ensure_limit("discovery depth", depth + 1, MAX_DISCOVERY_DEPTH)?;
                let child = open_at(directory.as_raw_fd(), name, true)?;
                walk(&child, &path.join(name), depth + 1, count, packages)?;
            } else if name == OsStr::new(PACKAGE_HEADER_FILENAME)
                || name == OsStr::new("capability.toml")
            {
                if kind != libc::S_IFREG {
                    return Err(invalid("package metadata is not a regular file"));
                }
                if !packages.contains(path) {
                    ensure_limit("package count", packages.len() + 1, MAX_PACKAGE_COUNT)?;
                    packages.insert(path.to_path_buf());
                }
            }
            Ok(())
        })
    }

    pub(super) fn read_optional(
        path: &Path,
        maximum: usize,
    ) -> Result<Option<Vec<u8>>, CapabilityError> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("metadata has no parent directory"))?;
        let directory = open_directory(parent)?;
        let file = match open_at(
            directory.as_raw_fd(),
            path.file_name()
                .ok_or_else(|| invalid("metadata filename missing"))?,
            false,
        ) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("package metadata is not a regular file"));
        }
        if metadata.len() > maximum as u64 {
            return Err(CapabilityError::PackageLimit {
                kind: "file bytes",
                maximum,
            });
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
        ensure_limit("file bytes", bytes.len(), maximum)?;
        Ok(Some(bytes))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn traversal_respects_previously_consumed_entry_and_package_budgets() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().canonicalize().unwrap();
            std::fs::write(path.join(PACKAGE_HEADER_FILENAME), "{}").unwrap();
            let mut packages: BTreeSet<PathBuf> = (0..MAX_PACKAGE_COUNT - 1)
                .map(|n| PathBuf::from(format!("already-visited-{n}")))
                .collect();
            let mut count = MAX_DISCOVERY_ENTRIES - 1;
            walk(
                &open_directory(&path).unwrap(),
                &path,
                0,
                &mut count,
                &mut packages,
            )
            .unwrap();
            assert_eq!(count, MAX_DISCOVERY_ENTRIES);
            assert_eq!(packages.len(), MAX_PACKAGE_COUNT);
            std::fs::write(path.join("ignored"), "").unwrap();
            count = MAX_DISCOVERY_ENTRIES - 1;
            assert!(matches!(
                walk(
                    &open_directory(&path).unwrap(),
                    &path,
                    0,
                    &mut count,
                    &mut packages
                ),
                Err(CapabilityError::PackageLimit {
                    kind: "discovery entries",
                    ..
                })
            ));
            assert_eq!(count, MAX_DISCOVERY_ENTRIES);
            std::fs::create_dir(path.join("next-package")).unwrap();
            std::fs::write(
                path.join("next-package").join(PACKAGE_HEADER_FILENAME),
                "{}",
            )
            .unwrap();
            assert!(matches!(
                walk(
                    &open_directory(&path).unwrap(),
                    &path,
                    0,
                    &mut 0,
                    &mut packages
                ),
                Err(CapabilityError::PackageLimit {
                    kind: "package count",
                    ..
                })
            ));
            assert_eq!(packages.len(), MAX_PACKAGE_COUNT);
            assert!(!packages.contains(&path.join("next-package")));
        }
    }
}

pub(super) fn read_required(path: &Path, maximum: usize) -> Result<Vec<u8>, CapabilityError> {
    read_optional(path, maximum)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "package metadata missing").into())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn discover(root: &Path) -> Result<Vec<PathBuf>, CapabilityError> {
    unix::discover(root)
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn read_optional(
    path: &Path,
    maximum: usize,
) -> Result<Option<Vec<u8>>, CapabilityError> {
    unix::read_optional(path, maximum)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn discover(_: &Path) -> Result<Vec<PathBuf>, CapabilityError> {
    Err(super::invalid(
        "unsupported descriptor-safe package filesystem",
    ))
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn read_optional(_: &Path, _: usize) -> Result<Option<Vec<u8>>, CapabilityError> {
    Err(super::invalid(
        "unsupported descriptor-safe package filesystem",
    ))
}
