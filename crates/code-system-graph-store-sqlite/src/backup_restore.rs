use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::backup::Backup;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension};
use same_file::Handle;

use super::{
    LATEST_SCHEMA_VERSION, RestoreReport, StoreError, StoreLock, open_read_only_connection, schema_version, validate_exact_schema
};
#[cfg(windows)]
use crate::file_permissions::current_user_sid_string;
use crate::file_permissions::{
    SQLITE_ARTIFACT_SUFFIXES, artifact_path, restrict_store_permissions
};

#[cfg(test)]
mod tests;

pub(super) fn backup_file(database_path: &Path, destination: &Path) -> Result<(), StoreError> {
    ensure_distinct_paths(database_path, destination)?;
    let source = ValidatedBackupSource::open(database_path)?;
    write_backup(
        destination,
        |destination| source.copy_to(destination),
        || source.finish(),
    )
}

pub(super) fn backup_connection(source: &Connection, destination: &Path) -> Result<(), StoreError> {
    write_backup(
        destination,
        |destination| backup_connection_to(source, destination),
        || Ok(()),
    )
}

fn write_backup(
    destination: &Path,
    copy: impl FnOnce(&mut Connection) -> Result<(), StoreError>,
    finish: impl FnOnce() -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let destination = canonical_destination_path(destination)?;
    let (staging, mut destination_connection) = StagedDatabase::create(&destination)?;
    let result = (|| {
        copy(&mut destination_connection)?;
        restrict_store_permissions(staging.path())
    })();
    drop(destination_connection);
    let result = result.and_then(|()| finish());
    match result {
        Ok(()) => staging.publish(&destination, PublishMode::CreateNew, None),
        Err(operation) => Err(staging.cleanup_error(operation)),
    }
}

fn copy_database_file_snapshot(
    source: &Path,
    destination: &Path,
) -> Result<blake3::Hash, StoreError> {
    let destination = canonical_destination_path(destination)?;
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let directory = OwnedStagingDirectory::create(parent).map_err(|source| StoreError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let staging = StagedDatabase {
        path: directory.path().join("database.sqlite"),
        directory,
    };
    let copy_result = (|| {
        let mut source_file = fs::File::open(source).map_err(|source_error| StoreError::Io {
            path: source.to_path_buf(),
            source: source_error,
        })?;
        let mut destination_file = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(staging.path())
            .map_err(|source| StoreError::Io {
                path: staging.path().to_path_buf(),
                source,
            })?;
        std::io::copy(&mut source_file, &mut destination_file).map_err(|source| {
            StoreError::Io {
                path: staging.path().to_path_buf(),
                source,
            }
        })?;
        destination_file
            .sync_all()
            .map_err(|source| StoreError::Io {
                path: staging.path().to_path_buf(),
                source,
            })?;
        drop(destination_file);
        restrict_store_permissions(staging.path())?;
        database_digest(staging.path()).map_err(|source| StoreError::Io {
            path: staging.path().to_path_buf(),
            source,
        })
    })();
    match copy_result {
        Ok(digest) => staging
            .publish(&destination, PublishMode::CreateNew, None)
            .map(|()| digest),
        Err(operation) => Err(staging.cleanup_error(operation)),
    }
}

#[derive(Clone, Copy)]
enum PublishMode {
    CreateNew,
    Replace,
}

struct StagedDatabase {
    directory: OwnedStagingDirectory,
    path: PathBuf,
}

struct DestinationSnapshot {
    identity: Handle,
    digest: blake3::Hash,
}

impl DestinationSnapshot {
    fn capture(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            identity: Handle::from_path(path)?,
            digest: database_digest(path)?,
        })
    }

    fn matches(&self, path: &Path) -> std::io::Result<bool> {
        Ok(self.identity == Handle::from_path(path)? && self.digest == database_digest(path)?)
    }

    fn same_as(&self, other: &Self) -> bool {
        self.identity == other.identity && self.digest == other.digest
    }
}

fn database_digest(path: &Path) -> std::io::Result<blake3::Hash> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hasher.finalize());
        }
        hasher.update(&buffer[..read]);
    }
}

