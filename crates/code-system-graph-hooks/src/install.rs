use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::types::{
    HookError, HookMode, HookStatus, HostKind, InstallReport, InstallRequest, UninstallReport
};

const PRODUCT_MARKER: &str = "code-system-graph-hooks:v1";
const STATE_DIRECTORY: &str = ".code-system-graph/hooks";
const GENERATED_STATE_IGNORE_RULE: &[u8] = b".code-system-graph/";
const CURSOR_LIMITATION: &str = "Cursor beforeSubmitPrompt can allow or block but cannot inject advisory routing context; installed an always-on project rule instead.";
const ANTIGRAVITY_LIMITATION: &str = "Antigravity IDE does not document a stable project-file prompt hook protocol; installed a workspace rule instead.";

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
    let spec = host_spec(request);
    let state_file = state_file(request);
    let mut backups = Vec::new();
    let mut warnings = duplicate_warnings(request, &spec)?;
    let runtime = request.code_system_graph_binary.with_file_name(format!(
        "code-system-graph-hooks{}",
        std::env::consts::EXE_SUFFIX
    ));

    let routing_changed = match spec.protocol {
        HostProtocol::Json { event } => {
            install_json_hook(request, &spec.path, event, &runtime, &mut backups)?
        }
        HostProtocol::Guidance => install_guidance(request, &spec.path, &mut backups)?,
    };
    let strict_changed = if request.mode == HookMode::Strict {
        install_strict_gate(request, &mut backups, &mut warnings)?
    } else {
        remove_strict_gate(request, &mut backups, &mut Vec::new())?
    };
    let state = InstallState {
        marker: PRODUCT_MARKER.to_owned(),
        host: request.host,
        mode: request.mode,
        host_file: spec.path.clone(),
        codegraph_enabled: request.codegraph_enabled,
    };
    let state_changed = write_json_if_changed(&state_file, &state, false, &mut backups)?;
    restrict_file(&state_file)?;
    let (gitignore_path, gitignore_updated) = configure_generated_state_ignore(&request.root)?;

    Ok(InstallReport {
        changed: routing_changed || strict_changed || state_changed || gitignore_updated,
        host_file: spec.path,
        state_file,
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
    let spec = host_spec(request);
    let state_path = state_file(request);
    let installed_state = read_install_state(&state_path)?;
    let policy = installed_state
        .as_ref()
        .map(|state| state.codegraph_enabled)
        .unwrap_or(request.codegraph_enabled);
    let runtime = request.code_system_graph_binary.with_file_name(format!(
        "code-system-graph-hooks{}",
        std::env::consts::EXE_SUFFIX
    ));
    let routing_installed = match spec.protocol {
        HostProtocol::Json { event } => {
            json_hook_installed(request, &spec.path, event, &runtime, policy)?
        }
        HostProtocol::Guidance => file_contains(&spec.path, &guidance_block(request.host, policy))?,
    };
    let strict_gate_installed = if request.mode == HookMode::Strict {
        file_contains(&git_pre_commit(request)?, &begin_marker(request.host))?
    } else if let Some(path) = optional_git_pre_commit(request)? {
        file_contains(&path, &begin_marker(request.host))?
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
    let warnings = duplicate_warnings(request, &spec)?;

    Ok(HookStatus {
        installed,
        routing_installed,
        strict_gate_installed,
        host_file: spec.path,
        state_file: state_path,
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
    let spec = host_spec(request);
    let mut backups = Vec::new();
    let mut removed_files = Vec::new();
    let mut warnings = Vec::new();

    let routing_changed = match spec.protocol {
        HostProtocol::Json { event } => uninstall_json_hook(&spec.path, event, &mut backups)?,
        HostProtocol::Guidance => {
            remove_guidance(request, &spec.path, &mut backups, &mut removed_files)?
        }
    };
    let strict_changed = remove_strict_gate(request, &mut backups, &mut removed_files)?;
    let state = state_file(request);
    let state_changed = if state.exists() {
        fs::remove_file(&state).map_err(|source| HookError::Io {
            path: state.clone(),
            source,
        })?;
        removed_files.push(state);
        true
    } else {
        false
    };

    warnings.extend(duplicate_warnings(request, &spec)?);
    Ok(UninstallReport {
        changed: routing_changed || strict_changed || state_changed,
        backups,
        removed_files,
        warnings,
    })
}

fn validate_request(request: &InstallRequest) -> Result<(), HookError> {
    let metadata = fs::metadata(&request.root).map_err(|source| HookError::Io {
        path: request.root.clone(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(HookError::InvalidConfiguration {
            path: request.root.clone(),
            message: "repository root is not a directory".to_owned(),
        });
    }
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
            Some(ANTIGRAVITY_LIMITATION),
        ),
        HostKind::Cursor => (
            ".cursor/rules/code-system-graph-routing.mdc",
            HostProtocol::Guidance,
            Some(CURSOR_LIMITATION),
        ),
    };
    HostSpec {
        path: request.root.join(relative),
        protocol,
        limitation,
    }
}

fn state_file(request: &InstallRequest) -> PathBuf {
    request
        .root
        .join(STATE_DIRECTORY)
        .join(format!("install-{}.json", request.host.as_str()))
}

fn read_install_state(path: &Path) -> Result<Option<InstallState>, HookError> {
    if !path.is_file() {
        return Ok(None);
    }
    let content = fs::read_to_string(path).map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let state =
        serde_json::from_str(&content).map_err(|error| HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    Ok(Some(state))
}

fn configure_generated_state_ignore(root: &Path) -> Result<(Option<PathBuf>, bool), HookError> {
    let canonical_root = fs::canonicalize(root).map_err(|source| HookError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    if !belongs_to_git_worktree(&canonical_root) {
        return Ok((None, false));
    }
    let (path, updated) = ensure_generated_state_ignored(root)?;
    Ok((Some(path), updated))
}

fn belongs_to_git_worktree(root: &Path) -> bool {
    root.ancestors()
        .any(|ancestor| ancestor.join(".git").exists())
}

fn ensure_generated_state_ignored(root: &Path) -> Result<(PathBuf, bool), HookError> {
    let path = root.join(".gitignore");
    let mut content = match fs::read(&path) {
        Ok(content) => content,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => {
            return Err(HookError::Io { path, source });
        }
    };
    if generated_state_is_ignored(&content) {
        return Ok((path, false));
    }
    if !content.is_empty() && !content.ends_with(b"\n") {
        content.push(b'\n');
    }
    content.extend_from_slice(GENERATED_STATE_IGNORE_RULE);
    content.push(b'\n');
    atomic_write(&path, &content)?;
    Ok((path, true))
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
    request: &InstallRequest,
    path: &Path,
    event: &str,
    runtime: &Path,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let (mut root, existed) = read_json_object(path)?;
    let hooks = object_field_mut(&mut root, "hooks", path)?;
    let entries = array_field_mut(hooks, event, path)?;
    let owned = owned_json_entry(request, runtime);
    if entries.iter().any(is_owned_json) {
        if entries.iter().any(|entry| entry == &owned) {
            return Ok(false);
        }
        entries.retain(|entry| !is_owned_json(entry));
    }
    entries.push(owned);
    write_value(path, &root, existed, backups)
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
                "statusMessage": "Selecting repository intelligence"
            }]
        }),
        HostKind::Gemini => json!({
            "matcher": "*",
            "hooks": [{
                "name": PRODUCT_MARKER,
                "type": "command",
                "command": command,
                "timeout": 5000,
                "description": "Select Code System Graph or CodeGraph from prompt intent"
            }]
        }),
        HostKind::Antigravity | HostKind::Cursor => Value::Null,
    }
}

fn uninstall_json_hook(
    path: &Path,
    event: &str,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    if !path.exists() {
        return Ok(false);
    }
    let (mut root, _) = read_json_object(path)?;
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
    write_value(path, &root, true, backups)
}

fn json_hook_installed(
    request: &InstallRequest,
    path: &Path,
    event: &str,
    runtime: &Path,
    policy: bool,
) -> Result<bool, HookError> {
    if !path.exists() {
        return Ok(false);
    }
    let owned = owned_json_entry_with_policy(request, runtime, policy);
    let (root, _) = read_json_object(path)?;
    Ok(root
        .get("hooks")
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.iter().any(|entry| entry == &owned)))
}

