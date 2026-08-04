use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use super::StoreError;
use crate::file_permissions::set_owner_only_file;

const ACCESS_LOCK_DIRECTORY: &str = ".code-system-graph-access-locks";

pub(super) struct StoreAccessLock {
    _file: File,
    database_path: PathBuf,
}

impl StoreAccessLock {
    pub(super) fn shared(database_path: &Path) -> Result<Self, StoreError> {
        Self::acquire(database_path, false)
    }

    pub(super) fn exclusive(database_path: &Path) -> Result<Self, StoreError> {
        Self::acquire(database_path, true)
    }

    pub(super) fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn acquire(database_path: &Path, exclusive: bool) -> Result<Self, StoreError> {
        let canonical_path = canonical_access_path(database_path)?;
        ensure_single_link(&canonical_path).map_err(|source| StoreError::Io {
            path: canonical_path.clone(),
            source,
        })?;
        let path = access_lock_path(&canonical_path)?;
        let file = open_access_lock(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })?;
        set_owner_only_file(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })?;
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        match result {
            Ok(()) => Ok(Self {
                _file: file,
                database_path: canonical_path,
            }),
            Err(std::fs::TryLockError::WouldBlock) => Err(StoreError::LockHeld(path)),
            Err(std::fs::TryLockError::Error(source)) => Err(StoreError::Io { path, source }),
        }
    }
}

fn canonical_access_path(database_path: &Path) -> Result<PathBuf, StoreError> {
    let file_name = database_path.file_name().ok_or_else(|| StoreError::Io {
        path: database_path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "database path must include a file name",
        ),
    })?;
    let parent = database_path.parent().unwrap_or_else(|| Path::new("."));
    if fs::symlink_metadata(database_path).is_err() {
        fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let parent = fs::canonicalize(parent).map_err(|source| StoreError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let path = parent.join(file_name);
    match fs::canonicalize(&path) {
        Ok(path) => Ok(path),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(source) => Err(StoreError::Io { path, source }),
    }
}

fn access_lock_root() -> Result<PathBuf, StoreError> {
    let directory = std::env::temp_dir().join(access_lock_directory_name().map_err(|source| {
        StoreError::Io {
            path: std::env::temp_dir(),
            source,
        }
    })?);
    create_access_lock_directory(&directory).map_err(|source| StoreError::Io {
        path: directory.clone(),
        source,
    })?;
    Ok(directory)
}

fn access_lock_path(database_path: &Path) -> Result<PathBuf, StoreError> {
    let directory = access_lock_root()?;
    let mut hasher = blake3::Hasher::new();
    hash_path_bytes(&mut hasher, database_path);
    Ok(directory.join(format!("{}.lock", hasher.finalize().to_hex())))
}

#[cfg(unix)]
fn hash_path_bytes(hasher: &mut blake3::Hasher, path: &Path) {
    use std::os::unix::ffi::OsStrExt;

    hasher.update(path.as_os_str().as_bytes());
}

#[cfg(windows)]
fn hash_path_bytes(hasher: &mut blake3::Hasher, path: &Path) {
    for code_unit in path.to_string_lossy().to_lowercase().encode_utf16() {
        hasher.update(&code_unit.to_le_bytes());
    }
}

#[cfg(not(any(unix, windows)))]
fn hash_path_bytes(hasher: &mut blake3::Hasher, path: &Path) {
    hasher.update(path.to_string_lossy().as_bytes());
}

#[cfg(unix)]
#[allow(unsafe_code)]
#[allow(clippy::unnecessary_wraps)]
fn access_lock_directory_name() -> std::io::Result<String> {
    // SAFETY: querying the effective user ID has no preconditions.
    let user_id = unsafe { libc::geteuid() };
    Ok(format!("{ACCESS_LOCK_DIRECTORY}-{user_id}"))
}

#[cfg(windows)]
fn access_lock_directory_name() -> std::io::Result<String> {
    use crate::file_permissions::current_user_sid_string;

    let sid = current_user_sid_string()?;
    Ok(format!(
        "{ACCESS_LOCK_DIRECTORY}-{}",
        blake3::hash(sid.as_bytes()).to_hex()
    ))
}

#[cfg(not(any(unix, windows)))]
#[allow(clippy::unnecessary_wraps)]
fn access_lock_directory_name() -> std::io::Result<String> {
    Ok(ACCESS_LOCK_DIRECTORY.to_owned())
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn create_access_lock_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let mut builder = fs::DirBuilder::new();
    match builder.mode(0o700).create(path) {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => return Err(source),
    }
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: querying the effective user ID has no preconditions.
    let user_id = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink() || !metadata.is_dir() || metadata.uid() != user_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "access-lock directory is not owned by the effective user",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
fn create_access_lock_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "access-lock path must be a directory",
        ));
    }
    set_owner_only_file(path)
}

