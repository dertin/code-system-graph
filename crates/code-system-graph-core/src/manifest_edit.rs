use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use code_system_graph_model::stable_id;
use thiserror::Error;

use crate::{ManifestError, ManualLinkConfig, parse_manifest};

/// Preview of one constrained workspace-manifest mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEdit {
    /// Original content fingerprint used for concurrent-change detection.
    pub original_hash: String,
    /// Complete updated manifest content.
    pub updated_source: String,
    /// Human-readable operation summary.
    pub summary: String,
}

/// Result of atomically committing a manifest edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestWriteReport {
    /// Canonical manifest path.
    pub manifest_path: PathBuf,
    /// Non-overwriting backup of the previous manifest.
    pub backup_path: PathBuf,
}

/// Error returned by constrained manifest editing.
#[derive(Debug, Error)]
pub enum ManifestEditError {
    /// Existing or generated manifest is invalid.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// Alias cannot be represented safely as an unquoted YAML key.
    #[error("repository alias `{0}` must contain only ASCII letters, digits, `_`, or `-`")]
    InvalidAlias(String),
    /// Alias already exists.
    #[error("repository alias `{0}` already exists")]
    DuplicateAlias(String),
    /// Alias does not exist.
    #[error("repository alias `{0}` does not exist")]
    MissingAlias(String),
    /// Manifest layout cannot be edited without rewriting unrelated content.
    #[error("manifest layout is not surgically editable: {0}")]
    UnsupportedLayout(String),
    /// Filesystem operation failed.
    #[error("manifest filesystem operation failed for `{path}`: {source}")]
    Io {
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying operating-system error.
        source: std::io::Error,
    },
    /// Manifest changed after preview generation.
    #[error("manifest `{0}` changed after preview; regenerate the preview")]
    ConcurrentChange(PathBuf),
}

/// Previews adding a minimal repository entry while preserving unrelated bytes.
///
/// # Errors
///
/// Returns [`ManifestEditError`] for invalid aliases, duplicate aliases, unsupported layout, or
/// invalid generated manifests.
pub fn preview_add_repository(
    source: &str,
    alias: &str,
    repository_path: &str,
) -> Result<ManifestEdit, ManifestEditError> {
    validate_alias(alias)?;
    let manifest = parse_manifest(source)?;
    if manifest.repos.contains_key(alias) {
        return Err(ManifestEditError::DuplicateAlias(alias.to_owned()));
    }
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let repos_index = find_repos_line(&lines)?;
    let insertion_index = find_repos_end(&lines, repos_index);
    let insertion_offset = lines[..insertion_index]
        .iter()
        .map(|line| line.len())
        .sum::<usize>();
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let quoted_path =
        serde_json::to_string(repository_path).map_err(|source| ManifestEditError::Io {
            path: PathBuf::from("<repository-path>"),
            source: std::io::Error::other(source),
        })?;
    let mut updated = String::with_capacity(source.len() + alias.len() + quoted_path.len() + 16);
    updated.push_str(&source[..insertion_offset]);
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push_str(newline);
    }
    updated.push_str("  ");
    updated.push_str(alias);
    updated.push(':');
    updated.push_str(newline);
    updated.push_str("    path: ");
    updated.push_str(&quoted_path);
    updated.push_str(newline);
    updated.push_str(&source[insertion_offset..]);
    parse_manifest(&updated)?;
    Ok(ManifestEdit {
        original_hash: stable_id("manifest-source", source),
        updated_source: updated,
        summary: format!("add repository `{alias}` at `{repository_path}`"),
    })
}