struct OwnedStagingDirectory {
    path: PathBuf,
    cleanup: bool,
}

impl OwnedStagingDirectory {
    fn create(parent: &Path) -> std::io::Result<Self> {
        let named = tempfile::Builder::new()
            .prefix(".csgraph-stage-")
            .make_in(parent, |path| {
                create_private_staging_directory(path)?;
                Ok(())
            })?;
        let ((), path) = named.keep().map_err(|error| error.error)?;
        Ok(Self {
            path,
            cleanup: true,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn close(mut self) -> std::io::Result<()> {
        fs::remove_dir_all(&self.path)?;
        self.cleanup = false;
        Ok(())
    }

    fn keep(mut self) -> PathBuf {
        self.cleanup = false;
        self.path.clone()
    }
}

impl Drop for OwnedStagingDirectory {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(unix)]
fn create_private_staging_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn create_private_staging_directory(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

    fn wide(value: &std::ffi::OsStr) -> std::io::Result<Vec<u16>> {
        let mut wide = value.encode_wide().collect::<Vec<_>>();
        if wide.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "staging path contains a NUL code unit",
            ));
        }
        wide.push(0);
        Ok(wide)
    }

    let path = wide(path.as_os_str())?;
    let descriptor = format!("D:P(A;OICI;FA;;;{})", current_user_sid_string()?);
    let descriptor_text = wide(std::ffi::OsStr::new(&descriptor))?;
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the SDDL and output pointers are valid for the duration of each call.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: the path and security descriptor remain valid until the call returns.
    let created = unsafe { CreateDirectoryW(path.as_ptr(), &raw const attributes) };
    // SAFETY: the descriptor was allocated by the conversion API and must be released with LocalFree.
    unsafe {
        LocalFree(descriptor);
    }
    if created == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn create_private_staging_directory(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private staging directories are unsupported on this platform",
    ))
}

impl StagedDatabase {
    fn create(destination: &Path) -> Result<(Self, Connection), StoreError> {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let directory = OwnedStagingDirectory::create(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let path = directory.path().join("database.sqlite");
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        Ok((Self { directory, path }, connection))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn publish(
        self,
        destination: &Path,
        mode: PublishMode,
        expected: Option<&DestinationSnapshot>,
    ) -> Result<(), StoreError> {
        let publish_result = match mode {
            PublishMode::CreateNew => publish_new_database_file(&self.path, destination),
            PublishMode::Replace => match expected {
                Some(expected) => replace_database_file_if_same(
                    &self.path,
                    destination,
                    expected,
                    &self.directory.path().join("displaced.sqlite"),
                ),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "atomic replacement requires the original file identity",
                )),
            },
        };
        match publish_result {
            Ok(()) => {
                drop(self);
                Ok(())
            }
            Err(source) => {
                let operation = StoreError::Io {
                    path: destination.to_path_buf(),
                    source,
                };
                match mode {
                    PublishMode::CreateNew => Err(self.cleanup_error(operation)),
                    PublishMode::Replace => {
                        let _preserved_staging = self.directory.keep();
                        Err(operation)
                    }
                }
            }
        }
    }

    fn cleanup_error(self, operation: StoreError) -> StoreError {
        match self.directory.close() {
            Ok(()) => operation,
            Err(source) => StoreError::OperationCleanupFailed {
                operation: Box::new(operation),
                cleanup: Box::new(StoreError::Io {
                    path: self.path,
                    source,
                }),
            },
        }
    }
}

