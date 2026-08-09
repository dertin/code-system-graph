use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use code_system_graph_model::{
    CheckoutId, NativePath, NativePathEncoding, RepoId, RepositoryRecord, WorkspaceId, WorkspaceRecord, stable_id, stable_id_bytes
};
use thiserror::Error;
use url::Url;

use crate::WorkspaceManifest;

/// Validated workspace record plus native paths used by scanners.
#[derive(Debug, Clone)]
pub struct RegisteredWorkspace {
    /// Serializable registry record suitable for persistence.
    pub record: WorkspaceRecord,
    checkout_paths: BTreeMap<String, PathBuf>,
}

impl RegisteredWorkspace {
    /// Returns the canonical checkout path for a registered alias.
    #[must_use]
    pub fn checkout_path(&self, alias: &str) -> Option<&Path> {
        self.checkout_paths.get(alias).map(PathBuf::as_path)
    }
}

/// Error returned while resolving a workspace registry.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// The manifest path has no parent directory.
    #[error("workspace manifest path `{0}` has no parent directory")]
    ManifestParentMissing(PathBuf),
    /// A configured path could not be canonicalized.
    #[error("failed to canonicalize `{path}`: {source}")]
    Canonicalize {
        /// Path supplied or derived from the manifest.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// A repository resolves outside every configured root.
    #[error(
        "repository `{alias}` resolves outside allowed roots: `{path}`; \
         add an explicit `allowedRoots` entry"
    )]
    OutsideAllowedRoots {
        /// Repository alias.
        alias: String,
        /// Canonical path rejected by policy.
        path: PathBuf,
    },
}

/// Resolves canonical repository and linked-worktree identities without modifying Git state.
///
/// The manifest directory is always an allowed root. Additional roots must be declared through
/// `allowedRoots`. Repository IDs prefer a credential-free normalized remote and otherwise use
/// the Git common directory or canonical checkout path. Checkout IDs always include the concrete
/// worktree path.
///
/// # Errors
///
/// Returns [`RegistryError`] when configured paths do not exist, cannot be canonicalized, or
/// escape the root allowlist.
pub fn register_workspace(
    config_path: &Path,
    manifest_source: &str,
    manifest: &WorkspaceManifest,
) -> Result<RegisteredWorkspace, RegistryError> {
    let canonical_config = canonicalize(config_path)?;
    let config_directory = canonical_config
        .parent()
        .ok_or_else(|| RegistryError::ManifestParentMissing(config_path.to_path_buf()))?;
    let config_directory = config_directory.to_path_buf();
    let mut allowed_roots = vec![config_directory.clone()];
    for configured_root in &manifest.allowed_roots {
        let root = resolve_path(&config_directory, Path::new(configured_root));
        allowed_roots.push(canonicalize(&root)?);
    }
    allowed_roots.sort();
    allowed_roots.dedup();

    let mut records = Vec::with_capacity(manifest.repos.len());
    let mut checkout_paths = BTreeMap::new();
    for (alias, repository) in &manifest.repos {
        let configured_path = resolve_path(&config_directory, Path::new(&repository.path));
        let configured_path = canonicalize(&configured_path)?;
        let git_root = configured_path
            .join(".git")
            .exists()
            .then(|| git_path(&configured_path, &["rev-parse", "--show-toplevel"]))
            .flatten()
            .and_then(|path| canonicalize(&path).ok())
            .filter(|path| path == &configured_path);
        let is_git_repository = git_root.is_some();
        let checkout_path = git_root.unwrap_or(configured_path);
        ensure_allowed(alias, &checkout_path, &allowed_roots)?;

        let git_common_dir = is_git_repository
            .then(|| git_common_directory(&checkout_path))
            .flatten();
        if let Some(common_dir) = &git_common_dir {
            ensure_allowed(alias, common_dir, &allowed_roots)?;
        }
        let normalized_remote = is_git_repository
            .then(|| git_text(&checkout_path, &["remote", "get-url", "origin"]))
            .flatten()
            .map(|remote| normalize_remote(&remote));
        let head_commit = is_git_repository
            .then(|| git_text(&checkout_path, &["rev-parse", "HEAD"]))
            .flatten();
        let working_tree_dirty = is_git_repository
            && git_output(
                &checkout_path,
                &["status", "--porcelain", "--untracked-files=normal"],
            )
            .is_some_and(|output| !output.is_empty());
        let native_checkout = encode_native_path(&checkout_path);
        let native_common = git_common_dir.as_deref().map(encode_native_path);
        let repository_key = normalized_remote.as_ref().map_or_else(
            || {
                native_common.as_ref().map_or_else(
                    || format!("path:{}", native_path_fingerprint(&native_checkout)),
                    |common| format!("git:{}", native_path_fingerprint(common)),
                )
            },
            |remote| format!("remote:{remote}"),
        );
        let repo_id = RepoId::new(stable_id("repo", &repository_key));
        let checkout_id = CheckoutId::new(stable_id(
            "checkout",
            &format!(
                "{}:{}",
                repo_id.as_str(),
                native_path_fingerprint(&native_checkout)
            ),
        ));
        let is_linked_worktree = checkout_path.join(".git").is_file();

        records.push(RepositoryRecord {
            id: repo_id,
            checkout_id,
            alias: alias.clone(),
            canonical_path: native_checkout,
            git_common_dir: native_common,
            normalized_remote,
            head_commit,
            is_linked_worktree,
            working_tree_dirty,
        });
        checkout_paths.insert(alias.clone(), checkout_path);
    }
    records.sort_by(|left, right| left.alias.cmp(&right.alias));

    let config_native = encode_native_path(&config_directory);
    let workspace_id = WorkspaceId::new(stable_id(
        "workspace",
        &format!(
            "{}:{}",
            manifest.name,
            native_path_fingerprint(&config_native)
        ),
    ));
    Ok(RegisteredWorkspace {
        record: WorkspaceRecord {
            id: workspace_id,
            name: manifest.name.clone(),
            manifest_hash: stable_id("manifest", manifest_source),
            config_path: Some(encode_native_path(&canonical_config)),
            repositories: records,
        },
        checkout_paths,
    })
}