/// Previews removing exactly one repository block while preserving unrelated bytes.
///
/// # Errors
///
/// Returns [`ManifestEditError`] when the alias is missing, layout is unsupported, or removal
/// would make the manifest invalid.
pub fn preview_remove_repository(
    source: &str,
    alias: &str,
) -> Result<ManifestEdit, ManifestEditError> {
    validate_alias(alias)?;
    let manifest = parse_manifest(source)?;
    if !manifest.repos.contains_key(alias) {
        return Err(ManifestEditError::MissingAlias(alias.to_owned()));
    }
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let repos_index = find_repos_line(&lines)?;
    let alias_index = find_alias_line(&lines, repos_index, alias)?;
    let end_index = find_repository_end(&lines, alias_index);
    let start_offset = lines[..alias_index]
        .iter()
        .map(|line| line.len())
        .sum::<usize>();
    let end_offset = lines[..end_index]
        .iter()
        .map(|line| line.len())
        .sum::<usize>();
    let mut updated = String::with_capacity(source.len() - (end_offset - start_offset));
    updated.push_str(&source[..start_offset]);
    updated.push_str(&source[end_offset..]);
    parse_manifest(&updated)?;
    Ok(ManifestEdit {
        original_hash: stable_id("manifest-source", source),
        updated_source: updated,
        summary: format!("remove repository `{alias}`"),
    })
}

/// Previews appending one exact manual relationship while preserving unrelated bytes.
///
/// # Errors
///
/// Returns [`ManifestEditError`] when the declaration is invalid, duplicates an existing
/// relationship, or the top-level manifest layout cannot be edited safely.
pub fn preview_add_manual_link(
    source: &str,
    link: &ManualLinkConfig,
) -> Result<ManifestEdit, ManifestEditError> {
    let mut manifest = parse_manifest(source)?;
    manifest.manual_links.push(link.clone());
    crate::validate_manual_links(&manifest.manual_links)?;

    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let manual_links_index = lines
        .iter()
        .position(|line| line_content(line) == "manualLinks:");
    let insertion_index =
        manual_links_index.map_or(lines.len(), |index| find_section_end(&lines, index));
    let insertion_offset = lines[..insertion_index]
        .iter()
        .map(|line| line.len())
        .sum::<usize>();
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let rendered = render_manual_link(link, newline)?;
    let mut updated = String::with_capacity(source.len() + rendered.len() + 24);
    updated.push_str(&source[..insertion_offset]);
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push_str(newline);
    }
    if manual_links_index.is_none() {
        updated.push_str("manualLinks:");
        updated.push_str(newline);
    }
    updated.push_str(&rendered);
    updated.push_str(&source[insertion_offset..]);
    parse_manifest(&updated)?;
    Ok(ManifestEdit {
        original_hash: stable_id("manifest-source", source),
        updated_source: updated,
        summary: format!(
            "add manual relationship `{}` -> `{}` ({:?})",
            link.from, link.to, link.relation
        ),
    })
}

/// Commits a preview with backup, concurrent-change detection, and atomic replacement.
///
/// # Errors
///
/// Returns [`ManifestEditError`] when the source changed, backup creation fails, or the atomic
/// replacement cannot be committed.
pub fn commit_manifest_edit(
    manifest_path: &Path,
    edit: &ManifestEdit,
) -> Result<ManifestWriteReport, ManifestEditError> {
    let canonical = fs::canonicalize(manifest_path).map_err(|source| ManifestEditError::Io {
        path: manifest_path.to_path_buf(),
        source,
    })?;
    let current = fs::read_to_string(&canonical).map_err(|source| ManifestEditError::Io {
        path: canonical.clone(),
        source,
    })?;
    if stable_id("manifest-source", &current) != edit.original_hash {
        return Err(ManifestEditError::ConcurrentChange(canonical));
    }
    parse_manifest(&edit.updated_source)?;
    let backup_path = next_backup_path(&canonical)?;
    copy_new_file(&canonical, &backup_path)?;
    let mut destination =
        AtomicWriteFile::open(&canonical).map_err(|source| ManifestEditError::Io {
            path: canonical.clone(),
            source,
        })?;
    destination
        .write_all(edit.updated_source.as_bytes())
        .and_then(|()| destination.sync_all())
        .map_err(|source| ManifestEditError::Io {
            path: canonical.clone(),
            source,
        })?;
    destination
        .commit()
        .map_err(|source| ManifestEditError::Io {
            path: canonical.clone(),
            source,
        })?;
    Ok(ManifestWriteReport {
        manifest_path: canonical,
        backup_path,
    })
}

