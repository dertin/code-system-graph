use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use code_system_graph_model::stable_id;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod tracker;

pub use tracker::{MonotonicClock, ScanJobTracker};

/// Default maximum wall time for one supervised scan or sync pass.
pub const DEFAULT_MAX_SCAN_WALL_TIME_MS: u64 = 21_600_000;
/// Default maximum time without verified forward progress.
pub const DEFAULT_MAX_NO_PROGRESS_TIME_MS: u64 = 300_000;
/// Default maximum wall time for one repository-local `CodeGraph` synchronization.
pub const DEFAULT_MAX_CODEGRAPH_SYNC_WALL_TIME_MS_PER_REPO: u64 = 3_600_000;
/// Default maximum source-symbol anchors corroborated through `CodeGraph` per repository.
pub const DEFAULT_MAX_CODEGRAPH_CORROBORATION_ANCHORS_PER_REPO: i64 = 50;
/// Default maximum resident memory accepted for one worker process.
pub const DEFAULT_MAX_WORKER_MEMORY_BYTES: u64 = 17_179_869_184;
/// Default cooperative shutdown grace period before forced termination.
pub const DEFAULT_GRACEFUL_TERMINATION_MS: u64 = 5_000;
/// Default inactivity lease for a foreground watch session.
pub const DEFAULT_WATCH_IDLE_TIMEOUT_MS: u64 = 28_800_000;
/// Default absolute lifetime for one foreground watch session.
pub const DEFAULT_MAX_WATCH_SESSION_WALL_TIME_MS: u64 = 86_400_000;
/// Default minimum delay between watched sync pass starts.
pub const DEFAULT_MIN_WATCH_RESCAN_INTERVAL_MS: u64 = 10_000;
/// Default maximum retained historical checkpoint-cache bytes.
pub const DEFAULT_MAX_CHECKPOINT_CACHE_BYTES: u64 = 10_737_418_240;
pub const DEFAULT_MAX_EXPLORE_WALL_TIME_MS: u64 = 8_000;
pub const DEFAULT_MAX_EXPLORE_CODEGRAPH_OPERATIONS: u64 = 8;
pub const DEFAULT_MAX_EXPLORE_CONCURRENT_CODEGRAPH_PROCESSES: u64 = 2;
pub const DEFAULT_MAX_EXPLORE_SOURCE_FILES: u64 = 25;
pub const DEFAULT_MAX_EXPLORE_RESOLVED_SYMBOLS: u64 = 5;
pub const DEFAULT_MAX_EXPLORE_ANCHORS: u64 = 3;
pub const DEFAULT_MAX_EXPLORE_NEIGHBORS_PER_DIRECTION: u64 = 8;
pub const DEFAULT_MAX_EXPLORE_LOCAL_RELATIONSHIPS: u64 = 48;
pub const DEFAULT_MAX_EXPLORE_FEDERATED_HANDOFFS_PER_ANCHOR: u64 = 10;
pub const DEFAULT_MAX_EXPLORE_FEDERATED_HANDOFFS: u64 = 30;
pub const DEFAULT_MAX_EXPLORE_EVIDENCE_LOCATIONS_PER_HANDOFF: u64 = 4;
pub const DEFAULT_MAX_EXPLORE_SOURCE_MARKDOWN_BYTES: u64 = 262_144;
pub const DEFAULT_MAX_EXPLORE_ENRICHMENT_BYTES: u64 = 65_536;
pub const DEFAULT_MAX_AGENT_NEXT_ACTIONS_PER_RESPONSE: u64 = 12;
pub const DEFAULT_MAX_QUERY_REPOSITORY_SUGGESTIONS: u64 = 20;
pub const DEFAULT_MAX_MCP_TOOL_RESPONSE_BYTES: u64 = 524_288;
pub const DEFAULT_MAX_MCP_RESOURCE_ITEMS: u64 = 100;
pub const DEFAULT_MAX_MCP_RESOURCE_BYTES: u64 = 262_144;
pub const DEFAULT_MAX_MCP_SCHEMA_CATALOG_BYTES: u64 = 2_097_152;
/// Smallest Markdown response budget that can retain the mandatory MCP control block.
pub const MIN_MCP_MARKDOWN_BYTES: u64 = 256;

