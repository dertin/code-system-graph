//! Capability-scoped directory access that never follows symbolic links.

use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Maximum repository-local configuration size accepted from analyzed checkouts.
pub const MAX_REPOSITORY_CONFIG_BYTES: usize = 1024 * 1024;

/// Error returned while performing capability-scoped filesystem access.
#[derive(Debug, Error)]
pub enum CapabilityError {
    /// A filesystem operation failed.
    #[error("failed to access `{path}`: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// A symbolic link or reparse point was encountered.
    #[error("`{path}` is a symbolic link or reparse point")]
    Symlink {
        /// Rejected path.
        path: PathBuf,
    },
    /// The target is not a regular file.
    #[error("`{path}` is not a regular file")]
    NotRegularFile {
        /// Rejected path.
        path: PathBuf,
    },
    /// The target is not a directory.
    #[error("`{path}` is not a directory")]
    NotDirectory {
        /// Rejected path.
        path: PathBuf,
    },
    /// A relative path escapes the authorized root.
    #[error("`{path}` escapes the authorized root `{root}`")]
    OutsideRoot {
        /// Requested relative path.
        path: PathBuf,
        /// Canonical authorized root.
        root: PathBuf,
    },
    /// A relative path is malformed.
    #[error("relative path `{path}` is invalid")]
    InvalidRelativePath {
        /// Rejected relative path.
        path: PathBuf,
    },
    /// A file exceeds the configured byte limit.
    #[error("`{path}` exceeds {limit} bytes")]
    TooLarge {
        /// Rejected path.
        path: PathBuf,
        /// Maximum accepted size in bytes.
        limit: usize,
    },
    /// File contents are not valid UTF-8.
    #[error("`{path}` is not valid UTF-8")]
    InvalidUtf8 {
        /// Rejected path.
        path: PathBuf,
    },
}

/// Handles scoped directory operations relative to a canonical checkout root.
pub struct CapabilityDir {
    root: PathBuf,
    #[cfg(unix)]
    directory: File,
}

/// Classification of a repository-relative path for regular-file reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegularFileEntry {
    /// No entry exists at the relative path.
    Absent,
    /// A regular file exists at the relative path.
    Regular,
}

