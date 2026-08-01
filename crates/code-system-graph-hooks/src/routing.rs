use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};

use crate::types::{HookError, RoutingIntent, RoutingRequest, RoutingResponse};

const FEDERATED_CODEGRAPH_GUIDANCE: &str = "Use Code System Graph first for federated contracts, architecture, impact, diff, or PR-overlap context; use explore for repository-local source and symbol detail.";
const LOCAL_CODEGRAPH_GUIDANCE: &str = "Use Code System Graph explore first for repository-local symbols, callers, tests, and implementation detail; use CodeGraph directly only if the provider is degraded.";
const FEDERATED_NATIVE_GUIDANCE: &str = "Use Code System Graph first for federated contracts, architecture, impact, diff, or PR-overlap context. Repository-local source and symbol detail is unavailable in the native-only profile.";
const LOCAL_NATIVE_GUIDANCE: &str = "Use Code System Graph for persisted repository entities, relationships, and source-free evidence. Repository-local source and symbol detail is unavailable in the native-only profile.";

const FEDERATED_SIGNALS: &[&str] = &[
    "cross-repo",
    "cross repo",
    "multiple repos",
    "across repos",
    "contract",
    "api boundary",
    "architecture",
    "architectural",
    "impact",
    "blast radius",
    "diff",
    "pull request",
    "pull-request",
    "pr overlap",
    "overlapping pr",
    "dependency graph",
    "service boundary",
    "federated",
];

const LOCAL_SIGNALS: &[&str] = &[
    "repository",
    "repo",
    "code",
    "symbol",
    "function",
    "method",
    "class",
    "module",
    "file",
    "test",
    "bug",
    "refactor",
    "implement",
    "caller",
    "call site",
];

#[derive(Debug, Default, Serialize, Deserialize)]
struct DedupState {
    entries: BTreeMap<String, u64>,
}

/// Classifies a prompt without reading source, running tools, or retaining prompt text.
#[must_use]
pub fn classify_prompt(prompt: &str) -> RoutingIntent {
    let normalized = prompt.to_lowercase();
    if FEDERATED_SIGNALS
        .iter()
        .any(|signal| normalized.contains(signal))
    {
        RoutingIntent::Federated
    } else if LOCAL_SIGNALS
        .iter()
        .any(|signal| normalized.contains(signal))
    {
        RoutingIntent::LocalRepository
    } else {
        RoutingIntent::None
    }
}

/// Classifies one host event and applies session/repository TTL deduplication.
///
/// Only the top-level `prompt` and optional `session_id` fields are read. Prompt text is never
/// written to disk or included in the returned guidance.
///
/// # Errors
///
/// Returns [`HookError`] when the event has no textual prompt, the clock is invalid, or the
/// deduplication state cannot be read or atomically written.
pub fn route(request: &RoutingRequest) -> Result<RoutingResponse, HookError> {
    let prompt = request
        .event
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .ok_or(HookError::MissingPrompt)?;
    let intent = classify_prompt(prompt);
    let guidance = guidance_for(intent, request.codegraph_enabled);
    let Some(guidance) = guidance else {
        return Ok(RoutingResponse {
            intent,
            guidance: None,
            deduplicated: false,
        });
    };

    let now = unix_seconds()?;
    let state_path = dedup_state_path(request);
    let mut state = load_state(&state_path)?;
    state.entries.retain(|_, expires_at| *expires_at > now);
    let session = request
        .event
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("session-unavailable");
    let key = dedup_key(request, session);
    if state.entries.contains_key(&key) {
        return Ok(RoutingResponse {
            intent,
            guidance: None,
            deduplicated: true,
        });
    }

    state
        .entries
        .insert(key, now.saturating_add(request.ttl_seconds));
    write_state(&state_path, &state)?;
    Ok(RoutingResponse {
        intent,
        guidance: Some(guidance.to_owned()),
        deduplicated: false,
    })
}

fn guidance_for(intent: RoutingIntent, codegraph_enabled: bool) -> Option<&'static str> {
    match (intent, codegraph_enabled) {
        (RoutingIntent::None, _) => None,
        (RoutingIntent::LocalRepository, true) => Some(LOCAL_CODEGRAPH_GUIDANCE),
        (RoutingIntent::Federated, true) => Some(FEDERATED_CODEGRAPH_GUIDANCE),
        (RoutingIntent::LocalRepository, false) => Some(LOCAL_NATIVE_GUIDANCE),
        (RoutingIntent::Federated, false) => Some(FEDERATED_NATIVE_GUIDANCE),
    }
}

fn dedup_state_path(request: &RoutingRequest) -> std::path::PathBuf {
    request
        .root
        .join(".code-system-graph")
        .join("hooks")
        .join(format!("dedup-{}.json", request.host.as_str()))
}

fn dedup_key(request: &RoutingRequest, session: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(request.host.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(request.root.as_os_str().as_encoded_bytes());
    hasher.update(&[0]);
    hasher.update(session.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn load_state(path: &Path) -> Result<DedupState, HookError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(HookError::from),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(DedupState::default()),
        Err(source) => Err(HookError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_state(path: &Path, state: &DedupState) -> Result<(), HookError> {
    let parent = path
        .parent()
        .ok_or_else(|| HookError::InvalidConfiguration {
            path: path.to_path_buf(),
            message: "state path has no parent".to_owned(),
        })?;
    fs::create_dir_all(parent).map_err(|source| HookError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec(state)?;
    let mut destination = AtomicWriteFile::open(path).map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    destination
        .write_all(&bytes)
        .and_then(|()| destination.sync_all())
        .map_err(|source| HookError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    destination.commit().map_err(|source| HookError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    restrict_file(path)
}

fn unix_seconds() -> Result<u64, HookError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| HookError::InvalidSystemTime)
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

#[cfg(test)]
mod tests {
    use super::{guidance_for, RoutingIntent};

    #[test]
    fn guidance_should_follow_codegraph_policy() {
        for intent in [RoutingIntent::LocalRepository, RoutingIntent::Federated] {
            let native = guidance_for(intent, false).expect("native guidance");
            let enriched = guidance_for(intent, true).expect("CodeGraph guidance");

            assert!(!native.contains("explore"));
            assert!(enriched.contains("explore"));
        }
    }
}