/// Encodes a native path without requiring UTF-8.
#[must_use]
pub fn encode_native_path(path: &Path) -> NativePath {
    NativePath {
        encoding: native_path_encoding(),
        bytes: native_path_bytes(path),
        display: path.to_string_lossy().into_owned(),
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf, RegistryError> {
    std::fs::canonicalize(path).map_err(|source| RegistryError::Canonicalize {
        path: path.to_path_buf(),
        source,
    })
}

fn ensure_allowed(
    alias: &str,
    path: &Path,
    allowed_roots: &[PathBuf],
) -> Result<(), RegistryError> {
    if allowed_roots.iter().any(|root| path.starts_with(root)) {
        return Ok(());
    }
    Err(RegistryError::OutsideAllowedRoots {
        alias: alias.to_owned(),
        path: path.to_path_buf(),
    })
}

fn resolve_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn git_common_directory(checkout_path: &Path) -> Option<PathBuf> {
    git_path(
        checkout_path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .or_else(|| {
        git_path(checkout_path, &["rev-parse", "--git-common-dir"])
            .map(|path| resolve_path(checkout_path, &path))
    })
    .and_then(|path| std::fs::canonicalize(path).ok())
}

fn git_path(checkout_path: &Path, arguments: &[&str]) -> Option<PathBuf> {
    git_output(checkout_path, arguments).map(path_from_git_output)
}

fn git_text(checkout_path: &Path, arguments: &[&str]) -> Option<String> {
    git_output(checkout_path, arguments)
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn git_output(checkout_path: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout_path)
        .args(arguments)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    output.status.success().then_some(trim_ascii(output.stdout))
}

fn trim_ascii(mut bytes: Vec<u8>) -> Vec<u8> {
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes.pop();
    }
    bytes
}

fn normalize_remote(remote: &str) -> String {
    let trimmed = remote.trim();
    if let Some((authority, path)) = scp_remote_parts(trimmed) {
        return format!(
            "{}/{}",
            authority.to_ascii_lowercase(),
            clean_remote_path(path)
        );
    }
    if let Ok(url) = Url::parse(trimmed) {
        if let Some(host) = url.host_str() {
            return format!(
                "{}/{}",
                host.to_ascii_lowercase(),
                clean_remote_path(url.path())
            );
        }
        return format!("{}:{}", url.scheme(), clean_remote_path(url.path()));
    }
    clean_remote_path(trimmed)
}

fn scp_remote_parts(remote: &str) -> Option<(&str, &str)> {
    if remote.contains("://") {
        return None;
    }
    let (authority, path) = remote.split_once(':')?;
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    (!host.is_empty() && !path.is_empty()).then_some((host, path))
}

fn clean_remote_path(path: &str) -> String {
    path.trim()
        .trim_matches('/')
        .trim_end_matches(".git")
        .replace('\\', "/")
}

fn native_path_fingerprint(path: &NativePath) -> String {
    let namespace = match path.encoding {
        NativePathEncoding::UnixBytes => "path-unix",
        NativePathEncoding::WindowsWide => "path-windows",
        NativePathEncoding::Utf8 => "path-utf8",
    };
    stable_id_bytes(namespace, &path.bytes)
}

#[cfg(unix)]
fn native_path_encoding() -> NativePathEncoding {
    NativePathEncoding::UnixBytes
}

#[cfg(windows)]
fn native_path_encoding() -> NativePathEncoding {
    NativePathEncoding::WindowsWide
}

#[cfg(not(any(unix, windows)))]
fn native_path_encoding() -> NativePathEncoding {
    NativePathEncoding::Utf8
}

#[cfg(unix)]
fn native_path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn native_path_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn native_path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

#[cfg(unix)]
fn path_from_git_output(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;

    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_from_git_output(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::{normalize_remote, register_workspace};
    use crate::parse_manifest;

    #[test]
    fn normalize_remote_should_remove_credentials_protocol_and_git_suffix() {
        let https = normalize_remote("https://token@example.com/team/api.git");
        let ssh = normalize_remote("git@example.com:team/api.git");

        assert_eq!(https, ssh);
    }

    #[cfg(unix)]
    #[test]
    fn encode_native_path_should_preserve_non_utf8_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path =
            std::path::PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]));
        let encoded = super::encode_native_path(&path);

        assert_eq!(encoded.bytes, vec![b'/', b't', b'm', b'p', b'/', 0xff]);
    }

    #[test]
    fn register_workspace_should_reject_symlink_escape() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let workspace = temporary.path().join("workspace");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(&outside)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, workspace.join("escaped"))?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, workspace.join("escaped"))?;
        let manifest_path = workspace.join("code-system-graph.yaml");
        fs::write(&manifest_path, "")?;
        let source = "version: 1\nname: test\nrepos:\n  escaped:\n    path: escaped\n";
        let manifest = parse_manifest(source)?;
        let result = register_workspace(&manifest_path, source, &manifest);

        assert!(matches!(
            result,
            Err(super::RegistryError::OutsideAllowedRoots { .. })
        ));
        Ok(())
    }

    #[test]
    fn register_workspace_should_share_repo_id_across_linked_worktrees()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let repository = temporary.path().join("repository");
        let linked = temporary.path().join("linked");
        fs::create_dir_all(&repository)?;
        git(&repository, &["init"])?;
        git(&repository, &["config", "core.autocrlf", "false"])?;
        fs::write(repository.join("README.md"), "fixture")?;
        git(&repository, &["add", "README.md"])?;
        git(
            &repository,
            &[
                "-c",
                "user.name=Code System Graph Test",
                "-c",
                "user.email=code-system-graph@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        )?;
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "linked-fixture",
                linked.to_string_lossy().as_ref(),
            ],
        )?;
        let manifest_path = temporary.path().join("code-system-graph.yaml");
        let source = "version: 1\nname: test\nrepos:\n  main:\n    path: repository\n  linked:\n    path: linked\n";
        fs::write(&manifest_path, source)?;
        let manifest = parse_manifest(source)?;
        let registry = register_workspace(&manifest_path, source, &manifest)?;
        let main = registry
            .record
            .repositories
            .iter()
            .find(|record| record.alias == "main")
            .ok_or_else(|| std::io::Error::other("main record missing"))?;
        let worktree = registry
            .record
            .repositories
            .iter()
            .find(|record| record.alias == "linked")
            .ok_or_else(|| std::io::Error::other("linked record missing"))?;

        assert_eq!(
            (
                main.id.clone(),
                main.checkout_id == worktree.checkout_id,
                worktree.id.clone(),
                worktree.is_linked_worktree,
            ),
            (worktree.id.clone(), false, main.id.clone(), true)
        );
        Ok(())
    }

    fn git(repository: &Path, arguments: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        Err(std::io::Error::other(String::from_utf8_lossy(&output.stderr)).into())
    }
}
