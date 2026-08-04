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

/// Returns the current Windows user's textual security identifier.
///
/// # Errors
///
/// Returns an I/O error when the process token or SID cannot be queried.
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
pub(crate) fn current_user_sid_string() -> io::Result<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_NO_TOKEN, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken
    };

    let mut token = std::ptr::null_mut();
    // SAFETY: the pseudo thread handle is valid and `token` points to writable storage.
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut token) } == 0 {
        let thread_error = io::Error::last_os_error();
        if thread_error.raw_os_error() != Some(ERROR_NO_TOKEN.cast_signed()) {
            return Err(thread_error);
        }
        // SAFETY: the pseudo process handle is valid and `token` points to writable storage.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let result = (|| {
        let mut required = 0_u32;
        // SAFETY: a null buffer with length zero is the documented size-query call.
        unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &raw mut required);
        }
        if required == 0 {
            return Err(io::Error::last_os_error());
        }
        let word_size = std::mem::size_of::<usize>();
        let mut buffer = vec![0_usize; (required as usize).div_ceil(word_size)];
        // SAFETY: the aligned buffer has at least `required` writable bytes.
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                required,
                &raw mut required,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful TokenUser query initialized a TOKEN_USER at the buffer start.
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut sid_text = std::ptr::null_mut();
        // SAFETY: the SID comes from the live token buffer and the output pointer is writable.
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut sid_text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut length = 0;
        // SAFETY: ConvertSidToStringSidW returned a NUL-terminated allocated UTF-16 string.
        unsafe {
            while *sid_text.add(length) != 0 {
                length += 1;
            }
        }
        // SAFETY: the preceding scan established the initialized string length.
        let sid = unsafe { std::slice::from_raw_parts(sid_text, length) };
        let result = String::from_utf16(sid)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()));
        // SAFETY: the SID string was allocated by ConvertSidToStringSidW.
        unsafe {
            LocalFree(sid_text.cast());
        }
        result
    })();
    // SAFETY: OpenProcessToken returned this owned token handle.
    unsafe {
        CloseHandle(token);
    }
    result
}

/// Restricts an existing regular file to the current Windows user.
///
/// Installs a protected DACL containing one full-control ACE for the current
/// process user's concrete SID; inherited access is intentionally removed.
///
/// # Errors
///
/// Returns an I/O error when the SID cannot be queried or the DACL cannot be applied.
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

    let descriptor_sddl = format!("D:P(A;;FA;;;{})\0", current_user_sid_string()?)
        .encode_utf16()
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the SDDL input is NUL-terminated, `descriptor` is a valid out-pointer, and the
    // returned LocalAlloc allocation is released exactly once below.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
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