fn install_guidance(
    request: &InstallRequest,
    path: &Path,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let existing = read_optional_string(path)?;
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
                    path: path.to_path_buf(),
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
    write_string(path, &updated, path.exists(), backups)
}

fn guidance_scaffold(host: HostKind, block: &str) -> String {
    if host == HostKind::Cursor {
        format!(
            "---\ndescription: Code System Graph routing guidance ({PRODUCT_MARKER})\nalwaysApply: true\n---\n\n{block}"
        )
    } else {
        block.to_owned()
    }
}

fn guidance_block(host: HostKind, codegraph_enabled: bool) -> String {
    let routing = if codegraph_enabled {
        "- For work local to this repository, use Code System Graph explore first and use CodeGraph directly only if the provider is degraded.\n- For cross-repository work, contracts, architecture, impact, diffs, or pull-request overlap, use Code System Graph first and explore for local symbol detail."
    } else {
        "- For repository-local work, use Code System Graph only for persisted entities, relationships, and source-free evidence; local source and symbol detail is unavailable in the native-only profile.\n- For cross-repository work, contracts, architecture, impact, diffs, or pull-request overlap, use Code System Graph first."
    };
    format!(
        "{begin}\n# Code System Graph intelligence routing\n\nClassify only the user's submitted prompt. Do not quote, copy, or inject the prompt itself.\n\n{routing}\n- Never automatically run scans, CodeGraph init or sync, source queries, or mutations because of this rule.\n- Keep routing guidance brief and advisory.\n{end}\n",
        begin = begin_marker(host),
        end = end_marker(host)
    )
}