#[cfg(not(any(unix, windows)))]
fn create_access_lock_directory(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "cross-process store access locks are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn ensure_single_link(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    match fs::metadata(path) {
        Ok(metadata) if metadata.nlink() > 1 => Err(multiple_links_error()),
        Ok(_) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source),
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn ensure_single_link(path: &Path) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle
    };

    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(source),
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the file handle is live and the output pointer is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut information) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if information.nNumberOfLinks > 1 {
        return Err(multiple_links_error());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn ensure_single_link(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn multiple_links_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "database files with multiple hard links are unsupported",
    )
}

#[cfg(unix)]
fn open_access_lock(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_access_lock(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;

    const ACCESS_READER_HELPER_ENV: &str = "CSG_ACCESS_READER_HELPER";
    const ACCESS_READER_DATABASE_ENV: &str = "CSG_ACCESS_READER_DATABASE";
    const ACCESS_READER_READY_ENV: &str = "CSG_ACCESS_READER_READY";

    #[test]
    fn exclusive_lock_should_prevent_creating_missing_database()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let _exclusive = StoreAccessLock::exclusive(&database)?;

        let blocked = SqliteStore::open(&database);

        assert!(matches!(blocked, Err(StoreError::LockHeld(_))));
        assert!(!database.exists());
        Ok(())
    }

    #[test]
    fn read_only_access_helper() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var(ACCESS_READER_HELPER_ENV).as_deref() != Ok("1") {
            return Ok(());
        }
        let database = std::env::var_os(ACCESS_READER_DATABASE_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| std::io::Error::other("reader database path is missing"))?;
        let ready = std::env::var_os(ACCESS_READER_READY_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| std::io::Error::other("reader ready path is missing"))?;
        let _store = SqliteStore::open_read_only(database)?;
        fs::write(ready, "ready")?;
        std::thread::sleep(std::time::Duration::from_secs(30));
        Ok(())
    }

    #[test]
    fn exclusive_lock_should_reject_reader_in_another_process()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let ready = temporary.path().join("reader-ready");
        drop(SqliteStore::open(&database)?);
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("access_lock::tests::read_only_access_helper")
            .arg("--nocapture")
            .env(ACCESS_READER_HELPER_ENV, "1")
            .env(ACCESS_READER_DATABASE_ENV, &database)
            .env(ACCESS_READER_READY_ENV, &ready)
            .spawn()?;
        let mut started = false;
        for _ in 0..1_000 {
            if ready.exists() {
                started = true;
                break;
            }
            if child.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let blocked = started.then(|| StoreAccessLock::exclusive(&database));
        let _ = child.kill();
        let status = child.wait()?;

        assert!(started, "reader helper did not acquire its shared lock");
        assert!(matches!(blocked, Some(Err(StoreError::LockHeld(_)))));
        assert!(
            !status.success(),
            "reader helper should be terminated by the test"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_alias_should_share_the_canonical_path_lock() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let alias = temporary.path().join("store-alias.db");
        let store = SqliteStore::open(&database)?;
        symlink(&database, &alias)?;

        let blocked = StoreAccessLock::exclusive(&alias);
        drop(store);

        assert!(matches!(blocked, Err(StoreError::LockHeld(_))));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn access_lock_should_pin_target_across_symlink_retargeting()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir()?;
        let first = temporary.path().join("first.db");
        let second = temporary.path().join("second.db");
        let alias = temporary.path().join("store-alias.db");
        drop(SqliteStore::open(&first)?);
        drop(SqliteStore::open(&second)?);
        symlink(&first, &alias)?;
        let access_lock = StoreAccessLock::shared(&alias)?;
        fs::remove_file(&alias)?;
        symlink(&second, &alias)?;

        assert_eq!(
            access_lock.database_path(),
            fs::canonicalize(&first)?.as_path()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn access_lock_path_should_live_under_runtime_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let lock_path = access_lock_path(&database)?;
        let expected_root = access_lock_root()?;

        assert!(lock_path.starts_with(&expected_root));
        assert_ne!(lock_path.parent(), database.parent());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn shared_access_lock_should_not_require_database_directory_write_access()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir()?;
        let readonly = temporary.path().join("readonly");
        fs::create_dir(&readonly)?;
        let database = readonly.join("store.db");
        drop(SqliteStore::open(&database)?);
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o555))?;

        let lock = StoreAccessLock::shared(&database);
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755))?;

        assert!(
            lock.is_ok(),
            "expected shared access lock to succeed: {:?}",
            lock.err()
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn missing_windows_paths_should_lock_case_insensitively()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let upper = canonical_access_path(&temporary.path().join("Store.db"))?;
        let lower = canonical_access_path(&temporary.path().join("store.db"))?;

        assert_eq!(access_lock_path(&upper)?, access_lock_path(&lower)?);
        Ok(())
    }
}