impl CapabilityDir {
    /// Opens one canonical directory without following a symlink root.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError`] when the root cannot be canonicalized, is not a directory, or is
    /// a symbolic link.
    pub fn open(root: &Path) -> Result<Self, CapabilityError> {
        let canonical = fs::canonicalize(root).map_err(|source| CapabilityError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let metadata = fs::symlink_metadata(&canonical).map_err(|source| CapabilityError::Io {
            path: canonical.clone(),
            source,
        })?;
        if is_symlink_or_reparse_point(&metadata) {
            return Err(CapabilityError::Symlink { path: canonical });
        }
        if !metadata.is_dir() {
            return Err(CapabilityError::NotDirectory { path: canonical });
        }
        #[cfg(unix)]
        let directory = open_directory_nofollow(&canonical)?;
        Ok(Self {
            root: canonical,
            #[cfg(unix)]
            directory,
        })
    }

    /// Returns the canonical authorized root path.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reads a bounded UTF-8 file relative to the authorized root.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError`] when the relative path is invalid, the file is unsafe, or the
    /// read exceeds `max_bytes`.
    pub fn read_utf8_file_bounded(
        &self,
        relative: &Path,
        max_bytes: usize,
    ) -> Result<String, CapabilityError> {
        let bytes = self.read_file_bounded(relative, max_bytes)?;
        String::from_utf8(bytes).map_err(|_| CapabilityError::InvalidUtf8 {
            path: self.root.join(relative),
        })
    }

    /// Reads a bounded file relative to the authorized root.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError`] when the relative path is invalid, the file is unsafe, or the
    /// read exceeds `max_bytes`.
    pub fn read_file_bounded(
        &self,
        relative: &Path,
        max_bytes: usize,
    ) -> Result<Vec<u8>, CapabilityError> {
        validate_relative_path(relative, &self.root)?;
        let joined = self.root.join(relative);
        let metadata = fs::symlink_metadata(&joined).map_err(|source| CapabilityError::Io {
            path: joined.clone(),
            source,
        })?;
        if is_symlink_or_reparse_point(&metadata) {
            return Err(CapabilityError::Symlink { path: joined });
        }
        if !metadata.is_file() {
            return Err(CapabilityError::NotRegularFile { path: joined });
        }
        #[cfg(unix)]
        {
            read_file_bounded_unix(&self.directory, &self.root, relative, max_bytes)
        }
        #[cfg(not(unix))]
        {
            read_file_bounded_portable(&self.root, relative, max_bytes)
        }
    }

    /// Classifies whether a relative path is absent, a regular file, or an unsafe non-regular entry.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError`] when the relative path is invalid, a symlink, or unreadable.
    pub fn classify_regular_file_entry(
        &self,
        relative: &Path,
    ) -> Result<RegularFileEntry, CapabilityError> {
        validate_relative_path(relative, &self.root)?;
        let path = self.root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if is_symlink_or_reparse_point(&metadata) {
                    return Err(CapabilityError::Symlink { path });
                }
                if metadata.is_file() {
                    Ok(RegularFileEntry::Regular)
                } else {
                    Err(CapabilityError::NotRegularFile { path })
                }
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                Ok(RegularFileEntry::Absent)
            }
            Err(source) => Err(CapabilityError::Io { path, source }),
        }
    }

    /// Returns whether a regular file exists at a relative path.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError`] when the relative path is invalid or resolves to an unsafe
    /// entry.
    pub fn regular_file_exists(&self, relative: &Path) -> Result<bool, CapabilityError> {
        validate_relative_path(relative, &self.root)?;
        let path = self.root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if is_symlink_or_reparse_point(&metadata) {
                    return Err(CapabilityError::Symlink { path });
                }
                Ok(metadata.is_file())
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(CapabilityError::Io { path, source }),
        }
    }

    /// Atomically writes bytes to a relative file, creating parent directories as needed.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError`] when the relative path is invalid, a parent is unsafe, or the
    /// write fails.
    pub fn atomic_write(&self, relative: &Path, bytes: &[u8]) -> Result<(), CapabilityError> {
        validate_relative_path(relative, &self.root)?;
        #[cfg(unix)]
        {
            let (parent, file_name) = split_relative(relative)?;
            let parent_dir = descend_unix(&self.directory, &self.root, parent, true)?;
            atomic_write_unix(&parent_dir, &self.root.join(parent), file_name, bytes)
        }
        #[cfg(not(unix))]
        {
            atomic_write_portable(&self.root, relative, bytes)
        }
    }

    /// Removes a regular file relative to the authorized root when it exists.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError`] when the relative path is invalid or the target is unsafe.
    pub fn remove_file_if_exists(&self, relative: &Path) -> Result<bool, CapabilityError> {
        validate_relative_path(relative, &self.root)?;
        #[cfg(unix)]
        {
            let (parent, file_name) = split_relative(relative)?;
            let parent_dir = match descend_unix(&self.directory, &self.root, parent, false) {
                Ok(directory) => directory,
                Err(CapabilityError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Ok(false);
                }
                Err(error) => return Err(error),
            };
            remove_file_unix(&parent_dir, &self.root.join(parent), file_name)
        }
        #[cfg(not(unix))]
        {
            remove_file_portable(&self.root, relative)
        }
    }
}

fn validate_relative_path(relative: &Path, root: &Path) -> Result<(), CapabilityError> {
    if relative.is_absolute() {
        return Err(CapabilityError::InvalidRelativePath {
            path: relative.to_path_buf(),
        });
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(CapabilityError::InvalidRelativePath {
                    path: relative.to_path_buf(),
                });
            }
        }
    }
    let joined = root.join(relative);
    if let Ok(canonical) = fs::canonicalize(&joined)
        && !canonical.starts_with(root)
    {
        return Err(CapabilityError::OutsideRoot {
            path: relative.to_path_buf(),
            root: root.to_path_buf(),
        });
    }
    Ok(())
}

fn split_relative(relative: &Path) -> Result<(&Path, &OsStr), CapabilityError> {
    let file_name = relative
        .file_name()
        .ok_or_else(|| CapabilityError::InvalidRelativePath {
            path: relative.to_path_buf(),
        })?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    Ok((parent, file_name))
}

