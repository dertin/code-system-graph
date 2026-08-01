use std::io;
use std::path::PathBuf;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Agent host targeted by an installation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostKind {
    /// Anthropic Claude Code.
    ClaudeCode,
    /// `OpenAI` Codex CLI.
    Codex,
    /// Google Gemini CLI.
    Gemini,
    /// Google Antigravity IDE.
    Antigravity,
    /// Cursor editor and CLI.
    Cursor,
}

impl HostKind {
    /// Returns the stable command-line and state-file identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Cursor => "cursor",
        }
    }
}

impl FromStr for HostKind {
    type Err = HookError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "claude-code" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            "gemini" => Ok(Self::Gemini),
            "antigravity" => Ok(Self::Antigravity),
            "cursor" => Ok(Self::Cursor),
            other => Err(HookError::UnknownHost(other.to_owned())),
        }
    }
}

/// Failure policy for installed integration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HookMode {
    /// Prompt routing and hook failures never block normal host operation.
    #[default]
    Advisory,
    /// Prompt routing remains advisory, while the Git pre-commit gate fails closed.
    Strict,
}

/// Complete request for installing, inspecting, or removing one host integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstallRequest {
    /// Repository worktree root in which host files are managed.
    pub root: PathBuf,
    /// Host whose documented project configuration is targeted.
    pub host: HostKind,
    /// Advisory or strict pre-commit behavior.
    #[serde(default)]
    pub mode: HookMode,
    /// `Code System Graph` executable used by the strict gate and to locate its sibling hook runtime.
    pub code_system_graph_binary: PathBuf,
    /// `Code System Graph` `SQLite` database passed to strict staged-change analysis.
    pub database: PathBuf,
    /// Registered `Code System Graph` workspace name.
    pub workspace: String,
    /// Registered repository alias for the selected root.
    pub repository: String,
    /// Whether installed guidance may recommend CodeGraph-backed local exploration.
    #[serde(default)]
    pub codegraph_enabled: bool,
}

/// Result of an installation attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstallReport {
    /// Whether any managed file changed.
    pub changed: bool,
    /// Host configuration or guidance file managed by this installation.
    pub host_file: PathBuf,
    /// Managed state file written with restrictive permissions.
    pub state_file: PathBuf,
    /// Repository-local Git ignore file when the root belongs to a Git worktree.
    pub gitignore_path: Option<PathBuf>,
    /// Whether installation added the generated-state rule.
    pub gitignore_updated: bool,
    /// Files whose previous contents were backed up.
    pub backups: Vec<PathBuf>,
    /// Non-fatal duplicate or compatibility observations.
    pub warnings: Vec<String>,
    /// Host limitation when a stable prompt-hook protocol cannot carry guidance.
    pub limitation: Option<String>,
}

/// Current state of one host integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HookStatus {
    /// Whether all files required by the requested mode are present and marker-owned.
    pub installed: bool,
    /// Whether the host routing component is installed.
    pub routing_installed: bool,
    /// Whether the strict pre-commit component is installed.
    pub strict_gate_installed: bool,
    /// Host configuration or guidance path.
    pub host_file: PathBuf,
    /// State path used for installation metadata and runtime deduplication.
    pub state_file: PathBuf,
    /// Non-fatal duplicate product-hook observations.
    pub warnings: Vec<String>,
    /// Host limitation when a guidance file is used instead of a prompt hook.
    pub limitation: Option<String>,
}

/// Result of removing one host integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UninstallReport {
    /// Whether any marker-owned content was removed.
    pub changed: bool,
    /// Backups created before merging marker-owned content out of existing files.
    pub backups: Vec<PathBuf>,
    /// Files deleted because they contained only marker-owned content.
    pub removed_files: Vec<PathBuf>,
    /// Non-fatal observations made while uninstalling.
    pub warnings: Vec<String>,
}

/// Routing category inferred only from submitted prompt text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RoutingIntent {
    /// No repository-intelligence routing signal was found.
    None,
    /// Work is local to one repository.
    LocalRepository,
    /// Work spans repositories, contracts, architecture, impact, diffs, or pull requests.
    Federated,
}

/// Input to the host-independent prompt router.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingRequest {
    /// Host whose documented output shape should be generated.
    pub host: HostKind,
    /// Repository root used only as a deduplication scope.
    pub root: PathBuf,
    /// Raw host event. Only the top-level `prompt` and session identifier are read.
    pub event: serde_json::Value,
    /// Whether guidance may recommend CodeGraph-backed local exploration.
    #[serde(default)]
    pub codegraph_enabled: bool,
    /// Session/repository deduplication lifetime in seconds.
    #[serde(default = "default_ttl_seconds")]
    pub ttl_seconds: u64,
}

/// Host-independent routing result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingResponse {
    /// Classified prompt intent.
    pub intent: RoutingIntent,
    /// Static guidance, omitted when no routing signal exists or TTL deduplication suppresses it.
    pub guidance: Option<String>,
    /// Whether a prior session/repository decision suppressed duplicate guidance.
    pub deduplicated: bool,
}

/// Error returned by host installation and routing APIs.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HookError {
    /// Filesystem operation failed.
    #[error("filesystem operation failed for `{path}`: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Host JSON configuration was malformed or had an incompatible shape.
    #[error("invalid host configuration `{path}`: {message}")]
    InvalidConfiguration {
        /// Affected host file.
        path: PathBuf,
        /// Validation detail.
        message: String,
    },
    /// Serialization failed.
    #[error("JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    /// The repository root has no final component from which to derive an alias.
    #[error("repository root `{0}` has no final path component")]
    MissingRepositoryAlias(PathBuf),
    /// System time is earlier than the Unix epoch.
    #[error("system clock is earlier than the Unix epoch")]
    InvalidSystemTime,
    /// A command-line host identifier is unknown.
    #[error("unknown host `{0}`")]
    UnknownHost(String),
    /// A routing event omitted a textual top-level prompt.
    #[error("hook event does not contain a top-level string `prompt`")]
    MissingPrompt,
}

pub(crate) const fn default_ttl_seconds() -> u64 {
    300
}
