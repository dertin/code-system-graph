//! Capability-scoped repository root used for marker-owned host file installation.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use crate::types::HookError;

/// Maximum host configuration payload accepted during installation.
pub const MAX_HOST_FILE_BYTES: usize = 16 * 1024 * 1024;

/// Handles scoped directory operations relative to one canonical repository root.
pub struct ManagedRoot {
    root: PathBuf,
    #[cfg(unix)]
    directory: File,
}

impl ManagedRoot {
    /// Opens one canonical repository root without following a symlink root.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] when the root cannot be opened safely.
    pub fn open(root: &Path) -> Result<Self, HookError> {
        let canonical = fs::canonicalize(root).map_err(|source| HookError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let metadata = fs::symlink_metadata(&canonical).map_err(|source| HookError::Io {
            path: canonical.clone(),
            source,
        })?;
        if is_symlink_or_reparse_point(&metadata) {
            return Err(HookError::InvalidConfiguration {
                path: canonical,
                message: "repository root is a symbolic link or reparse point".to_owned(),
            });
        }
        if !metadata.is_dir() {
            return Err(HookError::InvalidConfiguration {
                path: canonical,
                message: "repository root is not a directory".to_owned(),
            });
        }
        #[cfg(unix)]
        let directory = open_directory_nofollow(&canonical).map_err(|source| HookError::Io {
            path: canonical.clone(),
            source,
        })?;
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

    /// Returns the absolute path for one validated relative path.
    #[must_use]
    pub fn absolute(&self, relative: &Path) -> PathBuf {
        self.root.join(relative)
    }

    /// Returns whether a regular file exists at a relative path.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] when the relative path is invalid or resolves to an unsafe entry.
    pub fn regular_file_exists(&self, relative: &Path) -> Result<bool, HookError> {
        validate_relative_path(relative, &self.root).map_err(map_validation_error)?;
        let path = self.root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if is_symlink_or_reparse_point(&metadata) {
                    return Err(HookError::InvalidConfiguration {
                        path,
                        message: "managed path is a symbolic link or reparse point".to_owned(),
                    });
                }
                Ok(metadata.is_file())
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(HookError::Io { path, source }),
        }
    }

    /// Returns whether an entry exists at a relative path.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] when the relative path is invalid or resolves to an unsafe entry.
    pub fn entry_exists(&self, relative: &Path) -> Result<bool, HookError> {
        validate_relative_path(relative, &self.root).map_err(map_validation_error)?;
        let path = self.root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if is_symlink_or_reparse_point(&metadata) {
                    return Err(HookError::InvalidConfiguration {
                        path,
                        message: "managed path is a symbolic link or reparse point".to_owned(),
                    });
                }
                Ok(true)
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(HookError::Io { path, source }),
        }
    }

    /// Returns whether a relative path is a directory.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] when the relative path is invalid or resolves to an unsafe entry.
    pub fn is_directory(&self, relative: &Path) -> Result<bool, HookError> {
        validate_relative_path(relative, &self.root).map_err(map_validation_error)?;
        let path = self.root.join(relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(HookError::Io { path, source }),
        };
        if is_symlink_or_reparse_point(&metadata) {
            return Err(HookError::InvalidConfiguration {
                path,
                message: "managed path is a symbolic link or reparse point".to_owned(),
            });
        }
        Ok(metadata.is_dir())
    }

    /// Reads a bounded UTF-8 file relative to the authorized root.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] when the relative path is invalid, the file is unsafe, or the read
    /// exceeds `max_bytes`.
    pub fn read_utf8_bounded(
        &self,
        relative: &Path,
        max_bytes: usize,
    ) -> Result<String, HookError> {
        let bytes = self.read_bytes_bounded(relative, max_bytes)?;
        String::from_utf8(bytes).map_err(|_| HookError::InvalidConfiguration {
            path: self.root.join(relative),
            message: "managed file is not valid UTF-8".to_owned(),
        })
    }

    /// Reads a bounded file relative to the authorized root when it exists.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] when the relative path is invalid, the file is unsafe, or the read
    /// exceeds `max_bytes`.
    pub fn read_optional_utf8_bounded(
        &self,
        relative: &Path,
        max_bytes: usize,
    ) -> Result<Option<String>, HookError> {
        if !self.regular_file_exists(relative)? {
            return Ok(None);
        }
        self.read_utf8_bounded(relative, max_bytes).map(Some)
    }