fn remove_guidance(
    request: &InstallRequest,
    path: &Path,
    backups: &mut Vec<PathBuf>,
    removed_files: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let Some(content) = read_optional_string(path)? else {
        return Ok(false);
    };
    let Some(updated) = remove_marked_block(&content, request.host) else {
        return Ok(false);
    };
    let generated_cursor_scaffold = request.host == HostKind::Cursor
        && updated.trim()
            == format!(
                "---\ndescription: Code System Graph routing guidance ({PRODUCT_MARKER})\nalwaysApply: true\n---"
            );
    backup(path, backups)?;
    if updated.trim().is_empty() || generated_cursor_scaffold {
        fs::remove_file(path).map_err(|source| HookError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        removed_files.push(path.to_path_buf());
    } else {
        atomic_write(path, updated.as_bytes())?;
    }
    Ok(true)
}

fn install_strict_gate(
    request: &InstallRequest,
    backups: &mut Vec<PathBuf>,
    warnings: &mut Vec<String>,
) -> Result<bool, HookError> {
    let path = git_pre_commit(request)?;
    let existing = read_optional_string(&path)?;
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
            path.display()
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
    let changed = write_string(&path, &updated, path.exists(), backups)?;
    if changed {
        make_executable(&path)?;
    }
    Ok(changed)
}

fn strict_gate_block(request: &InstallRequest) -> String {
    format!(
        "{begin}\nCODE_SYSTEM_GRAPH_RESULT=\"$({binary} changes --scope staged --database {database} --workspace {workspace} --repository {repository})\" || {{\n  echo \"Code System Graph staged-change analysis failed; commit blocked by strict mode.\" >&2\n  exit 1\n}}\nCODE_SYSTEM_GRAPH_FINGERPRINT=\"$(printf '%s' \"$CODE_SYSTEM_GRAPH_RESULT\" | tr -d '\\n' | sed -n 's/.*\"exact_diff_fingerprint\":\"\\([^\"]*\\)\".*/\\1/p')\"\nif [ -z \"$CODE_SYSTEM_GRAPH_FINGERPRINT\" ]; then\n  echo \"Code System Graph returned no exact staged fingerprint; commit blocked by strict mode.\" >&2\n  exit 1\nfi\nunset CODE_SYSTEM_GRAPH_RESULT CODE_SYSTEM_GRAPH_FINGERPRINT\n{end}\n",
        begin = begin_marker(request.host),
        binary = shell_quote(
            request
                .code_system_graph_binary
                .as_os_str()
                .to_string_lossy()
                .as_ref()
        ),
        database = shell_quote(request.database.as_os_str().to_string_lossy().as_ref()),
        workspace = shell_quote(&request.workspace),
        repository = shell_quote(&request.repository),
        end = end_marker(request.host)
    )
}

fn remove_strict_gate(
    request: &InstallRequest,
    backups: &mut Vec<PathBuf>,
    removed_files: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let Some(path) = optional_git_pre_commit(request)? else {
        return Ok(false);
    };
    let Some(content) = read_optional_string(&path)? else {
        return Ok(false);
    };
    let Some(updated) = remove_marked_block(&content, request.host) else {
        return Ok(false);
    };
    backup(&path, backups)?;
    if updated.trim() == "#!/bin/sh" || updated.trim().is_empty() {
        fs::remove_file(&path).map_err(|source| HookError::Io {
            path: path.clone(),
            source,
        })?;
        removed_files.push(path);
    } else {
        atomic_write(&path, updated.as_bytes())?;
        make_executable(&path)?;
    }
    Ok(true)
}

fn git_pre_commit(request: &InstallRequest) -> Result<PathBuf, HookError> {
    optional_git_pre_commit(request)?.ok_or_else(|| HookError::InvalidConfiguration {
        path: request.root.clone(),
        message: "strict mode requires a Git repository worktree".to_owned(),
    })
}

fn optional_git_pre_commit(request: &InstallRequest) -> Result<Option<PathBuf>, HookError> {
    let dot_git = request.root.join(".git");
    if dot_git.is_dir() {
        return Ok(Some(dot_git.join("hooks/pre-commit")));
    }
    if !dot_git.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&dot_git).map_err(|source| HookError::Io {
        path: dot_git.clone(),
        source,
    })?;
    let relative = content
        .trim()
        .strip_prefix("gitdir:")
        .map(str::trim)
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: dot_git.clone(),
            message: "expected a Git directory or `gitdir:` pointer".to_owned(),
        })?;
    let git_dir = request.root.join(relative);
    Ok(Some(git_dir.join("hooks/pre-commit")))
}

