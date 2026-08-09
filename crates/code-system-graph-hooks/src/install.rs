use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::managed_root::{MAX_HOST_FILE_BYTES, ManagedRoot};
use crate::templates::{
    ANTIGRAVITY_LIMITATION, CURSOR_LIMITATION, CURSOR_RULE, GEMINI_HOOK_DESCRIPTION, HOOK_STATUS_MESSAGE, STATIC_ROUTING_CODEGRAPH, STATIC_ROUTING_NATIVE, STATIC_RULE, STRICT_GATE
};
use crate::types::{
    HookError, HookMode, HookStatus, HostKind, InstallReport, InstallRequest, UninstallReport
};

const PRODUCT_MARKER: &str = "code-system-graph-hooks:v1";
const STATE_DIRECTORY: &str = ".code-system-graph/hooks";
const GENERATED_STATE_IGNORE_RULE: &[u8] = b".code-system-graph/";

#[derive(Debug, Clone, Copy)]
enum HostProtocol {
    Json { event: &'static str },
    Guidance,
}

#[derive(Debug)]
struct HostSpec {
    path: PathBuf,
    protocol: HostProtocol,
    limitation: Option<&'static str>,
}

#[derive(Debug, Serialize, Deserialize)]
struct InstallState {
    marker: String,
    host: HostKind,
    mode: HookMode,
    host_file: PathBuf,
    #[serde(default)]
    codegraph_enabled: bool,
}

/// Installs or updates one host integration with atomic, marker-scoped writes.
///
/// Existing host configuration is merged rather than replaced. A timestamped backup is created
/// before each changed existing file. Repeating an identical installation is a no-op.
///
/// # Errors
///
/// Returns [`HookError`] for invalid roots or host configuration and for failed filesystem,
/// serialization, backup, or atomic-write operations.
pub fn install(request: &InstallRequest) -> Result<InstallReport, HookError> {
    validate_request(request)?;
    let managed = ManagedRoot::open(&request.root)?;
    let spec = host_spec(request);
    let state_relative = state_relative_path(request);
    let mut backups = Vec::new();
    let mut warnings = duplicate_warnings(&managed, request, &spec)?;
    let runtime = request.code_system_graph_binary.with_file_name(format!(
        "code-system-graph-hooks{}",
        std::env::consts::EXE_SUFFIX
    ));

    let routing_changed = match spec.protocol {
        HostProtocol::Json { event } => {
            install_json_hook(&managed, request, &spec.path, event, &runtime, &mut backups)?
        }
        HostProtocol::Guidance => install_guidance(&managed, request, &spec.path, &mut backups)?,
    };
    let strict_changed = if request.mode == HookMode::Strict {
        install_strict_gate(&managed, request, &mut backups, &mut warnings)?
    } else {
        remove_strict_gate(&managed, request, &mut backups, &mut Vec::new())?
    };
    let state = InstallState {
        marker: PRODUCT_MARKER.to_owned(),
        host: request.host,
        mode: request.mode,
        host_file: managed.absolute(&spec.path),
        codegraph_enabled: request.codegraph_enabled,
    };
    let state_changed =
        write_json_if_changed(&managed, &state_relative, &state, false, &mut backups)?;
    restrict_file(&managed.absolute(&state_relative))?;
    let (gitignore_path, gitignore_updated) = configure_generated_state_ignore(&managed)?;

    Ok(InstallReport {
        changed: routing_changed || strict_changed || state_changed || gitignore_updated,
        host_file: managed.absolute(&spec.path),
        state_file: managed.absolute(&state_relative),
        gitignore_path,
        gitignore_updated,
        backups,
        warnings,
        limitation: spec.limitation.map(str::to_owned),
    })
}

/// Inspects marker-owned host and strict-gate content without changing files.
///
/// # Errors
///
/// Returns [`HookError`] when existing host configuration cannot be read or parsed.
pub fn status(request: &InstallRequest) -> Result<HookStatus, HookError> {
    validate_request(request)?;
    let managed = ManagedRoot::open(&request.root)?;
    let spec = host_spec(request);
    let state_relative = state_relative_path(request);
    let installed_state = read_install_state(&managed, &state_relative)?;
    let policy = installed_state
        .as_ref()
        .map_or(request.codegraph_enabled, |state| state.codegraph_enabled);
    let runtime = request.code_system_graph_binary.with_file_name(format!(
        "code-system-graph-hooks{}",
        std::env::consts::EXE_SUFFIX
    ));
    let routing_installed = match spec.protocol {
        HostProtocol::Json { event } => {
            json_hook_installed(&managed, request, &spec.path, event, &runtime, policy)?
        }
        HostProtocol::Guidance => {
            file_contains(&managed, &spec.path, &guidance_block(request.host, policy))?
        }
    };
    let strict_gate_installed = if request.mode == HookMode::Strict {
        file_contains(
            &managed,
            &git_pre_commit(&managed)?,
            &begin_marker(request.host),
        )?
    } else if let Some(path) = optional_git_pre_commit(&managed)? {
        file_contains(&managed, &path, &begin_marker(request.host))?
    } else {
        false
    };
    let policy_matches = installed_state
        .as_ref()
        .is_none_or(|state| state.codegraph_enabled == request.codegraph_enabled);
    let installed = routing_installed
        && (request.mode == HookMode::Advisory || strict_gate_installed)
        && installed_state.is_some()
        && policy_matches;
    let warnings = duplicate_warnings(&managed, request, &spec)?;

    Ok(HookStatus {
        installed,
        routing_installed,
        strict_gate_installed,
        host_file: managed.absolute(&spec.path),
        state_file: managed.absolute(&state_relative),
        warnings,
        limitation: spec.limitation.map(str::to_owned),
    })
}

/// Removes only marker-owned content and merges around unrelated host configuration.
///
/// Existing files are backed up immediately before a changed merge. Backups are not blindly
/// restored because doing so could discard edits made after installation.
///
/// # Errors
///
/// Returns [`HookError`] when host configuration cannot be parsed or a backup, merge, deletion,
/// permission change, or atomic write fails.
pub fn uninstall(request: &InstallRequest) -> Result<UninstallReport, HookError> {
    validate_request(request)?;
    let managed = ManagedRoot::open(&request.root)?;
    let spec = host_spec(request);
    let state_relative = state_relative_path(request);
    let mut backups = Vec::new();
    let mut removed_files = Vec::new();
    let mut warnings = Vec::new();

    let routing_changed = match spec.protocol {
        HostProtocol::Json { event } => {
            uninstall_json_hook(&managed, &spec.path, event, &mut backups)?
        }
        HostProtocol::Guidance => remove_guidance(
            &managed,
            request,
            &spec.path,
            &mut backups,
            &mut removed_files,
        )?,
    };
    let strict_changed = remove_strict_gate(&managed, request, &mut backups, &mut removed_files)?;
    let state_changed = if managed.remove_file_if_exists(&state_relative)? {
        removed_files.push(managed.absolute(&state_relative));
        true
    } else {
        false
    };

    warnings.extend(duplicate_warnings(&managed, request, &spec)?);
    Ok(UninstallReport {
        changed: routing_changed || strict_changed || state_changed,
        backups,
        removed_files,
        warnings,
    })
}

fn validate_request(request: &InstallRequest) -> Result<(), HookError> {
    if request.workspace.trim().is_empty() {
        return Err(HookError::InvalidConfiguration {
            path: request.root.clone(),
            message: "workspace name must not be empty".to_owned(),
        });
    }
    if request.repository.trim().is_empty() {
        return Err(HookError::InvalidConfiguration {
            path: request.root.clone(),
            message: "repository alias must not be empty".to_owned(),
        });
    }
    Ok(())
}

fn host_spec(request: &InstallRequest) -> HostSpec {
    let (relative, protocol, limitation) = match request.host {
        HostKind::ClaudeCode => (
            ".claude/settings.local.json",
            HostProtocol::Json {
                event: "UserPromptSubmit",
            },
            None,
        ),
        HostKind::Codex => (
            ".codex/hooks.json",
            HostProtocol::Json {
                event: "UserPromptSubmit",
            },
            None,
        ),
        HostKind::Gemini => (
            ".gemini/settings.json",
            HostProtocol::Json {
                event: "BeforeAgent",
            },
            None,
        ),
        HostKind::Antigravity => (
            ".agents/rules/code-system-graph-routing.md",
            HostProtocol::Guidance,
            Some(ANTIGRAVITY_LIMITATION.trim_end()),
        ),
        HostKind::Cursor => (
            ".cursor/rules/code-system-graph-routing.mdc",
            HostProtocol::Guidance,
            Some(CURSOR_LIMITATION.trim_end()),
        ),
    };
    HostSpec {
        path: PathBuf::from(relative),
        protocol,
        limitation,
    }
}

fn state_relative_path(request: &InstallRequest) -> PathBuf {
    PathBuf::from(STATE_DIRECTORY).join(format!("install-{}.json", request.host.as_str()))
}

fn read_install_state(
    managed: &ManagedRoot,
    relative: &Path,
) -> Result<Option<InstallState>, HookError> {
    let Some(content) = managed.read_optional_utf8_bounded(relative, MAX_HOST_FILE_BYTES)? else {
        return Ok(None);
    };
    let state =
        serde_json::from_str(&content).map_err(|error| HookError::InvalidConfiguration {
            path: managed.absolute(relative),
            message: error.to_string(),
        })?;
    Ok(Some(state))
}

fn configure_generated_state_ignore(
    managed: &ManagedRoot,
) -> Result<(Option<PathBuf>, bool), HookError> {
    let canonical_root = managed.root();
    if !belongs_to_git_worktree(canonical_root) {
        return Ok((None, false));
    }
    let (relative, updated) = ensure_generated_state_ignored(managed)?;
    Ok((Some(managed.absolute(&relative)), updated))
}

fn belongs_to_git_worktree(root: &Path) -> bool {
    root.ancestors()
        .any(|ancestor| valid_git_worktree_marker(&ancestor.join(".git")))
}

fn valid_git_worktree_marker(marker: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(marker) else {
        return false;
    };
    if metadata.file_type().is_dir() {
        return fs::symlink_metadata(marker.join("HEAD"))
            .is_ok_and(|head| head.file_type().is_file());
    }
    if !metadata.file_type().is_file() || metadata.len() > 4_096 {
        return false;
    }
    let Ok(source) = fs::read_to_string(marker) else {
        return false;
    };
    source
        .lines()
        .next()
        .is_some_and(|line| line.trim_start().starts_with("gitdir:"))
}

fn ensure_generated_state_ignored(managed: &ManagedRoot) -> Result<(PathBuf, bool), HookError> {
    let relative = PathBuf::from(".gitignore");
    let content = managed
        .read_optional_bytes_bounded(&relative, MAX_HOST_FILE_BYTES)?
        .unwrap_or_default();
    if generated_state_is_ignored(&content) {
        return Ok((relative, false));
    }
    let mut updated = content;
    if !updated.is_empty() && !updated.ends_with(b"\n") {
        updated.push(b'\n');
    }
    updated.extend_from_slice(GENERATED_STATE_IGNORE_RULE);
    updated.push(b'\n');
    managed.atomic_write(&relative, &updated)?;
    Ok((relative, true))
}

fn generated_state_is_ignored(content: &[u8]) -> bool {
    content
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").map_or(line, |trimmed| trimmed))
        .fold(None, |state, line| match line {
            b".code-system-graph/"
            | b"/.code-system-graph/"
            | b".code-system-graph"
            | b"/.code-system-graph" => Some(true),
            b"!.code-system-graph/"
            | b"!/.code-system-graph/"
            | b"!.code-system-graph"
            | b"!/.code-system-graph" => Some(false),
            _ => state,
        })
        .unwrap_or(false)
}

fn install_json_hook(
    managed: &ManagedRoot,
    request: &InstallRequest,
    relative: &Path,
    event: &str,
    runtime: &Path,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let (mut root, existed) = read_json_object(managed, relative)?;
    let hooks = object_field_mut(&mut root, "hooks", managed, relative)?;
    let entries = array_field_mut(hooks, event, managed, relative)?;
    let owned = owned_json_entry(request, runtime);
    if entries.iter().any(is_owned_json) {
        if entries.iter().any(|entry| entry == &owned) {
            return Ok(false);
        }
        entries.retain(|entry| !is_owned_json(entry));
    }
    entries.push(owned);
    write_value(managed, relative, &root, existed, backups)
}

fn owned_json_entry(request: &InstallRequest, runtime: &Path) -> Value {
    owned_json_entry_with_policy(request, runtime, request.codegraph_enabled)
}

fn owned_json_entry_with_policy(
    request: &InstallRequest,
    runtime: &Path,
    codegraph_enabled: bool,
) -> Value {
    let command = format!(
        "{} route --host {} --root {} --codegraph-enabled {} --marker {}",
        shell_quote(runtime.as_os_str().to_string_lossy().as_ref()),
        request.host.as_str(),
        shell_quote(request.root.as_os_str().to_string_lossy().as_ref()),
        codegraph_enabled,
        PRODUCT_MARKER
    );
    match request.host {
        HostKind::ClaudeCode | HostKind::Codex => json!({
            "hooks": [{
                "type": "command",
                "command": command,
                "timeout": 5,
                "statusMessage": HOOK_STATUS_MESSAGE.trim_end()
            }]
        }),
        HostKind::Gemini => json!({
            "matcher": "*",
            "hooks": [{
                "name": PRODUCT_MARKER,
                "type": "command",
                "command": command,
                "timeout": 5000,
                "description": GEMINI_HOOK_DESCRIPTION.trim_end()
            }]
        }),
        HostKind::Antigravity | HostKind::Cursor => Value::Null,
    }
}

fn uninstall_json_hook(
    managed: &ManagedRoot,
    relative: &Path,
    event: &str,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    if !managed.regular_file_exists(relative)? {
        return Ok(false);
    }
    let (mut root, _) = read_json_object(managed, relative)?;
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    let Some(entries) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
        return Ok(false);
    };
    let original_len = entries.len();
    entries.retain(|entry| !is_owned_json(entry));
    if entries.len() == original_len {
        return Ok(false);
    }
    if entries.is_empty() {
        hooks.remove(event);
    }
    if hooks.is_empty() {
        root.as_object_mut().map(|object| object.remove("hooks"));
    }
    write_value(managed, relative, &root, true, backups)
}

