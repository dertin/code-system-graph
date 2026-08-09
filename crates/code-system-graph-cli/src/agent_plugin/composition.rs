use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use code_system_graph_model::stable_id_bytes;
use serde::{Deserialize, Serialize};

use super::filesystem::{
    verify_binding_ownership, verify_existing, write_file_atomically, write_new_atomically
};
use super::render::render_existing_integration;
use super::{
    AgentPluginError, AgentPluginGeneratorBuild, AgentPluginMcpBinding, AgentPluginUninstallReport, AgentPluginUninstallRequest, BINDING_RELATIVE_PATH, INTEGRATION_RECEIPT_RELATIVE_PATH, canonicalize_directory, conflict, pretty_json, read_json_file, required_json_string, unicode_path, validate_component_name, write_local_binding
};

const RECEIPT_GENERATOR: &str = "csgraph plugin integration";
const MANAGED_FILE_HASH_NAMESPACE: &str = "agent-plugin-integration-file-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IntegrationReceipt {
    schema_version: u8,
    generator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generator_build: Option<AgentPluginGeneratorBuild>,
    base_plugin_name: String,
    workspace: String,
    mcp_server_name: String,
    routing_skill: String,
    managed_documents: Vec<String>,
    managed_files: BTreeMap<String, String>,
    managed_local_files: BTreeMap<String, String>,
}

struct DocumentUpdate {
    path: PathBuf,
    original: Vec<u8>,
    updated: Vec<u8>,
}

struct StagedRemoval {
    skill_root: PathBuf,
    skill_backup: PathBuf,
    local_root: PathBuf,
    local_backup: PathBuf,
}

pub(super) fn validate_composed_base(
    base: &Path,
    mcp_server_name: &str,
    routing_skill: &str,
) -> Result<String, AgentPluginError> {
    validate_component_name(mcp_server_name, "MCP server")?;
    validate_component_name(routing_skill, "routing skill")?;
    let plugin = read_json_file(&base.join("plugin.json"), "plugin.json")?;
    let plugin_name = required_json_string(&plugin, "plugin.json", "name")?.to_owned();
    validate_component_name(&plugin_name, "base plugin")?;
    let ignore_path = base.join(".gitignore");
    let ignore = fs::read_to_string(&ignore_path).map_err(|source| AgentPluginError::Resolve {
        path: ignore_path,
        source,
    })?;
    if !ignore.lines().any(|line| line.trim() == "/.local/") {
        return Err(AgentPluginError::InvalidBase {
            file: ".gitignore",
            detail: "portable existing-plugin integration requires the exact `/.local/` rule"
                .to_owned(),
        });
    }
    Ok(plugin_name)
}

pub(super) fn install_composed_integration(
    base: &Path,
    plugin_name: &str,
    workspace: &str,
    mcp_server_name: &str,
    routing_skill: &str,
    binding: &AgentPluginMcpBinding,
    replace_generated: bool,
) -> Result<(Vec<String>, bool), AgentPluginError> {
    let has_codex_manifest = base.join(".codex-plugin/plugin.json").is_file();
    let (server, skill_files) = render_existing_integration(
        workspace,
        mcp_server_name,
        routing_skill,
        has_codex_manifest,
    )?;
    let mut local_files = super::binding_files(binding)?;
    let receipt = IntegrationReceipt {
        schema_version: 1,
        generator: RECEIPT_GENERATOR.to_owned(),
        generator_build: binding.generator_build.clone(),
        base_plugin_name: plugin_name.to_owned(),
        workspace: workspace.to_owned(),
        mcp_server_name: mcp_server_name.to_owned(),
        routing_skill: routing_skill.to_owned(),
        managed_documents: if has_codex_manifest {
            vec![
                "mcp.json".to_owned(),
                ".codex-plugin/plugin.json".to_owned(),
            ]
        } else {
            vec!["mcp.json".to_owned()]
        },
        managed_files: skill_files
            .iter()
            .map(|(path, contents)| {
                (
                    path.clone(),
                    stable_id_bytes(MANAGED_FILE_HASH_NAMESPACE, contents),
                )
            })
            .collect(),
        managed_local_files: local_files
            .iter()
            .map(|(path, contents)| {
                (
                    path.clone(),
                    stable_id_bytes(MANAGED_FILE_HASH_NAMESPACE, contents),
                )
            })
            .collect(),
    };
    local_files.insert(
        "plugin-integration.json".to_owned(),
        pretty_json(
            "plugin-integration.json",
            &serde_json::to_value(&receipt).map_err(|source| AgentPluginError::Json {
                file: "plugin-integration.json",
                source,
            })?,
        )?,
    );

    let document_updates = install_document_updates(base, mcp_server_name, &server)?;
    let documents_changed = !document_updates.is_empty();
    let skill_root = base.join(format!("skills/{routing_skill}"));
    let skill_subtree = skill_subtree(&skill_files, routing_skill)?;
    let skill_exists = fs::symlink_metadata(&skill_root).is_ok();
    if skill_exists {
        verify_existing(&skill_root, &skill_subtree)?;
    }

    apply_document_updates(&document_updates)?;
    if !skill_exists && let Err(error) = write_new_atomically(&skill_root, &skill_subtree) {
        rollback_document_updates(&document_updates);
        return Err(error);
    }
    let binding_changed = match write_local_binding(base, &local_files, replace_generated) {
        Ok(changed) => changed,
        Err(error) => {
            if !skill_exists {
                let _ = fs::remove_dir_all(&skill_root);
            }
            rollback_document_updates(&document_updates);
            return Err(error);
        }
    };

    let mut files = vec![
        "mcp.json".to_owned(),
        BINDING_RELATIVE_PATH.to_owned(),
        INTEGRATION_RECEIPT_RELATIVE_PATH.to_owned(),
    ];
    if has_codex_manifest {
        files.push(".codex-plugin/plugin.json".to_owned());
    }
    files.extend(skill_files.keys().cloned());
    files.sort();
    Ok((files, documents_changed || !skill_exists || binding_changed))
}