#[cfg(not(windows))]
fn publish_new_database_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::hard_link(source, destination)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn publish_new_database_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    fn wide(path: &Path) -> std::io::Result<Vec<u16>> {
        let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if wide.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path contains a NUL code unit",
            ));
        }
        wide.push(0);
        Ok(wide)
    }

    let source = wide(source)?;
    let destination = wide(destination)?;
    // SAFETY: both paths are valid NUL-terminated UTF-16 buffers for the duration of the call.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn canonical_destination_path(path: &Path) -> Result<PathBuf, StoreError> {
    let file_name = path.file_name().ok_or_else(|| StoreError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "database path must include a file name",
        ),
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| StoreError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let parent = fs::canonicalize(parent).map_err(|source| StoreError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    Ok(parent.join(file_name))
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn replace_database_file_if_same(
    replacement: &Path,
    destination: &Path,
    expected: &DestinationSnapshot,
    _displaced: &Path,
) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    fn exchange(left: &Path, right: &Path) -> std::io::Result<()> {
        let left = CString::new(left.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path contains a NUL byte",
            )
        })?;
        let right = CString::new(right.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path contains a NUL byte",
            )
        })?;
        // SAFETY: both C strings are NUL-terminated and remain valid for the syscall.
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                left.as_ptr(),
                libc::AT_FDCWD,
                right.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    exchange(replacement, destination)?;
    let displaced = match DestinationSnapshot::capture(replacement) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return match exchange(replacement, destination) {
                Ok(()) => Err(error),
                Err(rollback) => Err(std::io::Error::new(
                    rollback.kind(),
                    format!(
                        "failed to inspect displaced database: {error}; \
                         atomic replacement rollback also failed: {rollback}"
                    ),
                )),
            };
        }
    };
    if expected.same_as(&displaced) {
        return Ok(());
    }
    exchange(replacement, destination)?;
    if !displaced.matches(destination)? {
        return Err(std::io::Error::other(
            "atomic replacement rollback did not restore the displaced database",
        ));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "database destination changed during atomic replacement",
    ))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_replace_file(
    destination: &Path,
    replacement: &Path,
    displaced: &Path,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW};

    fn wide(path: &Path) -> std::io::Result<Vec<u16>> {
        let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if wide.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path contains a NUL code unit",
            ));
        }
        wide.push(0);
        Ok(wide)
    }

    let destination = wide(destination)?;
    let replacement = wide(replacement)?;
    let displaced = wide(displaced)?;
    // SAFETY: all paths are valid NUL-terminated UTF-16 buffers for the duration of the call.
    let result = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            replacement.as_ptr(),
            displaced.as_ptr(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn is_partial_windows_replace(error: &std::io::Error) -> bool {
    use windows_sys::Win32::Foundation::ERROR_UNABLE_TO_MOVE_REPLACEMENT_2;

    error.raw_os_error() == Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT_2.cast_signed())
}

#[cfg(windows)]
fn restore_displaced_windows_file(
    destination: &Path,
    displaced: &Path,
    backup: &Path,
    expected: &DestinationSnapshot,
) -> std::io::Result<()> {
    if let Err(error) = windows_replace_file(destination, displaced, backup) {
        if !is_partial_windows_replace(&error) {
            return Err(error);
        }
        publish_new_database_file(displaced, destination)?;
    }
    if expected.matches(destination)? {
        return Ok(());
    }
    if expected.matches(backup)?
        && let Err(error) = windows_replace_file(destination, backup, displaced)
    {
        if !is_partial_windows_replace(&error) {
            return Err(error);
        }
        publish_new_database_file(backup, destination)?;
    }
    if expected.matches(destination)? {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "atomic replacement rollback did not restore the displaced database",
        ))
    }
}