fn json_hook_installed(
    managed: &ManagedRoot,
    request: &InstallRequest,
    relative: &Path,
    event: &str,
    runtime: &Path,
    policy: bool,
) -> Result<bool, HookError> {
    if !managed.regular_file_exists(relative)? {
        return Ok(false);
    }
    let owned = owned_json_entry_with_policy(request, runtime, policy);
    let (root, _) = read_json_object(managed, relative)?;
    Ok(root
        .get("hooks")
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.iter().any(|entry| entry == &owned)))
}

fn install_guidance(
    managed: &ManagedRoot,
    request: &InstallRequest,
    relative: &Path,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let existing = read_optional_string(managed, relative)?;
    let marker = begin_marker(request.host);
    let block = guidance_block(request.host, request.codegraph_enabled);
    if existing
        .as_deref()
        .is_some_and(|content| content.contains(&block))
    {
        return Ok(false);
    }
    let existing = match existing {
        Some(content) if content.contains(&marker) => {
            let without_owned = remove_marked_block(&content, request.host).ok_or_else(|| {
                HookError::InvalidConfiguration {
                    path: managed.absolute(relative),
                    message: "managed guidance has an incomplete marker block".to_owned(),
                }
            })?;
            (!without_owned.trim().is_empty()).then_some(without_owned)
        }
        other => other,
    };
    let updated = match existing {
        Some(mut content) => {
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push('\n');
            content.push_str(&block);
            content
        }
        None => guidance_scaffold(request.host, &block),
    };
    write_string(
        managed,
        relative,
        &updated,
        managed.regular_file_exists(relative)?,
        backups,
    )
}

