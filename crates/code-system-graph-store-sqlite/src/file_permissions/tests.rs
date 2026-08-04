use std::fs;
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::Duration;

use super::remove_database_artifacts;
#[cfg(unix)]
use super::{SQLITE_ARTIFACT_SUFFIXES, artifact_path};
#[cfg(unix)]
use crate::SqliteStore;
use crate::StoreError;

#[cfg(unix)]
const UMASK_HELPER_ENV: &str = "CODE_SYSTEM_GRAPH_UMASK_PERMISSION_HELPER";
#[cfg(unix)]
const UMASK_READY_ENV: &str = "CODE_SYSTEM_GRAPH_UMASK_PERMISSION_READY";
#[cfg(unix)]
const UMASK_DATABASE_ENV: &str = "CODE_SYSTEM_GRAPH_UMASK_PERMISSION_DATABASE";

#[cfg(unix)]
#[test]
fn open_should_restrict_sidecars_created_during_schema_initialization()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("fresh.db");
    SqliteStore::open(&database)?;

    for suffix in &SQLITE_ARTIFACT_SUFFIXES[1..] {
        let sidecar_path = artifact_path(&database, suffix);
        if sidecar_path.exists() {
            assert_eq!(
                fs::metadata(&sidecar_path)?.permissions().mode() & 0o777,
                0o600,
                "sidecar `{}` must be owner-only after initialization",
                sidecar_path.display()
            );
        }
    }
    Ok(())
}

#[test]
fn artifact_cleanup_should_report_unremovable_sidecar() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let sidecar_directory = temporary.path().join("store.db-wal");
    let shm = temporary.path().join("store.db-shm");
    let journal = temporary.path().join("store.db-journal");
    fs::write(&database, b"partial")?;
    fs::create_dir(&sidecar_directory)?;
    fs::write(&shm, b"sensitive")?;
    fs::write(&journal, b"sensitive")?;

    let result = remove_database_artifacts(&database);

    assert!(matches!(
        result,
        Err(StoreError::Io { path, .. }) if path == sidecar_directory
    ));
    assert!(!shm.exists(), "cleanup must continue after the first error");
    assert!(
        !journal.exists(),
        "cleanup must attempt every database artifact"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn umask_permission_helper() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var(UMASK_HELPER_ENV).as_deref() != Ok("1") {
        return Ok(());
    }
    {
        use std::os::unix::fs::PermissionsExt;

        let database = std::env::var_os(UMASK_DATABASE_ENV)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| std::io::Error::other("umask database environment is missing"))?;
        let ready = std::env::var_os(UMASK_READY_ENV)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| std::io::Error::other("umask ready environment is missing"))?;
        let backup = database.with_extension("db.backup");
        {
            let store = SqliteStore::open(&database)?;
            store.backup_to(&backup)?;
        }
        let mut paths = vec![database.clone(), backup];
        paths.extend(
            SQLITE_ARTIFACT_SUFFIXES[1..]
                .iter()
                .map(|suffix| artifact_path(&database, suffix)),
        );
        for path in paths {
            if path.exists() {
                assert_eq!(
                    fs::metadata(&path)?.permissions().mode() & 0o777,
                    0o600,
                    "permissions differ for `{}`",
                    path.display()
                );
            }
        }
        fs::write(ready, "ready")?;
        std::thread::sleep(Duration::from_secs(30));
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn database_files_should_use_owner_only_permissions_under_permissive_umask()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("umask.db");
    let ready = temporary.path().join("ready");
    let executable = std::env::current_exe()?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "umask 000; exec {} --exact file_permissions::tests::umask_permission_helper --nocapture",
            executable.display()
        ))
        .env(UMASK_HELPER_ENV, "1")
        .env(UMASK_DATABASE_ENV, &database)
        .env(UMASK_READY_ENV, &ready)
        .spawn()?;
    let mut helper_ready = false;
    for _attempt in 0..200 {
        if ready.exists() {
            helper_ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if !helper_ready {
        child.kill()?;
        let _status = child.wait()?;
        return Err(std::io::Error::other("umask helper did not become ready").into());
    }
    child.kill()?;
    let _status = child.wait()?;
    Ok(())
}