// Keep the agent-facing policy inventory in one declarative list. The two public serde structs
// intentionally remain flat for schema-v2 compatibility; defaults, override resolution, and the
// delivery fingerprint are generated from this list so a new field cannot silently omit one of
// those behaviors.
macro_rules! with_agent_policy_fields {
    ($consumer:ident) => {
        $consumer! {
            max_explore_wall_time_ms: DEFAULT_MAX_EXPLORE_WALL_TIME_MS => "maxExploreWallTimeMs";
            max_explore_codegraph_operations: DEFAULT_MAX_EXPLORE_CODEGRAPH_OPERATIONS => "maxExploreCodeGraphOperations";
            max_explore_concurrent_codegraph_processes: DEFAULT_MAX_EXPLORE_CONCURRENT_CODEGRAPH_PROCESSES => "maxExploreConcurrentCodeGraphProcesses";
            max_explore_source_files: DEFAULT_MAX_EXPLORE_SOURCE_FILES => "maxExploreSourceFiles";
            max_explore_resolved_symbols: DEFAULT_MAX_EXPLORE_RESOLVED_SYMBOLS => "maxExploreResolvedSymbols";
            max_explore_anchors: DEFAULT_MAX_EXPLORE_ANCHORS => "maxExploreAnchors";
            max_explore_neighbors_per_direction: DEFAULT_MAX_EXPLORE_NEIGHBORS_PER_DIRECTION => "maxExploreNeighborsPerDirection";
            max_explore_local_relationships: DEFAULT_MAX_EXPLORE_LOCAL_RELATIONSHIPS => "maxExploreLocalRelationships";
            max_explore_federated_handoffs_per_anchor: DEFAULT_MAX_EXPLORE_FEDERATED_HANDOFFS_PER_ANCHOR => "maxExploreFederatedHandoffsPerAnchor";
            max_explore_federated_handoffs: DEFAULT_MAX_EXPLORE_FEDERATED_HANDOFFS => "maxExploreFederatedHandoffs";
            max_explore_evidence_locations_per_handoff: DEFAULT_MAX_EXPLORE_EVIDENCE_LOCATIONS_PER_HANDOFF => "maxExploreEvidenceLocationsPerHandoff";
            max_explore_source_markdown_bytes: DEFAULT_MAX_EXPLORE_SOURCE_MARKDOWN_BYTES => "maxExploreSourceMarkdownBytes";
            max_explore_enrichment_bytes: DEFAULT_MAX_EXPLORE_ENRICHMENT_BYTES => "maxExploreEnrichmentBytes";
            max_agent_next_actions_per_response: DEFAULT_MAX_AGENT_NEXT_ACTIONS_PER_RESPONSE => "maxAgentNextActionsPerResponse";
            max_query_repository_suggestions: DEFAULT_MAX_QUERY_REPOSITORY_SUGGESTIONS => "maxQueryRepositorySuggestions";
            max_mcp_tool_response_bytes: DEFAULT_MAX_MCP_TOOL_RESPONSE_BYTES => "maxMcpToolResponseBytes";
            max_mcp_resource_items: DEFAULT_MAX_MCP_RESOURCE_ITEMS => "maxMcpResourceItems";
            max_mcp_resource_bytes: DEFAULT_MAX_MCP_RESOURCE_BYTES => "maxMcpResourceBytes";
            max_mcp_schema_catalog_bytes: DEFAULT_MAX_MCP_SCHEMA_CATALOG_BYTES => "maxMcpSchemaCatalogBytes";
        }
    };
}

/// Effective per-repository limit for source-symbol corroboration through `CodeGraph`.
///
/// The workspace manifest keeps `-1` as its portable unlimited sentinel, but resolved policy and
/// consumers use this type so that sentinel handling does not leak beyond the serde boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
pub enum CodeGraphCorroborationAnchorLimit {
    /// Corroborate at most this many source-symbol anchors per repository.
    Bounded(NonZeroUsize),
    /// Do not apply a count limit.
    Unlimited,
}

impl CodeGraphCorroborationAnchorLimit {
    /// Returns the bounded limit, or `None` when corroboration is unlimited.
    #[must_use]
    pub const fn bounded(self) -> Option<NonZeroUsize> {
        match self {
            Self::Bounded(limit) => Some(limit),
            Self::Unlimited => None,
        }
    }
}

impl TryFrom<i64> for CodeGraphCorroborationAnchorLimit {
    type Error = InvalidExecutionPolicy;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value == -1 {
            return Ok(Self::Unlimited);
        }
        let limit = usize::try_from(value)
            .ok()
            .and_then(NonZeroUsize::new)
            .ok_or(InvalidExecutionPolicy::InvalidValue {
                field: "maxCodeGraphCorroborationAnchorsPerRepo",
                value: value.unsigned_abs(),
            })?;
        Ok(Self::Bounded(limit))
    }
}

impl Serialize for CodeGraphCorroborationAnchorLimit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = match self {
            Self::Bounded(limit) => i64::try_from(limit.get()).map_err(|_| {
                serde::ser::Error::custom("CodeGraph corroboration anchor limit exceeds i64")
            })?,
            Self::Unlimited => -1,
        };
        serializer.serialize_i64(value)
    }
}

impl<'de> Deserialize<'de> for CodeGraphCorroborationAnchorLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        i64::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for CodeGraphCorroborationAnchorLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bounded(limit) => limit.fmt(formatter),
            Self::Unlimited => formatter.write_str("-1"),
        }
    }
}