fn guidance_scaffold(host: HostKind, block: &str) -> String {
    if host == HostKind::Cursor {
        render_embedded_template(
            CURSOR_RULE,
            &[("PRODUCT_MARKER", PRODUCT_MARKER), ("BLOCK", block)],
        )
    } else {
        block.to_owned()
    }
}

fn guidance_block(host: HostKind, codegraph_enabled: bool) -> String {
    let routing = if codegraph_enabled {
        STATIC_ROUTING_CODEGRAPH.trim_end()
    } else {
        STATIC_ROUTING_NATIVE.trim_end()
    };
    render_embedded_template(
        STATIC_RULE,
        &[
            ("BEGIN_MARKER", &begin_marker(host)),
            ("ROUTING", routing),
            ("END_MARKER", &end_marker(host)),
        ],
    )
}

fn remove_guidance(
    managed: &ManagedRoot,
    request: &InstallRequest,
    relative: &Path,
    backups: &mut Vec<PathBuf>,
    removed_files: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let Some(content) = read_optional_string(managed, relative)? else {
        return Ok(false);
    };
    let Some(updated) = remove_marked_block(&content, request.host) else {
        return Ok(false);
    };
    let generated_cursor_scaffold = request.host == HostKind::Cursor
        && updated.trim() == guidance_scaffold(HostKind::Cursor, "").trim();
    backup(managed, relative, backups)?;
    if updated.trim().is_empty() || generated_cursor_scaffold {
        managed.remove_file_if_exists(relative)?;
        removed_files.push(managed.absolute(relative));
    } else {
        managed.atomic_write(relative, updated.as_bytes())?;
    }
    Ok(true)
}