fn validate_alias(alias: &str) -> Result<(), ManifestEditError> {
    let valid = !alias.is_empty()
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if !valid {
        return Err(ManifestEditError::InvalidAlias(alias.to_owned()));
    }
    Ok(())
}

fn find_repos_line(lines: &[&str]) -> Result<usize, ManifestEditError> {
    lines
        .iter()
        .position(|line| line_content(line) == "repos:")
        .ok_or_else(|| ManifestEditError::UnsupportedLayout("top-level `repos:` is missing".into()))
}

fn find_repos_end(lines: &[&str], repos_index: usize) -> usize {
    find_section_end(lines, repos_index)
}

fn find_section_end(lines: &[&str], section_index: usize) -> usize {
    lines
        .iter()
        .enumerate()
        .skip(section_index + 1)
        .find(|(_, line)| is_top_level_content(line))
        .map_or(lines.len(), |(index, _)| index)
}

fn find_alias_line(
    lines: &[&str],
    repos_index: usize,
    alias: &str,
) -> Result<usize, ManifestEditError> {
    let unquoted = format!("  {alias}:");
    let quoted_alias = serde_json::to_string(alias).map_err(|source| ManifestEditError::Io {
        path: PathBuf::from("<repository-alias>"),
        source: std::io::Error::other(source),
    })?;
    let quoted = format!("  {quoted_alias}:");
    let repos_end = find_repos_end(lines, repos_index);
    lines[repos_index + 1..repos_end]
        .iter()
        .position(|line| {
            let content = line_content(line);
            content == unquoted || content == quoted
        })
        .map(|relative| repos_index + 1 + relative)
        .ok_or_else(|| {
            ManifestEditError::UnsupportedLayout(format!(
                "repository `{alias}` key could not be located"
            ))
        })
}

fn find_repository_end(lines: &[&str], alias_index: usize) -> usize {
    lines
        .iter()
        .enumerate()
        .skip(alias_index + 1)
        .find(|(_, line)| {
            let content = line_content(line);
            !content.is_empty()
                && !content.trim_start().starts_with('#')
                && leading_spaces(content) <= 2
        })
        .map_or(lines.len(), |(index, _)| index)
}

fn is_top_level_content(line: &str) -> bool {
    let content = line_content(line);
    !content.is_empty() && !content.starts_with(char::is_whitespace) && !content.starts_with('#')
}

fn line_content(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|byte| *byte == b' ').count()
}

fn render_manual_link(link: &ManualLinkConfig, newline: &str) -> Result<String, ManifestEditError> {
    let yaml = serde_saphyr::to_string(link).map_err(|source| ManifestEditError::Io {
        path: PathBuf::from("<manual-link>"),
        source: std::io::Error::other(source),
    })?;
    let mut rendered = String::new();
    for (index, line) in yaml.lines().enumerate() {
        if index == 0 {
            rendered.push_str("  - ");
        } else {
            rendered.push_str("    ");
        }
        rendered.push_str(line);
        rendered.push_str(newline);
    }
    Ok(rendered)
}

fn next_backup_path(manifest_path: &Path) -> Result<PathBuf, ManifestEditError> {
    let extension = manifest_path
        .extension()
        .map(|value| value.to_string_lossy().into_owned());
    for sequence in 0..10_000_u32 {
        let suffix = if sequence == 0 {
            "pre-edit.backup".to_owned()
        } else {
            format!("pre-edit.{sequence}.backup")
        };
        let mut candidate = manifest_path.to_path_buf();
        candidate.set_extension(
            extension
                .as_ref()
                .map_or_else(|| suffix.clone(), |value| format!("{value}.{suffix}")),
        );
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ManifestEditError::Io {
        path: manifest_path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no available manifest backup filename",
        ),
    })
}