#[cfg(windows)]
fn replace_database_file_if_same(
    replacement: &Path,
    destination: &Path,
    expected: &DestinationSnapshot,
    displaced: &Path,
) -> std::io::Result<()> {
    if let Err(error) = windows_replace_file(destination, replacement, displaced) {
        if is_partial_windows_replace(&error) {
            publish_new_database_file(displaced, destination)?;
            if !expected.matches(destination)? {
                return Err(std::io::Error::other(
                    "partial atomic replacement recovery restored the wrong database",
                ));
            }
        }
        return Err(error);
    }
    let displaced_snapshot = match DestinationSnapshot::capture(displaced) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return match restore_displaced_windows_file(
                destination,
                displaced,
                replacement,
                expected,
            ) {
                Ok(()) => Err(error),
                Err(rollback) => Err(std::io::Error::new(
                    rollback.kind(),
                    format!(
                        "failed to inspect displaced database: {error}; \
                         atomic replacement rollback also failed: {rollback}"
                    ),
                )),
            };
        }
    };
    if expected.same_as(&displaced_snapshot) {
        return Ok(());
    }
    restore_displaced_windows_file(destination, displaced, replacement, &displaced_snapshot)?;
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "database destination changed during atomic replacement",
    ))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn replace_database_file_if_same(
    replacement: &Path,
    destination: &Path,
    expected: &DestinationSnapshot,
    _displaced: &Path,
) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    fn exchange(left: &Path, right: &Path) -> std::io::Result<()> {
        let left = CString::new(left.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path contains a NUL byte",
            )
        })?;
        let right = CString::new(right.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path contains a NUL byte",
            )
        })?;
        // SAFETY: both C strings are NUL-terminated and remain valid for the syscall.
        let result = unsafe { libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    exchange(replacement, destination)?;
    let displaced = match DestinationSnapshot::capture(replacement) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return match exchange(replacement, destination) {
                Ok(()) => Err(error),
                Err(rollback) => Err(std::io::Error::new(
                    rollback.kind(),
                    format!(
                        "failed to inspect displaced database: {error}; \
                         atomic replacement rollback also failed: {rollback}"
                    ),
                )),
            };
        }
    };
    if expected.same_as(&displaced) {
        return Ok(());
    }
    exchange(replacement, destination)?;
    if !displaced.matches(destination)? {
        return Err(std::io::Error::other(
            "atomic replacement rollback did not restore the displaced database",
        ));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "database destination changed during atomic replacement",
    ))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn replace_database_file_if_same(
    _replacement: &Path,
    _destination: &Path,
    _expected: &DestinationSnapshot,
    _displaced: &Path,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic in-place database replacement is unsupported on this Unix platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn replace_database_file_if_same(
    _replacement: &Path,
    _destination: &Path,
    _expected: &DestinationSnapshot,
    _displaced: &Path,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic compare-and-swap restore is unsupported on this platform",
    ))
}

fn prepare_existing_destination(
    database_path: &Path,
    backup_path: &Path,
) -> Result<(PathBuf, Option<Connection>, DestinationSnapshot, bool), StoreError> {
    restrict_store_permissions(database_path)?;
    let existing = lock_existing_destination(database_path)?;
    let unlocked_corruption = existing.is_none();
    if unlocked_corruption {
        ensure_corrupt_destination_has_no_sidecars(database_path)?;
    }
    let snapshot_before =
        DestinationSnapshot::capture(database_path).map_err(|source| StoreError::Io {
            path: database_path.to_path_buf(),
            source,
        })?;
    let safety_path = next_backup_path(database_path, "pre-restore", Some(backup_path))?;
    let safety_digest = copy_database_file_snapshot(database_path, &safety_path)?;
    let snapshot_after =
        DestinationSnapshot::capture(database_path).map_err(|source| StoreError::Io {
            path: database_path.to_path_buf(),
            source,
        })?;
    if !snapshot_before.same_as(&snapshot_after) || safety_digest != snapshot_before.digest {
        return Err(StoreError::Io {
            path: database_path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "database destination changed while creating its safety snapshot",
            ),
        });
    }
    if unlocked_corruption {
        ensure_corrupt_destination_has_no_sidecars(database_path)?;
    }
    Ok((safety_path, existing, snapshot_after, unlocked_corruption))
}