/// Removes an unchanged integration previously installed into a composed Agent Plugin.
///
/// # Errors
///
/// Returns [`AgentPluginError`] when the ownership receipt is absent or invalid, a managed
/// component changed after installation, or the update cannot be completed safely.
pub fn uninstall_composed_integration(
    request: &AgentPluginUninstallRequest,
) -> Result<AgentPluginUninstallReport, AgentPluginError> {
    validate_component_name(&request.mcp_server_name, "MCP server")?;
    validate_component_name(&request.routing_skill, "routing skill")?;
    let base = canonicalize_directory(&request.output)?;
    let (plugin_name, receipt) = load_owned_receipt(&base, request)?;
    verify_binding_ownership(&base.join(".local/code-system-graph"))?;
    verify_managed_local_files(&base, &receipt)?;
    verify_managed_skill(&base, &receipt)?;
    let (server, _) = render_existing_integration(
        &receipt.workspace,
        &receipt.mcp_server_name,
        &receipt.routing_skill,
        false,
    )?;

    let document_updates = uninstall_document_updates(
        &base,
        &receipt.managed_documents,
        &receipt.mcp_server_name,
        &server,
    )?;
    let staged = stage_managed_removal(&base, &receipt.routing_skill)?;
    if let Err(error) = apply_document_updates(&document_updates) {
        staged.restore();
        return Err(error);
    }
    if let Err(error) = staged.commit() {
        rollback_document_updates(&document_updates);
        return Err(error);
    }

    let mut removed = receipt.managed_files.keys().cloned().collect::<Vec<_>>();
    removed.extend([
        BINDING_RELATIVE_PATH.to_owned(),
        INTEGRATION_RECEIPT_RELATIVE_PATH.to_owned(),
    ]);
    for document in &receipt.managed_documents {
        removed.push(format!(
            "{document}#/{}/{}",
            "mcpServers", receipt.mcp_server_name
        ));
    }
    removed.sort();
    Ok(AgentPluginUninstallReport {
        schema_version: 1,
        plugin_name,
        mcp_server_name: request.mcp_server_name.clone(),
        skill_name: request.routing_skill.clone(),
        output: unicode_path(&base)?,
        removed,
        changed: true,
    })
}

fn merge_server(
    bytes: &[u8],
    file: &'static str,
    name: &str,
    expected: &serde_json::Value,
) -> Result<Option<Vec<u8>>, AgentPluginError> {
    let mut document: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|source| AgentPluginError::Json { file, source })?;
    let servers = document
        .get_mut("mcpServers")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| AgentPluginError::InvalidBase {
            file,
            detail: "`mcpServers` must be an object".to_owned(),
        })?;
    match servers.get(name) {
        Some(actual) if actual == expected => Ok(None),
        Some(_) => Err(AgentPluginError::InvalidBase {
            file,
            detail: format!("MCP server `{name}` conflicts with the managed integration"),
        }),
        None => {
            servers.insert(name.to_owned(), expected.clone());
            Ok(Some(pretty_json(file, &document)?))
        }
    }
}