/// Optional operator-owned execution-policy overrides from the workspace manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPolicyOverrides {
    /// Optional maximum wall time for one scan or sync pass.
    pub max_scan_wall_time_ms: Option<u64>,
    /// Optional maximum time without verified forward progress.
    pub max_no_progress_time_ms: Option<u64>,
    /// Optional per-repository `CodeGraph` synchronization wall time.
    #[serde(rename = "maxCodeGraphSyncWallTimeMsPerRepo")]
    pub max_codegraph_sync_wall_time_ms_per_repo: Option<u64>,
    /// Optional maximum worker resident memory.
    pub max_worker_memory_bytes: Option<u64>,
    /// Optional cooperative shutdown grace period.
    pub graceful_termination_ms: Option<u64>,
    /// Optional watcher inactivity lease.
    pub watch_idle_timeout_ms: Option<u64>,
    /// Optional absolute watcher-session lifetime.
    pub max_watch_session_wall_time_ms: Option<u64>,
    /// Optional minimum delay between watched sync pass starts.
    pub min_watch_rescan_interval_ms: Option<u64>,
    /// Optional maximum retained checkpoint-cache bytes.
    pub max_checkpoint_cache_bytes: Option<u64>,
    /// Optional corroboration-anchor count, or `-1` for unlimited.
    #[serde(rename = "maxCodeGraphCorroborationAnchorsPerRepo")]
    pub max_codegraph_corroboration_anchors_per_repo: Option<i64>,
    /// Optional Explore wall-time budget.
    pub max_explore_wall_time_ms: Option<u64>,
    /// Optional maximum public provider operations per Explore request.
    #[serde(rename = "maxExploreCodeGraphOperations")]
    pub max_explore_codegraph_operations: Option<u64>,
    /// Optional maximum concurrent provider child processes.
    #[serde(rename = "maxExploreConcurrentCodeGraphProcesses")]
    pub max_explore_concurrent_codegraph_processes: Option<u64>,
    /// Optional maximum source files returned by Explore.
    pub max_explore_source_files: Option<u64>,
    /// Optional maximum resolved local symbols.
    pub max_explore_resolved_symbols: Option<u64>,
    /// Optional maximum symbols selected as traversal anchors.
    pub max_explore_anchors: Option<u64>,
    /// Optional maximum neighbors per anchor and direction.
    pub max_explore_neighbors_per_direction: Option<u64>,
    /// Optional maximum retained local relationships.
    pub max_explore_local_relationships: Option<u64>,
    /// Optional maximum federated handoffs per anchor.
    pub max_explore_federated_handoffs_per_anchor: Option<u64>,
    /// Optional maximum federated handoffs per response.
    pub max_explore_federated_handoffs: Option<u64>,
    /// Optional maximum evidence locations per federated handoff.
    pub max_explore_evidence_locations_per_handoff: Option<u64>,
    /// Optional maximum retained source Markdown bytes.
    pub max_explore_source_markdown_bytes: Option<u64>,
    /// Optional maximum retained enrichment bytes.
    pub max_explore_enrichment_bytes: Option<u64>,
    /// Optional maximum agent next actions per response.
    pub max_agent_next_actions_per_response: Option<u64>,
    /// Optional maximum repository suggestions from Query.
    pub max_query_repository_suggestions: Option<u64>,
    /// Optional final MCP tool response byte limit.
    pub max_mcp_tool_response_bytes: Option<u64>,
    /// Optional MCP resource item limit.
    pub max_mcp_resource_items: Option<u64>,
    /// Optional final MCP resource byte limit.
    pub max_mcp_resource_bytes: Option<u64>,
    /// Optional final MCP schema-catalog byte limit.
    pub max_mcp_schema_catalog_bytes: Option<u64>,
}