fn install_strict_gate(
    managed: &ManagedRoot,
    request: &InstallRequest,
    backups: &mut Vec<PathBuf>,
    warnings: &mut Vec<String>,
) -> Result<bool, HookError> {
    let relative = git_pre_commit(managed)?;
    let existing = read_optional_string(managed, &relative)?;
    let marker = begin_marker(request.host);
    if existing
        .as_deref()
        .is_some_and(|content| content.contains(&marker))
    {
        return Ok(false);
    }
    if existing.as_deref().is_some_and(contains_product_reference) {
        warnings.push(format!(
            "another Code System Graph pre-commit hook exists in `{}`; it was preserved",
            managed.absolute(&relative).display()
        ));
    }
    let block = strict_gate_block(request);
    let updated = match existing {
        Some(mut content) => {
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push('\n');
            content.push_str(&block);
            content
        }
        None => format!("#!/bin/sh\n\n{block}"),
    };
    let changed = write_string(
        managed,
        &relative,
        &updated,
        managed.regular_file_exists(&relative)?,
        backups,
    )?;
    if changed {
        make_executable(&managed.absolute(&relative))?;
    }
    Ok(changed)
}

fn strict_gate_block(request: &InstallRequest) -> String {
    let binary = shell_quote(
        request
            .code_system_graph_binary
            .as_os_str()
            .to_string_lossy()
            .as_ref(),
    );
    let database = shell_quote(request.database.as_os_str().to_string_lossy().as_ref());
    let workspace = shell_quote(&request.workspace);
    let repository = shell_quote(&request.repository);
    render_embedded_template(
        STRICT_GATE,
        &[
            ("BEGIN_MARKER", &begin_marker(request.host)),
            ("BINARY", &binary),
            ("DATABASE", &database),
            ("WORKSPACE", &workspace),
            ("REPOSITORY", &repository),
            ("END_MARKER", &end_marker(request.host)),
        ],
    )
}