fn remove_server(
    bytes: &[u8],
    file: &'static str,
    name: &str,
    expected: &serde_json::Value,
) -> Result<Vec<u8>, AgentPluginError> {
    let mut document: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|source| AgentPluginError::Json { file, source })?;
    let servers = document
        .get_mut("mcpServers")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| AgentPluginError::InvalidBase {
            file,
            detail: "`mcpServers` must be an object".to_owned(),
        })?;
    match servers.get(name) {
        Some(actual) if actual == expected => {
            servers.remove(name);
            pretty_json(file, &document)
        }
        Some(_) => Err(conflict(
            Path::new(file),
            &format!("MCP server `{name}` changed after installation"),
        )),
        None => Err(conflict(
            Path::new(file),
            &format!("managed MCP server `{name}` is missing"),
        )),
    }
}

fn install_document_updates(
    base: &Path,
    server_name: &str,
    server: &serde_json::Value,
) -> Result<Vec<DocumentUpdate>, AgentPluginError> {
    let mut updates = Vec::new();
    let portable_path = base.join("mcp.json");
    let portable = read_bytes(&portable_path)?;
    if let Some(updated) = merge_server(&portable, "mcp.json", server_name, server)? {
        updates.push(DocumentUpdate {
            path: portable_path,
            original: portable,
            updated,
        });
    }
    let codex_path = base.join(".codex-plugin/plugin.json");
    if codex_path.exists() {
        let codex = read_bytes(&codex_path)?;
        if let Some(updated) =
            merge_server(&codex, ".codex-plugin/plugin.json", server_name, server)?
        {
            updates.push(DocumentUpdate {
                path: codex_path,
                original: codex,
                updated,
            });
        }
    }
    Ok(updates)
}

fn uninstall_document_updates(
    base: &Path,
    managed_documents: &[String],
    server_name: &str,
    server: &serde_json::Value,
) -> Result<Vec<DocumentUpdate>, AgentPluginError> {
    let mut updates = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for relative in managed_documents {
        let file = match relative.as_str() {
            "mcp.json" => "mcp.json",
            ".codex-plugin/plugin.json" => ".codex-plugin/plugin.json",
            _ => {
                return Err(conflict(
                    base,
                    "integration receipt contains an unsupported managed document",
                ));
            }
        };
        if !seen.insert(relative.as_str()) {
            return Err(conflict(
                base,
                "integration receipt contains duplicate managed documents",
            ));
        }
        let path = base.join(relative);
        let original = read_bytes(&path)?;
        let rewritten = remove_server(&original, file, server_name, server)?;
        updates.push(DocumentUpdate {
            path,
            original,
            updated: rewritten,
        });
    }
    if !seen.contains("mcp.json") {
        return Err(conflict(
            base,
            "integration receipt does not own the portable MCP document",
        ));
    }
    Ok(updates)
}

fn apply_document_updates(updates: &[DocumentUpdate]) -> Result<(), AgentPluginError> {
    for (index, update) in updates.iter().enumerate() {
        if let Err(error) = write_file_atomically(&update.path, &update.updated) {
            rollback_document_updates(&updates[..index]);
            return Err(error);
        }
    }
    Ok(())
}

fn rollback_document_updates(updates: &[DocumentUpdate]) {
    for update in updates.iter().rev() {
        let _ = write_file_atomically(&update.path, &update.original);
    }
}

fn load_owned_receipt(
    base: &Path,
    request: &AgentPluginUninstallRequest,
) -> Result<(String, IntegrationReceipt), AgentPluginError> {
    let plugin = read_json_file(&base.join("plugin.json"), "plugin.json")?;
    let plugin_name = required_json_string(&plugin, "plugin.json", "name")?.to_owned();
    let receipt_path = base.join(INTEGRATION_RECEIPT_RELATIVE_PATH);
    let receipt: IntegrationReceipt =
        serde_json::from_slice(&read_bytes(&receipt_path)?).map_err(|source| {
            AgentPluginError::Json {
                file: "plugin-integration.json",
                source,
            }
        })?;
    if receipt.schema_version != 1
        || receipt.generator != RECEIPT_GENERATOR
        || receipt.base_plugin_name != plugin_name
        || receipt.mcp_server_name != request.mcp_server_name
        || receipt.routing_skill != request.routing_skill
    {
        return Err(conflict(
            &receipt_path,
            "integration receipt does not own the requested plugin components",
        ));
    }
    Ok((plugin_name, receipt))
}

