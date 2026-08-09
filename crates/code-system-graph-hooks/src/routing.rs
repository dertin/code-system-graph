use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};

use crate::templates::{
    FEDERATED_CODEGRAPH_GUIDANCE, FEDERATED_NATIVE_GUIDANCE, FEDERATED_SIGNALS, LOCAL_CODEGRAPH_GUIDANCE, LOCAL_NATIVE_GUIDANCE, LOCAL_SIGNALS
};
use crate::types::{HookError, RoutingIntent, RoutingRequest, RoutingResponse};

#[derive(Debug, Default, Serialize, Deserialize)]
struct DedupState {
    entries: BTreeMap<String, u64>,
}

/// Classifies a prompt without reading source, running tools, or retaining prompt text.
#[must_use]
pub fn classify_prompt(prompt: &str) -> RoutingIntent {
    let normalized = prompt.to_lowercase();
    if contains_signal(FEDERATED_SIGNALS, &normalized) {
        RoutingIntent::Federated
    } else if contains_signal(LOCAL_SIGNALS, &normalized) {
        RoutingIntent::LocalRepository
    } else {
        RoutingIntent::None
    }
}

fn contains_signal(signals: &str, normalized_prompt: &str) -> bool {
    signals
        .lines()
        .map(str::trim)
        .filter(|signal| !signal.is_empty())
        .any(|signal| normalized_prompt.contains(signal))
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
        (RoutingIntent::LocalRepository, true) => Some(LOCAL_CODEGRAPH_GUIDANCE.trim_end()),
        (RoutingIntent::Federated, true) => Some(FEDERATED_CODEGRAPH_GUIDANCE.trim_end()),
        (RoutingIntent::LocalRepository, false) => Some(LOCAL_NATIVE_GUIDANCE.trim_end()),
        (RoutingIntent::Federated, false) => Some(FEDERATED_NATIVE_GUIDANCE.trim_end()),
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
    use super::{RoutingIntent, guidance_for};

    #[test]
    fn guidance_should_follow_codegraph_policy() {
        for intent in [RoutingIntent::LocalRepository, RoutingIntent::Federated] {
            let native = guidance_for(intent, false).expect("native guidance");
            let enriched = guidance_for(intent, true).expect("CodeGraph guidance");

            assert!(!native.contains("explore"));
            assert!(enriched.contains("explore"));
            assert!(native.contains("Follow the installed Code System Graph skill"));
            assert!(enriched.contains("Follow the installed Code System Graph skill"));
            assert!(!native.contains("routing path"));
            assert!(!enriched.contains("routing path"));
        }
    }
}