fn render_embedded_template(template: &str, variables: &[(&str, &str)]) -> String {
    variables
        .iter()
        .fold(template.to_owned(), |rendered, (name, value)| {
            rendered.replace(&format!("{{{{{name}}}}}"), value)
        })
}

fn remove_strict_gate(
    managed: &ManagedRoot,
    request: &InstallRequest,
    backups: &mut Vec<PathBuf>,
    removed_files: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let Some(relative) = optional_git_pre_commit(managed)? else {
        return Ok(false);
    };
    let Some(content) = read_optional_string(managed, &relative)? else {
        return Ok(false);
    };
    let Some(updated) = remove_marked_block(&content, request.host) else {
        return Ok(false);
    };
    backup(managed, &relative, backups)?;
    if updated.trim() == "#!/bin/sh" || updated.trim().is_empty() {
        managed.remove_file_if_exists(&relative)?;
        removed_files.push(managed.absolute(&relative));
    } else {
        managed.atomic_write(&relative, updated.as_bytes())?;
        make_executable(&managed.absolute(&relative))?;
    }
    Ok(true)
}

fn git_pre_commit(managed: &ManagedRoot) -> Result<PathBuf, HookError> {
    optional_git_pre_commit(managed)?.ok_or_else(|| HookError::InvalidConfiguration {
        path: managed.root().to_path_buf(),
        message: "strict mode requires a Git repository worktree".to_owned(),
    })
}