fn stage_managed_removal(
    base: &Path,
    routing_skill: &str,
) -> Result<StagedRemoval, AgentPluginError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let staged = StagedRemoval {
        skill_root: base.join(format!("skills/{routing_skill}")),
        skill_backup: base.join(format!("skills/.{routing_skill}.uninstall-{nonce}")),
        local_root: base.join(".local/code-system-graph"),
        local_backup: base.join(format!(".local/.code-system-graph.uninstall-{nonce}")),
    };
    fs::rename(&staged.skill_root, &staged.skill_backup).map_err(|source| {
        AgentPluginError::Write {
            path: staged.skill_root.clone(),
            source,
        }
    })?;
    if let Err(source) = fs::rename(&staged.local_root, &staged.local_backup) {
        let _ = fs::rename(&staged.skill_backup, &staged.skill_root);
        return Err(AgentPluginError::Write {
            path: staged.local_root,
            source,
        });
    }
    Ok(staged)
}

impl StagedRemoval {
    fn restore(self) {
        let _ = fs::rename(self.skill_backup, self.skill_root);
        let _ = fs::rename(self.local_backup, self.local_root);
    }

    fn commit(self) -> Result<(), AgentPluginError> {
        self.commit_with(|path| fs::remove_dir_all(path))
    }

    fn commit_with(
        self,
        mut remove: impl FnMut(&Path) -> std::io::Result<()>,
    ) -> Result<(), AgentPluginError> {
        let skill_restore = suffixed_path(&self.skill_backup, ".restore");
        let local_restore = suffixed_path(&self.local_backup, ".restore");
        if let Err(error) = copy_directory(&self.skill_backup, &skill_restore) {
            self.restore();
            return Err(error);
        }
        if let Err(error) = copy_directory(&self.local_backup, &local_restore) {
            let _ = fs::remove_dir_all(&skill_restore);
            self.restore();
            return Err(error);
        }

        for backup in [&self.skill_backup, &self.local_backup] {
            if let Err(source) = remove(backup) {
                let failed_path = backup.clone();
                restore_after_cleanup_failure(&self.skill_backup, &skill_restore, &self.skill_root);
                restore_after_cleanup_failure(&self.local_backup, &local_restore, &self.local_root);
                return Err(AgentPluginError::Write {
                    path: failed_path,
                    source,
                });
            }
        }
        let _ = fs::remove_dir_all(skill_restore);
        let _ = fs::remove_dir_all(local_restore);
        Ok(())
    }
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), AgentPluginError> {
    fs::create_dir(destination).map_err(|source| AgentPluginError::Write {
        path: destination.to_path_buf(),
        source,
    })?;
    let result = (|| {
        for entry in fs::read_dir(source).map_err(|error| AgentPluginError::Resolve {
            path: source.to_path_buf(),
            source: error,
        })? {
            let entry = entry.map_err(|error| AgentPluginError::Resolve {
                path: source.to_path_buf(),
                source: error,
            })?;
            let path = entry.path();
            let target = destination.join(entry.file_name());
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| AgentPluginError::Resolve {
                    path: path.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(conflict(&path, "managed backups cannot contain symlinks"));
            }
            if metadata.is_dir() {
                copy_directory(&path, &target)?;
            } else if metadata.is_file() {
                fs::copy(&path, &target).map_err(|source| AgentPluginError::Write {
                    path: target,
                    source,
                })?;
            } else {
                return Err(conflict(
                    &path,
                    "managed backups must contain only regular files",
                ));
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn restore_after_cleanup_failure(backup: &Path, restore: &Path, target: &Path) {
    if backup.exists() {
        let failed = suffixed_path(backup, ".failed");
        if fs::rename(backup, &failed).is_ok() {
            if fs::rename(restore, target).is_ok() {
                let _ = fs::remove_dir_all(failed);
                return;
            }
            let _ = fs::rename(failed, backup);
        }
        if fs::rename(backup, target).is_ok() {
            let _ = fs::remove_dir_all(restore);
            return;
        }
    }
    let _ = fs::rename(restore, target);
}

fn suffixed_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .expect("staged removal path has a file name")
        .to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, AgentPluginError> {
    fs::read(path).map_err(|source| AgentPluginError::Resolve {
        path: path.to_path_buf(),
        source,
    })
}

fn verify_managed_local_files(
    base: &Path,
    receipt: &IntegrationReceipt,
) -> Result<(), AgentPluginError> {
    let local_root = base.join(".local/code-system-graph");
    let mut expected = BTreeMap::new();
    for (relative, expected_hash) in &receipt.managed_local_files {
        if relative != "mcp-binding.json" {
            return Err(conflict(
                &local_root,
                "integration receipt contains an unsupported managed local file",
            ));
        }
        let path = local_root.join(relative);
        let contents = read_bytes(&path)?;
        if stable_id_bytes(MANAGED_FILE_HASH_NAMESPACE, &contents) != *expected_hash {
            return Err(conflict(
                &path,
                "managed local binding changed after installation; refusing to delete it",
            ));
        }
        expected.insert(relative.clone(), contents);
    }
    if !expected.contains_key("mcp-binding.json") {
        return Err(conflict(
            &local_root,
            "integration receipt does not own the local MCP binding",
        ));
    }
    expected.insert(
        "plugin-integration.json".to_owned(),
        read_bytes(&base.join(INTEGRATION_RECEIPT_RELATIVE_PATH))?,
    );
    verify_existing(&local_root, &expected)
}

fn skill_subtree(
    files: &BTreeMap<String, Vec<u8>>,
    skill_name: &str,
) -> Result<BTreeMap<String, Vec<u8>>, AgentPluginError> {
    let prefix = format!("skills/{skill_name}/");
    files
        .iter()
        .map(|(path, contents)| {
            path.strip_prefix(&prefix)
                .map(|relative| (relative.to_owned(), contents.clone()))
                .ok_or_else(|| AgentPluginError::Template {
                    file: "skills/code-system-graph/SKILL.md",
                    detail: "managed skill path escaped its root".to_owned(),
                })
        })
        .collect()
}

fn verify_managed_skill(base: &Path, receipt: &IntegrationReceipt) -> Result<(), AgentPluginError> {
    let prefix = format!("skills/{}/", receipt.routing_skill);
    let mut files = BTreeMap::new();
    for (relative, expected_hash) in &receipt.managed_files {
        let subtree = relative.strip_prefix(&prefix).ok_or_else(|| {
            conflict(
                base,
                "integration receipt contains a path outside its skill root",
            )
        })?;
        let path = base.join(relative);
        let contents = fs::read(&path).map_err(|source| AgentPluginError::Resolve {
            path: path.clone(),
            source,
        })?;
        if stable_id_bytes(MANAGED_FILE_HASH_NAMESPACE, &contents) != *expected_hash {
            return Err(conflict(
                &path,
                "managed skill changed after installation; refusing to delete it",
            ));
        }
        files.insert(subtree.to_owned(), contents);
    }
    verify_existing(
        &base.join(format!("skills/{}", receipt.routing_skill)),
        &files,
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;

    use super::StagedRemoval;

    #[test]
    fn staged_removal_should_restore_both_trees_when_cleanup_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let skill_root = temporary.path().join("skill");
        let skill_backup = temporary.path().join("skill.backup");
        let local_root = temporary.path().join("local");
        let local_backup = temporary.path().join("local.backup");
        fs::create_dir(&skill_backup)?;
        fs::write(skill_backup.join("SKILL.md"), "managed skill\n")?;
        fs::create_dir(&local_backup)?;
        fs::write(local_backup.join("mcp-binding.json"), "managed binding\n")?;
        let staged = StagedRemoval {
            skill_root: skill_root.clone(),
            skill_backup,
            local_root: local_root.clone(),
            local_backup,
        };
        let calls = Cell::new(0_usize);

        let result = staged.commit_with(|path| {
            let call = calls.get();
            calls.set(call + 1);
            if call == 1 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected cleanup failure",
                ));
            }
            fs::remove_dir_all(path)
        });

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(skill_root.join("SKILL.md"))?,
            "managed skill\n"
        );
        assert_eq!(
            fs::read_to_string(local_root.join("mcp-binding.json"))?,
            "managed binding\n"
        );
        Ok(())
    }
}