/// Effective global execution policy for one workspace operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPolicy {
    /// Maximum wall time for one scan or sync pass.
    pub max_scan_wall_time_ms: u64,
    /// Maximum time without verified forward progress.
    pub max_no_progress_time_ms: u64,
    /// Maximum wall time for one repository-local `CodeGraph` synchronization.
    #[serde(rename = "maxCodeGraphSyncWallTimeMsPerRepo")]
    pub max_codegraph_sync_wall_time_ms_per_repo: u64,
    /// Maximum resident memory accepted for one worker process.
    pub max_worker_memory_bytes: u64,
    /// Cooperative shutdown grace period before forced termination.
    pub graceful_termination_ms: u64,
    /// Inactivity lease for a foreground watch session.
    pub watch_idle_timeout_ms: u64,
    /// Absolute lifetime for one foreground watch session.
    pub max_watch_session_wall_time_ms: u64,
    /// Minimum delay between watched sync pass starts.
    pub min_watch_rescan_interval_ms: u64,
    /// Maximum retained historical checkpoint-cache bytes.
    pub max_checkpoint_cache_bytes: u64,
    /// Effective corroboration-anchor count, or unlimited.
    #[serde(rename = "maxCodeGraphCorroborationAnchorsPerRepo")]
    #[schemars(with = "i64")]
    pub max_codegraph_corroboration_anchors_per_repo: CodeGraphCorroborationAnchorLimit,
    /// Effective Explore wall-time budget.
    pub max_explore_wall_time_ms: u64,
    /// Effective maximum public provider operations per Explore request.
    #[serde(rename = "maxExploreCodeGraphOperations")]
    pub max_explore_codegraph_operations: u64,
    /// Effective maximum concurrent provider child processes.
    #[serde(rename = "maxExploreConcurrentCodeGraphProcesses")]
    pub max_explore_concurrent_codegraph_processes: u64,
    /// Effective maximum source files returned by Explore.
    pub max_explore_source_files: u64,
    /// Effective maximum resolved local symbols.
    pub max_explore_resolved_symbols: u64,
    /// Effective maximum symbols selected as traversal anchors.
    pub max_explore_anchors: u64,
    /// Effective maximum neighbors per anchor and direction.
    pub max_explore_neighbors_per_direction: u64,
    /// Effective maximum retained local relationships.
    pub max_explore_local_relationships: u64,
    /// Effective maximum federated handoffs per anchor.
    pub max_explore_federated_handoffs_per_anchor: u64,
    /// Effective maximum federated handoffs per response.
    pub max_explore_federated_handoffs: u64,
    /// Effective maximum evidence locations per federated handoff.
    pub max_explore_evidence_locations_per_handoff: u64,
    /// Effective maximum retained source Markdown bytes.
    pub max_explore_source_markdown_bytes: u64,
    /// Effective maximum retained enrichment bytes.
    pub max_explore_enrichment_bytes: u64,
    /// Effective maximum agent next actions per response.
    pub max_agent_next_actions_per_response: u64,
    /// Effective maximum repository suggestions from Query.
    pub max_query_repository_suggestions: u64,
    /// Effective final MCP tool response byte limit.
    pub max_mcp_tool_response_bytes: u64,
    /// Effective MCP resource item limit.
    pub max_mcp_resource_items: u64,
    /// Effective final MCP resource byte limit.
    pub max_mcp_resource_bytes: u64,
    /// Effective final MCP schema-catalog byte limit.
    pub max_mcp_schema_catalog_bytes: u64,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        macro_rules! policy_defaults {
            ($($field:ident: $default:ident => $external:literal;)*) => {
                Self {
            max_scan_wall_time_ms: DEFAULT_MAX_SCAN_WALL_TIME_MS,
            max_no_progress_time_ms: DEFAULT_MAX_NO_PROGRESS_TIME_MS,
            max_codegraph_sync_wall_time_ms_per_repo:
                DEFAULT_MAX_CODEGRAPH_SYNC_WALL_TIME_MS_PER_REPO,
            max_worker_memory_bytes: DEFAULT_MAX_WORKER_MEMORY_BYTES,
            graceful_termination_ms: DEFAULT_GRACEFUL_TERMINATION_MS,
            watch_idle_timeout_ms: DEFAULT_WATCH_IDLE_TIMEOUT_MS,
            max_watch_session_wall_time_ms: DEFAULT_MAX_WATCH_SESSION_WALL_TIME_MS,
            min_watch_rescan_interval_ms: DEFAULT_MIN_WATCH_RESCAN_INTERVAL_MS,
            max_checkpoint_cache_bytes: DEFAULT_MAX_CHECKPOINT_CACHE_BYTES,
            max_codegraph_corroboration_anchors_per_repo:
                CodeGraphCorroborationAnchorLimit::try_from(
                    DEFAULT_MAX_CODEGRAPH_CORROBORATION_ANCHORS_PER_REPO,
                )
                .expect("the built-in corroboration limit is valid"),
                    $($field: $default,)*
                }
            };
        }
        with_agent_policy_fields!(policy_defaults)
    }
}

impl ExecutionPolicy {
    /// Resolves a partial global override and validates the effective relationships.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidExecutionPolicy`] for zero, unrepresentable, or inconsistent values.
    pub fn resolve(
        overrides: Option<&ExecutionPolicyOverrides>,
    ) -> Result<Self, InvalidExecutionPolicy> {
        let mut policy = Self::default();
        if let Some(values) = overrides {
            macro_rules! apply {
                ($field:ident) => {
                    if let Some(value) = values.$field {
                        policy.$field = value;
                    }
                };
            }
            apply!(max_scan_wall_time_ms);
            apply!(max_no_progress_time_ms);
            apply!(max_codegraph_sync_wall_time_ms_per_repo);
            apply!(max_worker_memory_bytes);
            apply!(graceful_termination_ms);
            apply!(watch_idle_timeout_ms);
            apply!(max_watch_session_wall_time_ms);
            apply!(min_watch_rescan_interval_ms);
            apply!(max_checkpoint_cache_bytes);
            if let Some(value) = values.max_codegraph_corroboration_anchors_per_repo {
                policy.max_codegraph_corroboration_anchors_per_repo = value.try_into()?;
            }
            macro_rules! apply_agent_overrides {
                ($($field:ident: $default:ident => $external:literal;)*) => {
                    $(apply!($field);)*
                };
            }
            with_agent_policy_fields!(apply_agent_overrides);
        }
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), InvalidExecutionPolicy> {
        self.validate_scalar_values()?;
        self.validate_mcp_minimums()?;
        self.validate_scan_relationships()?;
        self.validate_explore_relationships()
    }