pub(super) fn restore_database(
    database_path: &Path,
    backup_path: &Path,
    source_ready: impl FnOnce(),
    safety_ready: impl FnOnce(Option<&Path>),
    after_copy: impl FnOnce(Option<&Path>) -> Result<(), StoreError>,
    before_publish: impl FnOnce() -> Result<(), StoreError>,
) -> Result<RestoreReport, StoreError> {
    ensure_distinct_paths(database_path, backup_path)?;
    let database_path = canonical_destination_path(database_path)?;
    let _lock = StoreLock::acquire(&database_path, Duration::from_mins(5))?;
    let source = ValidatedBackupSource::open(backup_path)?;
    source_ready();

    let database_existed = database_path.exists();
    let (safety_backup_path, mut existing_guard, original_snapshot, unlocked_corruption) =
        if database_existed {
            let (safety_path, existing, snapshot, unlocked_corruption) =
                prepare_existing_destination(&database_path, backup_path)?;
            safety_ready(Some(&safety_path));
            (
                Some(safety_path),
                existing,
                Some(snapshot),
                unlocked_corruption,
            )
        } else {
            safety_ready(None);
            (None, None, None, false)
        };

    let (staging, mut destination) = StagedDatabase::create(&database_path)?;
    let restore_result = (|| {
        source.copy_to(&mut destination)?;
        after_copy(safety_backup_path.as_deref())?;
        configure_staged_restore(&destination)?;
        validate_backup(&destination, staging.path())?;
        restrict_store_permissions(staging.path())?;
        schema_version(&destination)
    })();
    drop(destination);
    let restore_result = match restore_result {
        Ok(schema_version) => source.finish().map(|()| schema_version),
        Err(error) => Err(error),
    };
    drop(source);

    let schema_version = match restore_result {
        Ok(schema_version) => schema_version,
        Err(operation) => return Err(staging.cleanup_error(operation)),
    };

    release_destination_lock_for_publish(&mut existing_guard);
    if unlocked_corruption
        && let Err(operation) = ensure_corrupt_destination_has_no_sidecars(&database_path)
    {
        return Err(staging.cleanup_error(operation));
    }
    if let Err(operation) = before_publish() {
        return Err(staging.cleanup_error(operation));
    }
    if unlocked_corruption
        && let Err(operation) = ensure_corrupt_destination_has_no_sidecars(&database_path)
    {
        return Err(staging.cleanup_error(operation));
    }
    if let Some(snapshot) = original_snapshot.as_ref() {
        let unchanged = snapshot
            .matches(&database_path)
            .map_err(|source| StoreError::Io {
                path: database_path.clone(),
                source,
            })?;
        if !unchanged {
            return Err(staging.cleanup_error(StoreError::Io {
                path: database_path,
                source: std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "database destination changed before atomic replacement",
                ),
            }));
        }
    }

    if let Some(original_snapshot) = original_snapshot {
        staging.publish(
            &database_path,
            PublishMode::Replace,
            Some(&original_snapshot),
        )?;
    } else {
        staging.publish(&database_path, PublishMode::CreateNew, None)?;
    }

    Ok(RestoreReport {
        source_path: backup_path.to_path_buf(),
        safety_backup_path,
        schema_version,
    })
}

fn ensure_corrupt_destination_has_no_sidecars(path: &Path) -> Result<(), StoreError> {
    for suffix in &SQLITE_ARTIFACT_SUFFIXES[1..] {
        let sidecar = artifact_path(path, suffix);
        match fs::symlink_metadata(&sidecar) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(StoreError::Io {
                    path: sidecar,
                    source,
                });
            }
            Ok(_) => {
                return Err(StoreError::Io {
                    path: sidecar,
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "cannot snapshot a corrupt database while SQLite sidecars exist",
                    ),
                });
            }
        }
    }
    Ok(())
}

