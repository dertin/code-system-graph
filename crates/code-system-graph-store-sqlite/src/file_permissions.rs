use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use super::StoreError;

#[cfg(test)]
mod tests;

pub(crate) const SQLITE_ARTIFACT_SUFFIXES: [&str; 4] = ["", "-wal", "-shm", "-journal"];

pub(crate) fn artifact_path(database_path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return database_path.to_path_buf();
    }
    let mut value = database_path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

pub(crate) fn prepare_database_file(path: &Path) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "database path must be a regular file",
                ),
            });
        }
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    options.open(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

pub(crate) fn restrict_store_permissions(database_path: &Path) -> Result<(), StoreError> {
    for suffix in SQLITE_ARTIFACT_SUFFIXES {
        let sidecar_path = artifact_path(database_path, suffix);
        let metadata = match fs::symlink_metadata(&sidecar_path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(StoreError::Io {
                    path: sidecar_path,
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::Io {
                path: sidecar_path,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "database artifact path must be a regular file",
                ),
            });
        }
        set_owner_only_file(&sidecar_path).map_err(|source| StoreError::Io {
            path: sidecar_path,
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn remove_database_artifacts(database_path: &Path) -> Result<(), StoreError> {
    let mut first_error = None;
    for suffix in SQLITE_ARTIFACT_SUFFIXES {
        let artifact = artifact_path(database_path, suffix);
        match fs::remove_file(&artifact) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                first_error.get_or_insert_with(|| StoreError::Io {
                    path: artifact,
                    source,
                });
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Restricts an existing regular file to the current operating-system owner.
///
/// On Unix this sets mode `0600` through a descriptor opened without following
/// symlinks. On Windows this installs a protected DACL containing one
/// full-control owner-rights ACE; inherited access is intentionally removed.
///
/// # Errors
///
/// Returns an I/O error when the file cannot be opened or its permissions
/// cannot be restricted. Platforms other than Unix and Windows are unsupported.
#[cfg(unix)]
pub fn set_owner_only_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

/// Restricts an existing regular file to the current operating-system owner.
///
/// On Unix this sets mode `0600` through a descriptor opened without following
/// symlinks. On Windows this installs a protected DACL containing one
/// full-control owner-rights ACE; inherited access is intentionally removed.
///
/// # Errors
///
/// Returns an I/O error when the file cannot be opened or its permissions
/// cannot be restricted. Platforms other than Unix and Windows are unsupported.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn set_owner_only_file(path: &Path) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW
    };

    let mut path_wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if path_wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file path contains a NUL code unit",
        ));
    }
    path_wide.push(0);

    let descriptor_sddl = "D:P(A;;FA;;;OW)\0".encode_utf16().collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the SDDL input is NUL-terminated, `descriptor` is a valid out-pointer, and the
    // returned LocalAlloc allocation is released exactly once below.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `path_wide` is NUL-terminated and `descriptor` was initialized successfully above.
    let applied = unsafe {
        SetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    // SAFETY: the descriptor was allocated by LocalAlloc inside the conversion API.
    let _released = unsafe { LocalFree(descriptor.cast::<c_void>()) };
    if applied == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Restricts an existing regular file to the current operating-system owner.
///
/// # Errors
///
/// Always returns [`io::ErrorKind::Unsupported`] outside Unix and Windows.
#[cfg(not(any(unix, windows)))]
pub fn set_owner_only_file(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "owner-only file permissions are unsupported on this platform",
    ))
}