fn optional_git_pre_commit(managed: &ManagedRoot) -> Result<Option<PathBuf>, HookError> {
    let dot_git = Path::new(".git");
    if managed.is_directory(dot_git)? {
        return Ok(Some(PathBuf::from(".git/hooks/pre-commit")));
    }
    if !managed.entry_exists(dot_git)? {
        return Ok(None);
    }
    let content = managed.read_utf8_bounded(dot_git, 4_096)?;
    let relative = content
        .trim()
        .strip_prefix("gitdir:")
        .map(str::trim)
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: managed.absolute(dot_git),
            message: "expected a Git directory or `gitdir:` pointer".to_owned(),
        })?;
    let git_dir = Path::new(relative);
    let git_dir = if git_dir.is_absolute() {
        let canonical = fs::canonicalize(git_dir).map_err(|source| HookError::Io {
            path: git_dir.to_path_buf(),
            source,
        })?;
        if !canonical.starts_with(managed.root()) {
            return Err(HookError::InvalidConfiguration {
                path: canonical,
                message: "Git metadata escapes the authorized repository root".to_owned(),
            });
        }
        match canonical.strip_prefix(managed.root()) {
            Ok(path) => path.to_path_buf(),
            Err(_) => {
                return Err(HookError::InvalidConfiguration {
                    path: canonical,
                    message: "Git metadata escapes the authorized repository root".to_owned(),
                });
            }
        }
    } else {
        git_dir.to_path_buf()
    };
    Ok(Some(git_dir.join("hooks/pre-commit")))
}

fn duplicate_warnings(
    managed: &ManagedRoot,
    request: &InstallRequest,
    spec: &HostSpec,
) -> Result<Vec<String>, HookError> {
    let mut warnings = Vec::new();
    match spec.protocol {
        HostProtocol::Json { event } if managed.regular_file_exists(&spec.path)? => {
            let (root, _) = read_json_object(managed, &spec.path)?;
            if root
                .get("hooks")
                .and_then(|hooks| hooks.get(event))
                .and_then(Value::as_array)
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| !is_owned_json(entry) && json_mentions_product(entry))
                })
            {
                warnings.push(format!(
                    "another Code System Graph hook exists in `{}` and was preserved",
                    managed.absolute(&spec.path).display()
                ));
            }
        }
        HostProtocol::Guidance if managed.regular_file_exists(&spec.path)? => {
            if let Some(content) = read_optional_string(managed, &spec.path)?
                && !content.contains(&begin_marker(request.host))
                && contains_product_reference(&content)
            {
                warnings.push(format!(
                    "another Code System Graph guidance file exists in `{}` and was preserved",
                    managed.absolute(&spec.path).display()
                ));
            }
        }
        HostProtocol::Json { .. } | HostProtocol::Guidance => {}
    }
    Ok(warnings)
}