    /// Atomically writes bytes to a relative file, creating parent directories as needed.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] when the relative path is invalid, a parent is unsafe, or the write
    /// fails.
    pub fn atomic_write(&self, relative: &Path, bytes: &[u8]) -> Result<(), HookError> {
        validate_relative_path(relative, &self.root).map_err(map_validation_error)?;
        #[cfg(unix)]
        {
            let (parent, file_name) = split_relative(relative).map_err(map_validation_error)?;
            let parent_dir = descend_unix(&self.directory, &self.root, parent, true)
                .map_err(map_io_error(&self.root.join(parent)))?;
            atomic_write_unix(&parent_dir, &self.root.join(parent), file_name, bytes)
                .map_err(map_io_error(&self.root.join(relative)))?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            atomic_write_portable(&self.root, relative, bytes)
                .map_err(map_io_error(&self.root.join(relative)))
        }
    }

    /// Removes a regular file relative to the authorized root when it exists.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] when the relative path is invalid or the target is unsafe.
    pub fn remove_file_if_exists(&self, relative: &Path) -> Result<bool, HookError> {
        validate_relative_path(relative, &self.root).map_err(map_validation_error)?;
        #[cfg(unix)]
        {
            let (parent, file_name) = split_relative(relative).map_err(map_validation_error)?;
            let parent_dir = match descend_unix(&self.directory, &self.root, parent, false) {
                Ok(directory) => directory,
                Err(CapabilityIoError::Io { source })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Ok(false);
                }
                Err(error) => return Err(map_io_error(&self.root.join(parent))(error)),
            };
            remove_file_unix(&parent_dir, &self.root.join(parent), file_name)
                .map_err(map_io_error(&self.root.join(relative)))
        }
        #[cfg(not(unix))]
        {
            remove_file_portable(&self.root, relative)
                .map_err(map_io_error(&self.root.join(relative)))
        }
    }
}

#[derive(Debug)]
enum ValidationError {
    InvalidRelativePath { path: PathBuf },
    OutsideRoot { path: PathBuf, root: PathBuf },
}

fn map_validation_error(error: ValidationError) -> HookError {
    match error {
        ValidationError::InvalidRelativePath { path } => HookError::InvalidConfiguration {
            path,
            message: "managed relative path is invalid".to_owned(),
        },
        ValidationError::OutsideRoot { path, root } => HookError::InvalidConfiguration {
            path: root.join(path),
            message: "managed path escapes the authorized repository root".to_owned(),
        },
    }
}

fn map_io_error(path: &Path) -> impl Fn(CapabilityIoError) -> HookError + '_ {
    move |error| match error {
        CapabilityIoError::Io { source } => HookError::Io {
            path: path.to_path_buf(),
            source,
        },
        CapabilityIoError::Symlink => HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: "managed path is a symbolic link or reparse point".to_owned(),
        },
        CapabilityIoError::NotRegularFile => HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: "managed path is not a regular file".to_owned(),
        },
        CapabilityIoError::TooLarge { limit } => HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: format!("managed file exceeds {limit} bytes"),
        },
    }
}

#[derive(Debug)]
enum CapabilityIoError {
    Io {
        source: std::io::Error,
    },
    #[allow(dead_code)]
    Symlink,
    NotRegularFile,
    TooLarge {
        limit: usize,
    },
}

fn validate_relative_path(relative: &Path, root: &Path) -> Result<(), ValidationError> {
    if relative.is_absolute() {
        return Err(ValidationError::InvalidRelativePath {
            path: relative.to_path_buf(),
        });
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(ValidationError::InvalidRelativePath {
                    path: relative.to_path_buf(),
                });
            }
        }
    }
    let joined = root.join(relative);
    if let Ok(canonical) = fs::canonicalize(&joined)
        && !canonical.starts_with(root)
    {
        return Err(ValidationError::OutsideRoot {
            path: relative.to_path_buf(),
            root: root.to_path_buf(),
        });
    }
    Ok(())
}