    fn validate_mcp_minimums(&self) -> Result<(), InvalidExecutionPolicy> {
        for (field, value) in [
            ("maxMcpToolResponseBytes", self.max_mcp_tool_response_bytes),
            ("maxMcpResourceBytes", self.max_mcp_resource_bytes),
            (
                "maxMcpSchemaCatalogBytes",
                self.max_mcp_schema_catalog_bytes,
            ),
        ] {
            if value < MIN_MCP_MARKDOWN_BYTES {
                return Err(InvalidExecutionPolicy::BelowMinimum {
                    field,
                    value,
                    minimum: MIN_MCP_MARKDOWN_BYTES,
                });
            }
        }
        Ok(())
    }

    fn validate_scalar_values(&self) -> Result<(), InvalidExecutionPolicy> {
        for (field, value) in self
            .scan_canonical_values()
            .into_iter()
            .chain(self.agent_canonical_values())
        {
            let invalid_bytes = field.ends_with("Bytes") && usize::try_from(value).is_err();
            let invalid_count = !field.ends_with("Ms") && usize::try_from(value).is_err();
            let invalid_sqlite_quota =
                field == "maxCheckpointCacheBytes" && i64::try_from(value).is_err();
            let invalid_deadline = field.ends_with("Ms")
                && Instant::now()
                    .checked_add(Duration::from_millis(value))
                    .is_none();
            if value == 0
                || invalid_bytes
                || invalid_count
                || invalid_sqlite_quota
                || invalid_deadline
            {
                return Err(InvalidExecutionPolicy::InvalidValue { field, value });
            }
        }
        Ok(())
    }

    fn validate_scan_relationships(&self) -> Result<(), InvalidExecutionPolicy> {
        Self::require_not_greater(
            "maxNoProgressTimeMs",
            self.max_no_progress_time_ms,
            "maxScanWallTimeMs",
            self.max_scan_wall_time_ms,
        )?;
        Self::require_not_greater(
            "maxCodeGraphSyncWallTimeMsPerRepo",
            self.max_codegraph_sync_wall_time_ms_per_repo,
            "maxScanWallTimeMs",
            self.max_scan_wall_time_ms,
        )?;
        Self::require_not_greater(
            "gracefulTerminationMs",
            self.graceful_termination_ms,
            "maxNoProgressTimeMs",
            self.max_no_progress_time_ms,
        )?;
        Self::require_not_greater(
            "watchIdleTimeoutMs",
            self.watch_idle_timeout_ms,
            "maxWatchSessionWallTimeMs",
            self.max_watch_session_wall_time_ms,
        )?;
        Self::require_not_greater(
            "minWatchRescanIntervalMs",
            self.min_watch_rescan_interval_ms,
            "watchIdleTimeoutMs",
            self.watch_idle_timeout_ms,
        )
    }

    fn validate_explore_relationships(&self) -> Result<(), InvalidExecutionPolicy> {
        Self::require_not_greater(
            "maxExploreAnchors",
            self.max_explore_anchors,
            "maxExploreResolvedSymbols",
            self.max_explore_resolved_symbols,
        )?;
        Self::require_not_greater(
            "maxExploreConcurrentCodeGraphProcesses",
            self.max_explore_concurrent_codegraph_processes,
            "maxExploreCodeGraphOperations",
            self.max_explore_codegraph_operations,
        )?;
        let local_ceiling = self
            .max_explore_anchors
            .checked_mul(2)
            .and_then(|value| value.checked_mul(self.max_explore_neighbors_per_direction))
            .ok_or(InvalidExecutionPolicy::ArithmeticOverflow {
                expression: "maxExploreAnchors × 2 × maxExploreNeighborsPerDirection",
            })?;
        Self::require_not_greater(
            "maxExploreLocalRelationships",
            self.max_explore_local_relationships,
            "anchor neighbor capacity",
            local_ceiling,
        )?;
        let handoff_ceiling = self
            .max_explore_anchors
            .checked_mul(self.max_explore_federated_handoffs_per_anchor)
            .ok_or(InvalidExecutionPolicy::ArithmeticOverflow {
                expression: "maxExploreAnchors × maxExploreFederatedHandoffsPerAnchor",
            })?;
        Self::require_not_greater(
            "maxExploreFederatedHandoffs",
            self.max_explore_federated_handoffs,
            "anchor handoff capacity",
            handoff_ceiling,
        )?;
        Self::require_not_greater(
            "maxExploreSourceMarkdownBytes",
            self.max_explore_source_markdown_bytes,
            "maxMcpToolResponseBytes",
            self.max_mcp_tool_response_bytes,
        )?;
        Self::require_not_greater(
            "maxExploreEnrichmentBytes",
            self.max_explore_enrichment_bytes,
            "maxMcpToolResponseBytes",
            self.max_mcp_tool_response_bytes,
        )
    }