fn read_json_object(managed: &ManagedRoot, relative: &Path) -> Result<(Value, bool), HookError> {
    if !managed.regular_file_exists(relative)? {
        return Ok((Value::Object(Map::new()), false));
    }
    let content = managed.read_utf8_bounded(relative, MAX_HOST_FILE_BYTES)?;
    let value: Value =
        serde_json::from_str(&content).map_err(|source| HookError::InvalidConfiguration {
            path: managed.absolute(relative),
            message: source.to_string(),
        })?;
    if !value.is_object() {
        return Err(HookError::InvalidConfiguration {
            path: managed.absolute(relative),
            message: "top-level JSON value must be an object".to_owned(),
        });
    }
    Ok((value, true))
}

fn object_field_mut<'a>(
    root: &'a mut Value,
    key: &str,
    managed: &ManagedRoot,
    relative: &Path,
) -> Result<&'a mut Map<String, Value>, HookError> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: managed.absolute(relative),
            message: "top-level JSON value must be an object".to_owned(),
        })?;
    let value = object
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    value
        .as_object_mut()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: managed.absolute(relative),
            message: format!("`{key}` must be an object"),
        })
}

fn array_field_mut<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    managed: &ManagedRoot,
    relative: &Path,
) -> Result<&'a mut Vec<Value>, HookError> {
    let value = object
        .entry(key.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    value
        .as_array_mut()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: managed.absolute(relative),
            message: format!("hook event `{key}` must be an array"),
        })
}

fn is_owned_json(value: &Value) -> bool {
    json_strings(value).any(|text| text.contains(PRODUCT_MARKER))
}

fn json_mentions_product(value: &Value) -> bool {
    json_strings(value).any(contains_product_reference)
}

fn json_strings(value: &Value) -> Box<dyn Iterator<Item = &str> + '_> {
    match value {
        Value::String(text) => Box::new(std::iter::once(text.as_str())),
        Value::Array(values) => Box::new(values.iter().flat_map(json_strings)),
        Value::Object(values) => Box::new(values.values().flat_map(json_strings)),
        Value::Null | Value::Bool(_) | Value::Number(_) => Box::new(std::iter::empty()),
    }
}

fn contains_product_reference(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("code-system-graph") || value.contains("code system graph")
}

fn begin_marker(host: HostKind) -> String {
    format!("# BEGIN {PRODUCT_MARKER}:{}", host.as_str())
}

fn end_marker(host: HostKind) -> String {
    format!("# END {PRODUCT_MARKER}:{}", host.as_str())
}

fn remove_marked_block(content: &str, host: HostKind) -> Option<String> {
    let begin = begin_marker(host);
    let end = end_marker(host);
    let start = content.find(&begin)?;
    let relative_end = content[start..].find(&end)?;
    let mut finish = start + relative_end + end.len();
    if content.as_bytes().get(finish) == Some(&b'\n') {
        finish += 1;
    }
    let mut updated = String::with_capacity(content.len() - (finish - start));
    updated.push_str(&content[..start]);
    updated.push_str(&content[finish..]);
    Some(updated.trim_end().to_owned() + "\n")
}

fn file_contains(managed: &ManagedRoot, relative: &Path, needle: &str) -> Result<bool, HookError> {
    Ok(read_optional_string(managed, relative)?
        .as_deref()
        .is_some_and(|content| content.contains(needle)))
}