fn split_relative(relative: &Path) -> Result<(&Path, &OsStr), ValidationError> {
    let file_name = relative
        .file_name()
        .ok_or_else(|| ValidationError::InvalidRelativePath {
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

#[cfg(unix)]
fn open_directory_nofollow(path: &Path) -> Result<File, std::io::Error> {
    use std::fs::OpenOptions;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(unix)]
fn descend_unix(
    directory: &File,
    root: &Path,
    relative: &Path,
    create: bool,
) -> Result<File, CapabilityIoError> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::{Mode, mkdirat};

    if relative.as_os_str().is_empty() {
        return open_directory_nofollow(root).map_err(|source| CapabilityIoError::Io { source });
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
                    CapabilityIoError::Io {
                        source: source.into(),
                    }
                })?;
                let fd = openat(parent, name, flags, Mode::empty()).map_err(|source| {
                    CapabilityIoError::Io {
                        source: source.into(),
                    }
                })?;
                File::from(fd)
            }
            Err(source) => {
                return Err(CapabilityIoError::Io {
                    source: source.into(),
                });
            }
        };
        current_path.push(name);
        handle = Some(opened);
    }

    handle.ok_or(CapabilityIoError::Io {
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "managed relative path is invalid",
        ),
    })
}

#[cfg(unix)]
fn read_bytes_bounded_unix(
    directory: &File,
    root: &Path,
    relative: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, CapabilityIoError> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::{Mode, SFlag, fstat};

    let (parent, file_name) = split_relative(relative).map_err(|error| CapabilityIoError::Io {
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string()),
    })?;
    let _parent_path = root.join(parent);
    let parent_dir = descend_unix(directory, root, parent, false)?;
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let fd = openat(parent_dir, file_name, flags, Mode::empty()).map_err(|source| {
        CapabilityIoError::Io {
            source: source.into(),
        }
    })?;
    let metadata = fstat(&fd).map_err(|source| CapabilityIoError::Io {
        source: source.into(),
    })?;
    if !SFlag::from_bits_truncate(metadata.st_mode).contains(SFlag::S_IFREG) {
        return Err(CapabilityIoError::NotRegularFile);
    }
    let size = usize::try_from(metadata.st_size)
        .map_err(|_| CapabilityIoError::TooLarge { limit: max_bytes })?;
    if size > max_bytes {
        return Err(CapabilityIoError::TooLarge { limit: max_bytes });
    }
    let file = File::from(fd);
    read_file_to_end_bounded(file, max_bytes)
}

fn read_file_to_end_bounded(
    mut reader: impl Read,
    max_bytes: usize,
) -> Result<Vec<u8>, CapabilityIoError> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|source| CapabilityIoError::Io { source })?;
        if read == 0 {
            break;
        }
        if buffer.len() + read > max_bytes {
            return Err(CapabilityIoError::TooLarge { limit: max_bytes });
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok(buffer)
}

impl ManagedRoot {
    fn read_bytes_bounded(&self, relative: &Path, max_bytes: usize) -> Result<Vec<u8>, HookError> {
        validate_relative_path(relative, &self.root).map_err(map_validation_error)?;
        #[cfg(unix)]
        {
            read_bytes_bounded_unix(&self.directory, &self.root, relative, max_bytes)
                .map_err(map_io_error(&self.root.join(relative)))
        }
        #[cfg(not(unix))]
        {
            read_bytes_bounded_portable(&self.root, relative, max_bytes)
                .map_err(map_io_error(&self.root.join(relative)))
        }
    }
}