    fn require_not_greater(
        field: &'static str,
        value: u64,
        maximum_field: &'static str,
        maximum: u64,
    ) -> Result<(), InvalidExecutionPolicy> {
        if value > maximum {
            return Err(InvalidExecutionPolicy::InvalidRelationship {
                field,
                value,
                maximum_field,
                maximum,
            });
        }
        Ok(())
    }

    fn scan_canonical_values(&self) -> [(&'static str, u64); 9] {
        [
            ("maxScanWallTimeMs", self.max_scan_wall_time_ms),
            ("maxNoProgressTimeMs", self.max_no_progress_time_ms),
            (
                "maxCodeGraphSyncWallTimeMsPerRepo",
                self.max_codegraph_sync_wall_time_ms_per_repo,
            ),
            ("maxWorkerMemoryBytes", self.max_worker_memory_bytes),
            ("gracefulTerminationMs", self.graceful_termination_ms),
            ("watchIdleTimeoutMs", self.watch_idle_timeout_ms),
            (
                "maxWatchSessionWallTimeMs",
                self.max_watch_session_wall_time_ms,
            ),
            (
                "minWatchRescanIntervalMs",
                self.min_watch_rescan_interval_ms,
            ),
            ("maxCheckpointCacheBytes", self.max_checkpoint_cache_bytes),
        ]
    }

    fn agent_canonical_values(&self) -> Vec<(&'static str, u64)> {
        macro_rules! canonical_agent_values {
            ($($field:ident: $default:ident => $external:literal;)*) => {
                vec![$(($external, self.$field),)*]
            };
        }
        with_agent_policy_fields!(canonical_agent_values)
    }

    /// Returns the stable canonical fingerprint of the effective operational policy.
    #[must_use]
    pub fn scan_fingerprint(&self) -> String {
        let mut canonical = self
            .scan_canonical_values()
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(";");
        write!(
            canonical,
            ";maxCodeGraphCorroborationAnchorsPerRepo={}",
            self.max_codegraph_corroboration_anchors_per_repo
        )
        .expect("writing to a String cannot fail");
        stable_id("execution-policy", &canonical)
    }

    #[must_use]
    /// Returns the historical scan-only fingerprint alias.
    pub fn fingerprint(&self) -> String {
        self.scan_fingerprint()
    }

    /// Returns a fingerprint that also includes the additive `CodeGraph` corroboration bound.
    #[must_use]
    pub fn fingerprint_with_codegraph_limit(
        &self,
        limit: CodeGraphCorroborationAnchorLimit,
    ) -> String {
        let mut canonical = self
            .scan_canonical_values()
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(";");
        write!(
            canonical,
            ";maxCodeGraphCorroborationAnchorsPerRepo={limit}"
        )
        .expect("writing to a String cannot fail");
        stable_id("execution-policy", &canonical)
    }

    /// Returns the fingerprint for limits that only affect ephemeral agent delivery.
    #[must_use]
    pub fn agent_delivery_fingerprint(&self) -> String {
        let mut canonical = String::new();
        for (index, (name, value)) in self.agent_canonical_values().into_iter().enumerate() {
            if index > 0 {
                canonical.push(';');
            }
            write!(canonical, "{name}={value}").expect("writing to a String cannot fail");
        }
        stable_id("agent-delivery-policy", &canonical)
    }
}

/// Invalid operator-owned execution policy.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvalidExecutionPolicy {
    /// One effective value is zero or cannot be represented internally.
    #[error("execution policy `{field}` must be positive and representable; received {value}")]
    InvalidValue {
        /// Manifest field containing the invalid value.
        field: &'static str,
        /// Rejected numeric value.
        value: u64,
    },
    /// An MCP Markdown budget cannot retain its mandatory control block.
    #[error("execution policy `{field}` must be at least {minimum}; received {value}")]
    BelowMinimum {
        /// Manifest field containing the undersized budget.
        field: &'static str,
        /// Rejected byte budget.
        value: u64,
        /// Smallest supported byte budget.
        minimum: u64,
    },
    /// One subordinate deadline exceeds its containing deadline.
    #[error("execution policy `{field}` ({value}) must not exceed `{maximum_field}` ({maximum})")]
    InvalidRelationship {
        /// Subordinate manifest field.
        field: &'static str,
        /// Supplied subordinate value.
        value: u64,
        /// Containing manifest field.
        maximum_field: &'static str,
        /// Supplied containing value.
        maximum: u64,
    },
    /// A checked capacity relationship overflowed before it could be compared.
    #[error("execution policy arithmetic overflow in `{expression}`")]
    ArithmeticOverflow {
        /// Checked expression that overflowed.
        expression: &'static str,
    },
}