fn read_optional_string(
    managed: &ManagedRoot,
    relative: &Path,
) -> Result<Option<String>, HookError> {
    managed.read_optional_utf8_bounded(relative, MAX_HOST_FILE_BYTES)
}

fn write_json_if_changed<T: Serialize>(
    managed: &ManagedRoot,
    relative: &Path,
    value: &T,
    backup_existing: bool,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_bytes_if_changed(managed, relative, &bytes, backup_existing, backups)
}

fn write_value(
    managed: &ManagedRoot,
    relative: &Path,
    value: &Value,
    existed: bool,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_bytes_if_changed(managed, relative, &bytes, existed, backups)
}

fn write_string(
    managed: &ManagedRoot,
    relative: &Path,
    content: &str,
    existed: bool,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    write_bytes_if_changed(managed, relative, content.as_bytes(), existed, backups)
}

fn write_bytes_if_changed(
    managed: &ManagedRoot,
    relative: &Path,
    bytes: &[u8],
    backup_existing: bool,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    if managed
        .read_optional_utf8_bounded(relative, MAX_HOST_FILE_BYTES)?
        .is_some_and(|existing| existing.as_bytes() == bytes)
    {
        return Ok(false);
    }
    if backup_existing && managed.regular_file_exists(relative)? {
        backup(managed, relative, backups)?;
    }
    managed.atomic_write(relative, bytes)?;
    Ok(true)
}

fn backup(
    managed: &ManagedRoot,
    relative: &Path,
    backups: &mut Vec<PathBuf>,
) -> Result<(), HookError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HookError::InvalidSystemTime)?;
    let file_name = relative
        .file_name()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: managed.absolute(relative),
            message: "backup source has no file name".to_owned(),
        })?
        .to_string_lossy();
    let backup_relative = relative.with_file_name(format!(
        "{file_name}.bak.code-system-graph.{}-{}",
        stamp.as_secs(),
        stamp.subsec_nanos()
    ));
    let content = managed.read_utf8_bounded(relative, MAX_HOST_FILE_BYTES)?;
    managed.atomic_write(&backup_relative, content.as_bytes())?;
    backups.push(managed.absolute(&backup_relative));
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<(), HookError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<(), HookError> {
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), HookError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .map_err(|source| HookError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    fs::set_permissions(path, permissions).map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), HookError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{guidance_block, guidance_scaffold, owned_json_entry, strict_gate_block};
    use crate::types::{HookMode, HostKind, InstallRequest};

    fn request(host: HostKind, codegraph_enabled: bool) -> InstallRequest {
        InstallRequest {
            root: PathBuf::from("/workspace/api"),
            host,
            mode: HookMode::Advisory,
            code_system_graph_binary: PathBuf::from("/bin/csgraph"),
            database: PathBuf::from("/workspace/graph.db"),
            workspace: "commerce".to_owned(),
            repository: "api".to_owned(),
            codegraph_enabled,
        }
    }

    #[test]
    fn generated_routing_should_follow_codegraph_policy() {
        let native = guidance_block(HostKind::Cursor, false);
        let enriched = guidance_block(HostKind::Cursor, true);
        assert!(!native.contains("explore"));
        assert!(enriched.contains("explore"));
        assert!(!native.contains("{{"));
        assert!(!enriched.contains("{{"));
        assert!(!guidance_scaffold(HostKind::Cursor, &native).contains("{{"));
        assert!(!strict_gate_block(&request(HostKind::Codex, false)).contains("{{"));

        let runtime = PathBuf::from("/bin/code-system-graph-hooks");
        let native_hook = owned_json_entry(&request(HostKind::Codex, false), &runtime).to_string();
        let enriched_hook = owned_json_entry(&request(HostKind::Codex, true), &runtime).to_string();
        assert!(native_hook.contains("--codegraph-enabled false"));
        assert!(enriched_hook.contains("--codegraph-enabled true"));
    }
}