fn copy_new_file(source: &Path, destination: &Path) -> Result<(), ManifestEditError> {
    let mut input = fs::File::open(source).map_err(|error| ManifestEditError::Io {
        path: source.to_path_buf(),
        source: error,
    })?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source| ManifestEditError::Io {
            path: destination.to_path_buf(),
            source,
        })?;
    std::io::copy(&mut input, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|source| ManifestEditError::Io {
            path: destination.to_path_buf(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::EdgeKind;

    use super::{
        ManifestEditError, commit_manifest_edit, preview_add_manual_link, preview_add_repository, preview_remove_repository
    };
    use crate::ManualLinkConfig;

    const MANIFEST: &str = r"# leading comment
version: 1
name: demo
repos:
  api:
    path: api
    openapi: openapi.yaml

allowedRoots:
  - .
";

    #[test]
    fn add_should_preserve_unrelated_manifest_bytes() {
        let result = preview_add_repository(MANIFEST, "worker", "../worker");

        assert!(matches!(
            result,
            Ok(edit)
                if edit.updated_source.starts_with("# leading comment\n")
                    && edit.updated_source.contains("  worker:\n    path: \"../worker\"\n")
                    && edit.updated_source.ends_with("allowedRoots:\n  - .\n")
        ));
    }

    #[test]
    fn remove_should_delete_only_selected_repository_block() {
        let source = MANIFEST.replace("  api:", "  web:\n    path: web\n  api:");
        let result = preview_remove_repository(&source, "api");

        assert!(matches!(
            result,
            Ok(edit)
                if edit.updated_source.contains("  web:\n    path: web\n")
                    && !edit.updated_source.contains("  api:")
                    && edit.updated_source.contains("allowedRoots:")
        ));
    }

    #[test]
    fn commit_should_backup_and_detect_concurrent_changes() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let manifest_path = temporary.path().join("code-system-graph.yaml");
        std::fs::write(&manifest_path, MANIFEST)?;
        let edit = preview_add_repository(MANIFEST, "worker", "worker")?;
        let report = commit_manifest_edit(&manifest_path, &edit)?;
        let backup = std::fs::read_to_string(report.backup_path)?;
        let written = std::fs::read_to_string(&manifest_path)?;
        std::fs::write(&manifest_path, format!("{MANIFEST}# concurrent change\n"))?;
        let stale_result = commit_manifest_edit(&manifest_path, &edit);

        assert_eq!(
            (
                backup == MANIFEST,
                written.contains("  worker:"),
                matches!(stale_result, Err(ManifestEditError::ConcurrentChange(_))),
            ),
            (true, true, true)
        );
        Ok(())
    }

    #[test]
    fn add_manual_link_should_preserve_unrelated_manifest_bytes() {
        let link = ManualLinkConfig {
            from: "service:web".to_owned(),
            to: "service:api".to_owned(),
            relation: EdgeKind::Consumes,
            contract: Some("POST /orders".to_owned()),
            reason: "Manual checkout boundary".to_owned(),
            suppress: false,
        };

        let result = preview_add_manual_link(MANIFEST, &link);

        assert!(matches!(
            result,
            Ok(edit)
                if edit.updated_source.starts_with("# leading comment\n")
                    && edit.updated_source.contains("manualLinks:\n")
                    && edit.updated_source.contains("  - from: service:web\n")
                    && edit.updated_source.ends_with("    suppress: false\n")
        ));
    }

    #[test]
    fn add_manual_link_should_append_to_existing_section() {
        let existing = format!(
            "{MANIFEST}manualLinks:\n  - from: service:web\n    to: service:api\n    relation: consumes\n    contract: null\n    reason: Existing\n    suppress: false\n"
        );
        let link = ManualLinkConfig {
            from: "service:worker".to_owned(),
            to: "service:api".to_owned(),
            relation: EdgeKind::Consumes,
            contract: None,
            reason: "Explicit worker dependency".to_owned(),
            suppress: false,
        };

        let result = preview_add_manual_link(&existing, &link);

        assert!(matches!(
            result,
            Ok(edit)
                if edit.updated_source.matches("manualLinks:").count() == 1
                    && edit.updated_source.contains("  - from: service:worker\n")
        ));
    }
}