fn is_symlink_or_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink() || {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 != 0
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

/// Walks one relative directory chain without following symbolic links or reparse points.
#[cfg_attr(unix, allow(dead_code))]
fn walk_directory_chain(
    root: &Path,
    relative: &Path,
    create: bool,
) -> Result<PathBuf, CapabilityError> {
    if relative.as_os_str().is_empty() {
        return Ok(root.to_path_buf());
    }

    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if is_symlink_or_reparse_point(&metadata) {
                    return Err(CapabilityError::Symlink {
                        path: current.clone(),
                    });
                }
                if !metadata.is_dir() {
                    return Err(CapabilityError::NotDirectory { path: current });
                }
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Err(CapabilityError::Io {
                        path: current.clone(),
                        source,
                    });
                }
                fs::create_dir(&current).map_err(|source| CapabilityError::Io {
                    path: current.clone(),
                    source,
                })?;
                let metadata =
                    fs::symlink_metadata(&current).map_err(|source| CapabilityError::Io {
                        path: current.clone(),
                        source,
                    })?;
                if is_symlink_or_reparse_point(&metadata) {
                    return Err(CapabilityError::Symlink { path: current });
                }
                if !metadata.is_dir() {
                    return Err(CapabilityError::NotDirectory { path: current });
                }
            }
            Err(source) => {
                return Err(CapabilityError::Io {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn open_directory_nofollow(path: &Path) -> Result<File, CapabilityError> {
    use std::fs::OpenOptions;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| CapabilityError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn descend_unix(
    directory: &File,
    root: &Path,
    relative: &Path,
    create: bool,
) -> Result<File, CapabilityError> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::{Mode, mkdirat};

    if relative.as_os_str().is_empty() {
        return open_directory_nofollow(root);
    }

    let mut current_path = root.to_path_buf();
    let mut handle = None::<File>;

    for component in relative.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let parent = handle.as_ref().unwrap_or(directory);
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let opened = match openat(parent, name, flags, Mode::empty()) {
            Ok(fd) => File::from(fd),
            Err(nix::errno::Errno::ENOENT) if create => {
                mkdirat(parent, name, Mode::from_bits_truncate(0o700)).map_err(|source| {
                    CapabilityError::Io {
                        path: current_path.join(name),
                        source: source.into(),
                    }
                })?;
                let fd = openat(parent, name, flags, Mode::empty()).map_err(|source| {
                    CapabilityError::Io {
                        path: current_path.join(name),
                        source: source.into(),
                    }
                })?;
                File::from(fd)
            }
            Err(source) => {
                return Err(CapabilityError::Io {
                    path: current_path.join(name),
                    source: source.into(),
                });
            }
        };
        current_path.push(name);
        handle = Some(opened);
    }

    handle.ok_or_else(|| CapabilityError::InvalidRelativePath {
        path: relative.to_path_buf(),
    })
}

#[cfg(unix)]
fn read_file_bounded_unix(
    directory: &File,
    root: &Path,
    relative: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, CapabilityError> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::{Mode, SFlag, fstat};

    let (parent, file_name) = split_relative(relative)?;
    let parent_path = root.join(parent);
    let parent_dir = descend_unix(directory, root, parent, false)?;
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let fd = match openat(parent_dir, file_name, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(source) => {
            let path = parent_path.join(file_name);
            if fs::symlink_metadata(&path)
                .is_ok_and(|metadata| is_symlink_or_reparse_point(&metadata))
            {
                return Err(CapabilityError::Symlink { path });
            }
            return Err(CapabilityError::Io {
                path,
                source: source.into(),
            });
        }
    };
    let metadata = fstat(&fd).map_err(|source| CapabilityError::Io {
        path: parent_path.join(file_name),
        source: source.into(),
    })?;
    if !SFlag::from_bits_truncate(metadata.st_mode).contains(SFlag::S_IFREG) {
        return Err(CapabilityError::NotRegularFile {
            path: parent_path.join(file_name),
        });
    }
    let size = usize::try_from(metadata.st_size).map_err(|_| CapabilityError::TooLarge {
        path: parent_path.join(file_name),
        limit: max_bytes,
    })?;
    if size > max_bytes {
        return Err(CapabilityError::TooLarge {
            path: parent_path.join(file_name),
            limit: max_bytes,
        });
    }
    let file = File::from(fd);
    read_file_to_end_bounded(file, &parent_path.join(file_name), max_bytes)
}

fn read_file_to_end_bounded(
    mut reader: impl Read,
    path: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, CapabilityError> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|source| CapabilityError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        if buffer.len() + read > max_bytes {
            return Err(CapabilityError::TooLarge {
                path: path.to_path_buf(),
                limit: max_bytes,
            });
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok(buffer)
}

#[cfg(unix)]
fn atomic_write_unix(
    parent: &File,
    parent_path: &Path,
    file_name: &OsStr,
    bytes: &[u8],
) -> Result<(), CapabilityError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    use nix::fcntl::{OFlag, openat, renameat};
    use nix::sys::stat::Mode;
    use nix::unistd::{UnlinkatFlags, unlinkat};

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CapabilityError::Io {
            path: parent_path.join(file_name),
            source: std::io::Error::other("system clock is earlier than the Unix epoch"),
        })?;
    let temp_name = format!(
        ".{}.tmp.code-system-graph.{}-{}",
        file_name.to_string_lossy(),
        stamp.as_secs(),
        stamp.subsec_nanos()
    );
    let create_flags =
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let fd = openat(
        parent,
        temp_name.as_str(),
        create_flags,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|source| CapabilityError::Io {
        path: parent_path.join(&temp_name),
        source: source.into(),
    })?;
    let mut file = File::from(fd);
    file.write_all(bytes)
        .map_err(|source| CapabilityError::Io {
            path: parent_path.join(&temp_name),
            source,
        })?;
    file.sync_all().map_err(|source| CapabilityError::Io {
        path: parent_path.join(&temp_name),
        source,
    })?;
    if let Err(source) = renameat(parent, temp_name.as_str(), parent, file_name) {
        let _ = unlinkat(parent, temp_name.as_str(), UnlinkatFlags::NoRemoveDir);
        return Err(CapabilityError::Io {
            path: parent_path.join(file_name),
            source: source.into(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn remove_file_unix(
    parent: &File,
    parent_path: &Path,
    file_name: &OsStr,
) -> Result<bool, CapabilityError> {
    use nix::unistd::{UnlinkatFlags, unlinkat};

    match unlinkat(parent, file_name, UnlinkatFlags::NoRemoveDir) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::ENOENT) => Ok(false),
        Err(source) => Err(CapabilityError::Io {
            path: parent_path.join(file_name),
            source: source.into(),
        }),
    }
}

#[cfg(not(unix))]
fn read_file_bounded_portable(
    root: &Path,
    relative: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, CapabilityError> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).map_err(|source| CapabilityError::Io {
        path: path.clone(),
        source,
    })?;
    if is_symlink_or_reparse_point(&metadata) {
        return Err(CapabilityError::Symlink { path });
    }
    if !metadata.is_file() {
        return Err(CapabilityError::NotRegularFile { path });
    }
    let size = metadata.len() as usize;
    if size > max_bytes {
        return Err(CapabilityError::TooLarge {
            path,
            limit: max_bytes,
        });
    }
    let file = fs::File::open(&path).map_err(|source| CapabilityError::Io {
        path: path.clone(),
        source,
    })?;
    read_file_to_end_bounded(file, &path, max_bytes)
}

#[cfg(not(unix))]
fn atomic_write_portable(
    root: &Path,
    relative: &Path,
    bytes: &[u8],
) -> Result<(), CapabilityError> {
    use atomic_write_file::AtomicWriteFile;

    let (parent, file_name) = split_relative(relative)?;
    let parent_path = walk_directory_chain(root, parent, true)?;
    let path = parent_path.join(file_name);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if is_symlink_or_reparse_point(&metadata) {
            return Err(CapabilityError::Symlink { path: path.clone() });
        }
        if metadata.is_dir() {
            return Err(CapabilityError::NotRegularFile { path });
        }
    }
    let mut destination = AtomicWriteFile::open(&path).map_err(|source| CapabilityError::Io {
        path: path.clone(),
        source,
    })?;
    destination
        .write_all(bytes)
        .and_then(|()| destination.sync_all())
        .map_err(|source| CapabilityError::Io {
            path: path.clone(),
            source,
        })?;
    destination
        .commit()
        .map_err(|source| CapabilityError::Io { path, source })
}

#[cfg(not(unix))]
fn remove_file_portable(root: &Path, relative: &Path) -> Result<bool, CapabilityError> {
    let path = root.join(relative);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(CapabilityError::Io { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};
    use std::path::Path;

    use super::{CapabilityDir, CapabilityError, MAX_REPOSITORY_CONFIG_BYTES};

    struct ChunkedReader<R> {
        inner: R,
        chunk_size: usize,
    }

    impl<R: Read> Read for ChunkedReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let limit = buf.len().min(self.chunk_size);
            self.inner.read(&mut buf[..limit])
        }
    }

    #[test]
    fn walk_directory_chain_should_reject_intermediate_symlink()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("repo");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&outside)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join(".code-system-graph"))?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, root.join(".code-system-graph"))?;

        let result =
            super::walk_directory_chain(&root, Path::new(".code-system-graph/hooks"), true);

        assert!(matches!(result, Err(CapabilityError::Symlink { .. })));
        assert!(!outside.join("hooks").exists());
        Ok(())
    }

    #[test]
    fn capability_dir_should_reject_symlinked_config() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let checkout = temporary.path().join("checkout");
        let outside = temporary.path().join("outside.yaml");
        std::fs::create_dir_all(&checkout)?;
        std::fs::write(&outside, "version: 1\n")?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, checkout.join(".code-system-graph.yaml"))?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, checkout.join(".code-system-graph.yaml"))?;

        let root = CapabilityDir::open(&checkout)?;
        let result = root.read_utf8_file_bounded(
            Path::new(".code-system-graph.yaml"),
            MAX_REPOSITORY_CONFIG_BYTES,
        );
        assert!(
            matches!(
                result,
                Err(CapabilityError::Symlink { .. }
                    | CapabilityError::OutsideRoot { .. }
                    | CapabilityError::NotRegularFile { .. })
            ),
            "unexpected result: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn capability_dir_should_reject_oversized_config() -> Result<(), Box<dyn std::error::Error>> {
        let checkout = tempfile::tempdir()?;
        std::fs::write(
            checkout.path().join(".code-system-graph.yaml"),
            "x".repeat(MAX_REPOSITORY_CONFIG_BYTES + 1),
        )?;
        let root = CapabilityDir::open(checkout.path())?;
        let result = root.read_utf8_file_bounded(
            Path::new(".code-system-graph.yaml"),
            MAX_REPOSITORY_CONFIG_BYTES,
        );
        assert!(matches!(result, Err(CapabilityError::TooLarge { .. })));
        Ok(())
    }

    #[test]
    fn capability_dir_should_treat_missing_parent_as_absent_on_remove()
    -> Result<(), Box<dyn std::error::Error>> {
        let checkout = tempfile::tempdir()?;
        let root = CapabilityDir::open(checkout.path())?;
        let removed =
            root.remove_file_if_exists(Path::new(".code-system-graph/hooks/state.json"))?;
        assert!(!removed);
        Ok(())
    }

    #[test]
    fn read_file_to_end_bounded_should_survive_short_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let payload = b"version: 1\n".repeat(2_048);
        let reader = ChunkedReader {
            inner: Cursor::new(payload.clone()),
            chunk_size: 13,
        };
        let read =
            super::read_file_to_end_bounded(reader, Path::new("config.yaml"), payload.len())?;
        assert_eq!(read, payload);
        Ok(())
    }

    #[test]
    fn read_file_to_end_bounded_should_reject_overflow_after_short_reads() {
        let payload = vec![b'x'; MAX_REPOSITORY_CONFIG_BYTES + 64];
        let reader = ChunkedReader {
            inner: Cursor::new(payload),
            chunk_size: 17,
        };
        let result = super::read_file_to_end_bounded(
            reader,
            Path::new("config.yaml"),
            MAX_REPOSITORY_CONFIG_BYTES,
        );
        assert!(matches!(result, Err(CapabilityError::TooLarge { .. })));
    }
}