/// Observable phase of one supervised scan or synchronization pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    /// Workspace configuration and checkout validation.
    Configuration,
    /// Filesystem artifact discovery.
    Discovery,
    /// Bounded content fingerprinting.
    Fingerprinting,
    /// Source-owned extraction and batch encoding.
    Extraction,
    /// Candidate graph assembly and linking.
    GraphAssembly,
    /// Deterministic community analysis.
    Communities,
    /// Atomic snapshot persistence.
    Publication,
    /// External repository-local `CodeGraph` synchronization.
    CodeGraphSync,
}

/// Resource exhausted by one supervised workspace operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionResource {
    /// Absolute wall time.
    WallTimeMs,
    /// Time without verified forward progress.
    NoProgressTimeMs,
    /// Resident worker memory.
    WorkerMemoryBytes,
    /// Deterministic work accounting overflow.
    WorkUnits,
    /// The worker terminated without a valid final protocol message.
    WorkerProcess,
    /// The worker protocol exceeded its byte limit or was malformed.
    WorkerProtocolBytes,
    /// The operator or hosting agent requested cancellation.
    Cancellation,
}

/// Observable resource accounting attached to a completed scan or sync pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSummary {
    /// Unique identifier assigned by the supervisor to this pass.
    pub run_id: String,
    /// Monotonic wall-clock duration observed by the supervisor.
    pub duration_ms: u64,
    /// Largest combined resident set observed for the worker process tree.
    pub peak_worker_memory_bytes: u64,
    /// Number of completed units reported through the progress protocol.
    pub completed_work_units: u64,
    /// Number of deterministic checkpoint records reused by this pass.
    pub checkpoint_hits: u64,
    /// Number of complete deterministic checkpoint records written by this pass.
    pub checkpoints_written: u64,
    /// Number of artifact-extractor invocations measured by the worker.
    pub measured_artifacts: u64,
    /// Median artifact-extractor duration in milliseconds.
    pub artifact_duration_p50_ms: u64,
    /// 95th-percentile artifact-extractor duration in milliseconds.
    pub artifact_duration_p95_ms: u64,
    /// 99th-percentile artifact-extractor duration in milliseconds.
    pub artifact_duration_p99_ms: u64,
}

/// Typed failure produced when one supervised execution resource is exhausted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Error)]
#[error(
    "execution `{run_id}` exceeded {resource:?} during {phase:?}: observed {observed}, maximum {maximum}, completed {completed_units} units"
)]
pub struct ExecutionLimitExceeded {
    /// Stable identifier of the supervised operation.
    pub run_id: String,
    /// Phase active when the resource was exhausted.
    pub phase: JobPhase,
    /// Exhausted resource.
    pub resource: ExecutionResource,
    /// Observed resource value.
    pub observed: u64,
    /// Effective maximum.
    pub maximum: u64,
    /// Verified work units completed before rejection.
    pub completed_units: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_should_be_generous_and_finite() {
        let policy = ExecutionPolicy::default();

        assert_eq!(policy.max_scan_wall_time_ms, 21_600_000);
        assert_eq!(
            CodeGraphCorroborationAnchorLimit::try_from(
                DEFAULT_MAX_CODEGRAPH_CORROBORATION_ANCHORS_PER_REPO
            )
            .expect("default anchor limit")
            .bounded()
            .map(NonZeroUsize::get),
            Some(50)
        );
        assert_eq!(policy.max_worker_memory_bytes, 17_179_869_184);
        assert_eq!(policy.max_checkpoint_cache_bytes, 10_737_418_240);
    }

    #[test]
    fn partial_override_should_preserve_other_defaults() {
        let policy = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_scan_wall_time_ms: Some(28_800_000),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect("valid override");

        assert_eq!(policy.max_scan_wall_time_ms, 28_800_000);
        assert_eq!(policy.max_no_progress_time_ms, 300_000);
    }

    #[test]
    fn zero_should_be_rejected() {
        let error = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_worker_memory_bytes: Some(0),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect_err("zero must fail");

        assert!(matches!(
            error,
            InvalidExecutionPolicy::InvalidValue {
                field: "maxWorkerMemoryBytes",
                value: 0
            }
        ));
    }

    #[test]
    fn corroboration_anchor_limit_should_accept_positive_or_unlimited() {
        let bounded =
            CodeGraphCorroborationAnchorLimit::try_from(12).expect("positive anchor limit");
        let unlimited =
            CodeGraphCorroborationAnchorLimit::try_from(-1).expect("unlimited anchor limit");

        assert_eq!(bounded.bounded().map(NonZeroUsize::get), Some(12));
        assert_eq!(unlimited.bounded(), None);
        assert_ne!(
            ExecutionPolicy::default().fingerprint_with_codegraph_limit(bounded),
            ExecutionPolicy::default().fingerprint_with_codegraph_limit(unlimited)
        );
        assert_eq!(
            serde_json::to_value(bounded).expect("bounded limit serializes"),
            serde_json::json!(12)
        );
        assert_eq!(
            serde_json::to_value(unlimited).expect("unlimited limit serializes"),
            serde_json::json!(-1)
        );
    }