fn lock_existing_destination(path: &Path) -> Result<Option<Connection>, StoreError> {
    let existing = match open_writable_database(path) {
        Ok(existing) => existing,
        Err(StoreError::Sqlite(error)) if is_database_corruption(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    existing.busy_timeout(Duration::from_secs(5))?;
    match existing.execute_batch(
        "PRAGMA wal_checkpoint(TRUNCATE);
         PRAGMA journal_mode=DELETE;
         BEGIN IMMEDIATE;",
    ) {
        Ok(()) => Ok(Some(existing)),
        Err(error) if is_database_corruption(&error) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn is_database_corruption(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(details, _)
            if matches!(
                details.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            )
    )
}

#[cfg(windows)]
fn release_destination_lock_for_publish(guard: &mut Option<Connection>) {
    drop(guard.take());
}

#[cfg(not(windows))]
fn release_destination_lock_for_publish(_guard: &mut Option<Connection>) {}

fn configure_staged_restore(connection: &Connection) -> Result<(), StoreError> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_mode", "DELETE")?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

struct ValidatedBackupSource {
    connection: Connection,
}

impl ValidatedBackupSource {
    fn open(path: &Path) -> Result<Self, StoreError> {
        let connection = open_read_only_connection(path, |connection| {
            configure_backup_source(connection)?;
            connection.execute_batch("BEGIN DEFERRED")?;
            validate_backup(connection, path)
        })?;
        Ok(Self { connection })
    }

    fn copy_to(&self, destination: &mut Connection) -> Result<(), StoreError> {
        backup_connection_to(&self.connection, destination)
    }

    fn finish(&self) -> Result<(), StoreError> {
        self.connection.execute_batch("COMMIT")?;
        Ok(())
    }
}

fn backup_connection_to(
    source: &Connection,
    destination: &mut Connection,
) -> Result<(), StoreError> {
    let backup = Backup::new(source, destination)?;
    backup.run_to_completion(128, Duration::from_millis(5), None)?;
    Ok(())
}

fn next_backup_path(
    database: &Path,
    label: &str,
    exclude: Option<&Path>,
) -> Result<PathBuf, StoreError> {
    let original_extension = database
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned());
    for sequence in 0..10_000_u32 {
        let suffix = if sequence == 0 {
            format!("{label}.backup")
        } else {
            format!("{label}.{sequence}.backup")
        };
        let extension = original_extension
            .as_ref()
            .map_or_else(|| suffix.clone(), |original| format!("{original}.{suffix}"));
        let mut candidate = database.to_path_buf();
        candidate.set_extension(extension);
        if candidate.exists() {
            continue;
        }
        if exclude.is_some_and(|excluded| paths_refer_to_same_file(&candidate, excluded)) {
            continue;
        }
        return Ok(candidate);
    }
    Err(StoreError::Io {
        path: database.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no available backup filename",
        ),
    })
}

fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (
        canonical_path_for_comparison(left),
        canonical_path_for_comparison(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn canonical_path_for_comparison(path: &Path) -> std::io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            let file_name = path.file_name().ok_or(source)?;
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            Ok(fs::canonicalize(parent)?.join(file_name))
        }
        Err(source) => Err(source),
    }
}

pub(super) fn ensure_distinct_paths(database: &Path, backup: &Path) -> Result<(), StoreError> {
    if paths_refer_to_same_file(database, backup) {
        return Err(StoreError::InvalidBackup {
            path: backup.to_path_buf(),
            reason: "backup and destination resolve to the same path".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn open_backup_source(path: &Path) -> Result<Connection, StoreError> {
    open_read_only_connection(path, configure_backup_source)
}

fn configure_backup_source(connection: &Connection) -> Result<(), StoreError> {
    connection.pragma_update(None, "query_only", true)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn open_writable_database(path: &Path) -> Result<Connection, StoreError> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?)
}

fn validate_backup(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    // `quick_check` verifies page and index consistency without the full-table cost of
    // `integrity_check`; foreign-key and exact-schema validation run separately below.
    let integrity =
        connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))?;
    if integrity != "ok" {
        return Err(StoreError::InvalidBackup {
            path: path.to_path_buf(),
            reason: format!("integrity check returned `{integrity}`"),
        });
    }
    let foreign_key_violation = connection
        .query_row(
            "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if foreign_key_violation.is_some() {
        return Err(StoreError::InvalidBackup {
            path: path.to_path_buf(),
            reason: "foreign key check reported violations".to_owned(),
        });
    }
    validate_exact_schema(connection).map_err(|error| match error {
        StoreError::InvalidSchema => StoreError::InvalidBackup {
            path: path.to_path_buf(),
            reason: format!(
                "schema objects do not match the exact supported contract version \
                 {LATEST_SCHEMA_VERSION}"
            ),
        },
        other => other,
    })
}