#[cfg(unix)]
fn atomic_write_unix(
    parent: &File,
    _parent_path: &Path,
    file_name: &OsStr,
    bytes: &[u8],
) -> Result<(), CapabilityIoError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    use nix::fcntl::{OFlag, openat, renameat};
    use nix::sys::stat::Mode;
    use nix::unistd::{UnlinkatFlags, unlinkat};

    let stamp =
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CapabilityIoError::Io {
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
    .map_err(|source| CapabilityIoError::Io {
        source: source.into(),
    })?;
    let mut file = File::from(fd);
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| CapabilityIoError::Io { source })?;
    if let Err(source) = renameat(parent, temp_name.as_str(), parent, file_name) {
        let _ = unlinkat(parent, temp_name.as_str(), UnlinkatFlags::NoRemoveDir);
        return Err(CapabilityIoError::Io {
            source: source.into(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn remove_file_unix(
    parent: &File,
    _parent_path: &Path,
    file_name: &OsStr,
) -> Result<bool, CapabilityIoError> {
    use nix::unistd::{UnlinkatFlags, unlinkat};

    match unlinkat(parent, file_name, UnlinkatFlags::NoRemoveDir) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::ENOENT) => Ok(false),
        Err(source) => Err(CapabilityIoError::Io {
            source: source.into(),
        }),
    }
}

#[cfg(not(unix))]
fn read_bytes_bounded_portable(
    root: &Path,
    relative: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, CapabilityIoError> {
    let path = root.join(relative);
    let metadata =
        fs::symlink_metadata(&path).map_err(|source| CapabilityIoError::Io { source })?;
    if is_symlink_or_reparse_point(&metadata) {
        return Err(CapabilityIoError::Symlink);
    }
    if !metadata.is_file() {
        return Err(CapabilityIoError::NotRegularFile);
    }
    let size = metadata.len() as usize;
    if size > max_bytes {
        return Err(CapabilityIoError::TooLarge { limit: max_bytes });
    }
    let file = fs::File::open(&path).map_err(|source| CapabilityIoError::Io { source })?;
    read_file_to_end_bounded(file, max_bytes)
}

#[cfg(not(unix))]
fn atomic_write_portable(
    root: &Path,
    relative: &Path,
    bytes: &[u8],
) -> Result<(), CapabilityIoError> {
    use atomic_write_file::AtomicWriteFile;

    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CapabilityIoError::Io { source })?;
        let metadata =
            fs::symlink_metadata(parent).map_err(|source| CapabilityIoError::Io { source })?;
        if is_symlink_or_reparse_point(&metadata) {
            return Err(CapabilityIoError::Symlink);
        }
    }
    let mut destination =
        AtomicWriteFile::open(&path).map_err(|source| CapabilityIoError::Io { source })?;
    destination
        .write_all(bytes)
        .and_then(|()| destination.sync_all())
        .map_err(|source| CapabilityIoError::Io { source })?;
    destination
        .commit()
        .map_err(|source| CapabilityIoError::Io { source })
}

#[cfg(not(unix))]
fn remove_file_portable(root: &Path, relative: &Path) -> Result<bool, CapabilityIoError> {
    let path = root.join(relative);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(CapabilityIoError::Io { source }),
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRelativePath { path } => {
                write!(
                    formatter,
                    "managed relative path `{}` is invalid",
                    path.display()
                )
            }
            Self::OutsideRoot { path, root } => write!(
                formatter,
                "managed path `{}` escapes authorized root `{}`",
                path.display(),
                root.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};
    use std::path::Path;

    use super::{MAX_HOST_FILE_BYTES, ManagedRoot};

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
    fn managed_root_should_treat_missing_parent_as_absent_on_remove()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        let managed = ManagedRoot::open(repository.path())?;
        let removed =
            managed.remove_file_if_exists(Path::new(".code-system-graph/hooks/state.json"))?;
        assert!(!removed);
        Ok(())
    }

    #[test]
    fn read_file_to_end_bounded_should_survive_short_reads() {
        let payload = b"{\"hooks\":{}}\n".repeat(4_096);
        let reader = ChunkedReader {
            inner: Cursor::new(payload.clone()),
            chunk_size: 11,
        };
        let read = super::read_file_to_end_bounded(reader, payload.len())
            .expect("short reads should still return the full payload");
        assert_eq!(read, payload);
    }

    #[test]
    fn read_file_to_end_bounded_should_reject_overflow_after_short_reads() {
        let payload = vec![b'#'; MAX_HOST_FILE_BYTES + 32];
        let reader = ChunkedReader {
            inner: Cursor::new(payload),
            chunk_size: 19,
        };
        let result = super::read_file_to_end_bounded(reader, MAX_HOST_FILE_BYTES);
        assert!(matches!(
            result,
            Err(super::CapabilityIoError::TooLarge { .. })
        ));
    }
}