    #[test]
    fn corroboration_anchor_limit_should_reject_zero_and_values_below_sentinel() {
        for value in [0, -2] {
            let error = CodeGraphCorroborationAnchorLimit::try_from(value)
                .expect_err("invalid anchor limit");

            assert!(matches!(
                error,
                InvalidExecutionPolicy::InvalidValue {
                    field: "maxCodeGraphCorroborationAnchorsPerRepo",
                    value: observed
                } if observed == value.unsigned_abs()
            ));
        }
    }

    #[test]
    fn technically_unrepresentable_values_should_be_rejected() {
        let quota = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_checkpoint_cache_bytes: Some(u64::MAX),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect_err("SQLite quota overflow must fail");

        assert!(matches!(quota, InvalidExecutionPolicy::InvalidValue { .. }));
    }

    #[test]
    fn mcp_markdown_budgets_should_enforce_the_control_block_minimum() {
        for overrides in [
            ExecutionPolicyOverrides {
                max_mcp_tool_response_bytes: Some(MIN_MCP_MARKDOWN_BYTES - 1),
                ..ExecutionPolicyOverrides::default()
            },
            ExecutionPolicyOverrides {
                max_mcp_resource_bytes: Some(MIN_MCP_MARKDOWN_BYTES - 1),
                ..ExecutionPolicyOverrides::default()
            },
            ExecutionPolicyOverrides {
                max_mcp_schema_catalog_bytes: Some(MIN_MCP_MARKDOWN_BYTES - 1),
                ..ExecutionPolicyOverrides::default()
            },
        ] {
            assert!(matches!(
                ExecutionPolicy::resolve(Some(&overrides)),
                Err(InvalidExecutionPolicy::BelowMinimum {
                    minimum: MIN_MCP_MARKDOWN_BYTES,
                    ..
                })
            ));
        }

        let minimum = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_mcp_tool_response_bytes: Some(MIN_MCP_MARKDOWN_BYTES),
            max_mcp_resource_bytes: Some(MIN_MCP_MARKDOWN_BYTES),
            max_mcp_schema_catalog_bytes: Some(MIN_MCP_MARKDOWN_BYTES),
            max_explore_source_markdown_bytes: Some(MIN_MCP_MARKDOWN_BYTES),
            max_explore_enrichment_bytes: Some(MIN_MCP_MARKDOWN_BYTES),
            ..ExecutionPolicyOverrides::default()
        }));
        assert!(minimum.is_ok());
    }

    #[test]
    fn subordinate_deadline_should_not_exceed_scan_deadline() {
        let error = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_scan_wall_time_ms: Some(1_000),
            max_no_progress_time_ms: Some(1_001),
            max_codegraph_sync_wall_time_ms_per_repo: Some(1_000),
            graceful_termination_ms: Some(500),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect_err("relationship must fail");

        assert!(matches!(
            error,
            InvalidExecutionPolicy::InvalidRelationship {
                field: "maxNoProgressTimeMs",
                ..
            }
        ));
    }

    #[test]
    fn fingerprint_should_ignore_yaml_field_order() {
        let first = ExecutionPolicy::default();
        let second = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides::default()))
            .expect("defaults valid");

        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn agent_delivery_limits_should_not_change_scan_fingerprint() {
        let baseline = ExecutionPolicy::default();
        let changed = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_mcp_tool_response_bytes: Some(600_000),
            max_explore_source_markdown_bytes: Some(300_000),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect("agent-only override is valid");

        assert_eq!(baseline.scan_fingerprint(), changed.scan_fingerprint());
        assert_ne!(
            baseline.agent_delivery_fingerprint(),
            changed.agent_delivery_fingerprint()
        );
    }

    #[test]
    fn explore_capacity_relationships_should_be_checked() {
        for overrides in [
            ExecutionPolicyOverrides {
                max_explore_anchors: Some(6),
                ..ExecutionPolicyOverrides::default()
            },
            ExecutionPolicyOverrides {
                max_explore_concurrent_codegraph_processes: Some(9),
                ..ExecutionPolicyOverrides::default()
            },
            ExecutionPolicyOverrides {
                max_explore_local_relationships: Some(49),
                ..ExecutionPolicyOverrides::default()
            },
            ExecutionPolicyOverrides {
                max_explore_federated_handoffs: Some(31),
                ..ExecutionPolicyOverrides::default()
            },
            ExecutionPolicyOverrides {
                max_explore_source_markdown_bytes: Some(524_289),
                ..ExecutionPolicyOverrides::default()
            },
        ] {
            assert!(matches!(
                ExecutionPolicy::resolve(Some(&overrides)),
                Err(InvalidExecutionPolicy::InvalidRelationship { .. })
            ));
        }
    }
}