fn duplicate_warnings(request: &InstallRequest, spec: &HostSpec) -> Result<Vec<String>, HookError> {
    let mut warnings = Vec::new();
    match spec.protocol {
        HostProtocol::Json { event } if spec.path.exists() => {
            let (root, _) = read_json_object(&spec.path)?;
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
                    spec.path.display()
                ));
            }
        }
        HostProtocol::Guidance if spec.path.exists() => {
            if let Some(content) = read_optional_string(&spec.path)?
                && !content.contains(&begin_marker(request.host))
                && contains_product_reference(&content)
            {
                warnings.push(format!(
                    "another Code System Graph guidance file exists in `{}` and was preserved",
                    spec.path.display()
                ));
            }
        }
        HostProtocol::Json { .. } | HostProtocol::Guidance => {}
    }
    Ok(warnings)
}

fn read_json_object(path: &Path) -> Result<(Value, bool), HookError> {
    if !path.exists() {
        return Ok((Value::Object(Map::new()), false));
    }
    let bytes = fs::read(path).map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|source| HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
    if !value.is_object() {
        return Err(HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: "top-level JSON value must be an object".to_owned(),
        });
    }
    Ok((value, true))
}

fn object_field_mut<'a>(
    root: &'a mut Value,
    key: &str,
    path: &Path,
) -> Result<&'a mut Map<String, Value>, HookError> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: "top-level JSON value must be an object".to_owned(),
        })?;
    let value = object
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    value
        .as_object_mut()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: format!("`{key}` must be an object"),
        })
}

fn array_field_mut<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    path: &Path,
) -> Result<&'a mut Vec<Value>, HookError> {
    let value = object
        .entry(key.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    value
        .as_array_mut()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: path.to_path_buf(),
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

fn file_contains(path: &Path, needle: &str) -> Result<bool, HookError> {
    Ok(read_optional_string(path)?
        .as_deref()
        .is_some_and(|content| content.contains(needle)))
}

fn read_optional_string(path: &Path) -> Result<Option<String>, HookError> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(HookError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_json_if_changed<T: Serialize>(
    path: &Path,
    value: &T,
    backup_existing: bool,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_bytes_if_changed(path, &bytes, backup_existing, backups)
}

fn write_value(
    path: &Path,
    value: &Value,
    existed: bool,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_bytes_if_changed(path, &bytes, existed, backups)
}

fn write_string(
    path: &Path,
    content: &str,
    existed: bool,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    write_bytes_if_changed(path, content.as_bytes(), existed, backups)
}

fn write_bytes_if_changed(
    path: &Path,
    bytes: &[u8],
    backup_existing: bool,
    backups: &mut Vec<PathBuf>,
) -> Result<bool, HookError> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    if backup_existing && path.exists() {
        backup(path, backups)?;
    }
    atomic_write(path, bytes)?;
    Ok(true)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), HookError> {
    let parent = path
        .parent()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: "managed file has no parent".to_owned(),
        })?;
    fs::create_dir_all(parent).map_err(|source| HookError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut destination = AtomicWriteFile::open(path).map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    destination
        .write_all(bytes)
        .and_then(|()| destination.sync_all())
        .map_err(|source| HookError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    destination.commit().map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn backup(path: &Path, backups: &mut Vec<PathBuf>) -> Result<(), HookError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HookError::InvalidSystemTime)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: "backup source has no file name".to_owned(),
        })?
        .to_string_lossy();
    let backup_path = path.with_file_name(format!(
        "{file_name}.bak.code-system-graph.{}-{}",
        stamp.as_secs(),
        stamp.subsec_nanos()
    ));
    fs::copy(path, &backup_path).map_err(|source| HookError::Io {
        path: backup_path.clone(),
        source,
    })?;
    backups.push(backup_path);
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

    use super::{guidance_block, owned_json_entry};
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

        let runtime = PathBuf::from("/bin/code-system-graph-hooks");
        let native_hook = owned_json_entry(&request(HostKind::Codex, false), &runtime).to_string();
        let enriched_hook = owned_json_entry(&request(HostKind::Codex, true), &runtime).to_string();
        assert!(native_hook.contains("--codegraph-enabled false"));
        assert!(enriched_hook.contains("--codegraph-enabled true"));
    }
}
