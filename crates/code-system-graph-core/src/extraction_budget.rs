use std::fmt;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use code_system_graph_model::stable_id;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Definitive initial extraction payload contract.
pub const EXTRACTION_CONTRACT_VERSION: &str = "1.0.0";

/// Default maximum input bytes accepted for one artifact-extractor invocation.
pub const DEFAULT_MAX_INPUT_BYTES_PER_ARTIFACT: u64 = 8_388_608;
/// Default maximum structured nesting depth for one artifact-extractor invocation.
pub const DEFAULT_MAX_STRUCTURAL_DEPTH_PER_ARTIFACT: u64 = 64;
/// Default maximum syntax-tree depth for one artifact-extractor invocation.
pub const DEFAULT_MAX_AST_DEPTH_PER_ARTIFACT: u64 = 256;
/// Default maximum deterministic work units for one artifact-extractor invocation.
pub const DEFAULT_MAX_WORK_UNITS_PER_ARTIFACT: u64 = 100_000;
/// Default maximum visited Tree-sitter nodes for one artifact-extractor invocation.
pub const DEFAULT_MAX_TREE_SITTER_NODES_PER_ARTIFACT: u64 = 500_000;
/// Default maximum observations for one artifact-extractor invocation.
pub const DEFAULT_MAX_OBSERVATIONS_PER_ARTIFACT: u64 = 100_000;
/// Default maximum accumulated extracted string bytes per invocation.
pub const DEFAULT_MAX_ACCUMULATED_STRING_BYTES_PER_ARTIFACT: u64 = 33_554_432;
/// Default maximum serialized output bytes per invocation.
pub const DEFAULT_MAX_SERIALIZED_OUTPUT_BYTES_PER_ARTIFACT: u64 = 33_554_432;
/// Default maximum bytes in one extracted string value.
pub const DEFAULT_MAX_STRING_BYTES_PER_VALUE: u64 = 65_536;
/// Default maximum bytes in one portable path value.
pub const DEFAULT_MAX_PORTABLE_PATH_BYTES_PER_VALUE: u64 = 4_096;
/// Default maximum bytes in one identifier value.
pub const DEFAULT_MAX_IDENTIFIER_BYTES_PER_VALUE: u64 = 1_024;
/// Default structured-extractor wall time per invocation.
pub const DEFAULT_MAX_STRUCTURED_WALL_TIME_MS_PER_ARTIFACT: u64 = 5_000;
/// Default Tree-sitter wall time per invocation.
pub const DEFAULT_MAX_TREE_SITTER_WALL_TIME_MS_PER_ARTIFACT: u64 = 10_000;

/// Optional operator-owned overrides from the workspace manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtractionBudgetOverrides {
    /// Optional input-byte maximum.
    pub max_input_bytes_per_artifact: Option<u64>,
    /// Optional structured-depth maximum.
    pub max_structural_depth_per_artifact: Option<u64>,
    /// Optional syntax-tree-depth maximum.
    pub max_ast_depth_per_artifact: Option<u64>,
    /// Optional structured-work maximum.
    pub max_work_units_per_artifact: Option<u64>,
    /// Optional visited Tree-sitter-node maximum.
    pub max_tree_sitter_nodes_per_artifact: Option<u64>,
    /// Optional observation maximum.
    pub max_observations_per_artifact: Option<u64>,
    /// Optional accumulated extracted-string-byte maximum.
    pub max_accumulated_string_bytes_per_artifact: Option<u64>,
    /// Optional serialized-output-byte maximum.
    pub max_serialized_output_bytes_per_artifact: Option<u64>,
    /// Optional per-string byte maximum.
    pub max_string_bytes_per_value: Option<u64>,
    /// Optional per-portable-path byte maximum.
    pub max_portable_path_bytes_per_value: Option<u64>,
    /// Optional per-identifier byte maximum.
    pub max_identifier_bytes_per_value: Option<u64>,
    /// Optional structured wall-time maximum in milliseconds.
    pub max_structured_wall_time_ms_per_artifact: Option<u64>,
    /// Optional Tree-sitter wall-time maximum in milliseconds.
    pub max_tree_sitter_wall_time_ms_per_artifact: Option<u64>,
}

/// Effective extraction limits for one scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionBudgets {
    /// Input-byte maximum.
    pub max_input_bytes_per_artifact: u64,
    /// Structured-depth maximum.
    pub max_structural_depth_per_artifact: u64,
    /// Syntax-tree-depth maximum.
    pub max_ast_depth_per_artifact: u64,
    /// Structured-work maximum.
    pub max_work_units_per_artifact: u64,
    /// Visited Tree-sitter-node maximum.
    pub max_tree_sitter_nodes_per_artifact: u64,
    /// Observation maximum.
    pub max_observations_per_artifact: u64,
    /// Accumulated extracted-string-byte maximum.
    pub max_accumulated_string_bytes_per_artifact: u64,
    /// Serialized-output-byte maximum.
    pub max_serialized_output_bytes_per_artifact: u64,
    /// Per-string byte maximum.
    pub max_string_bytes_per_value: u64,
    /// Per-portable-path byte maximum.
    pub max_portable_path_bytes_per_value: u64,
    /// Per-identifier byte maximum.
    pub max_identifier_bytes_per_value: u64,
    /// Structured wall-time maximum in milliseconds.
    pub max_structured_wall_time_ms_per_artifact: u64,
    /// Tree-sitter wall-time maximum in milliseconds.
    pub max_tree_sitter_wall_time_ms_per_artifact: u64,
}

impl Default for ExtractionBudgets {
    fn default() -> Self {
        Self {
            max_input_bytes_per_artifact: DEFAULT_MAX_INPUT_BYTES_PER_ARTIFACT,
            max_structural_depth_per_artifact: DEFAULT_MAX_STRUCTURAL_DEPTH_PER_ARTIFACT,
            max_ast_depth_per_artifact: DEFAULT_MAX_AST_DEPTH_PER_ARTIFACT,
            max_work_units_per_artifact: DEFAULT_MAX_WORK_UNITS_PER_ARTIFACT,
            max_tree_sitter_nodes_per_artifact: DEFAULT_MAX_TREE_SITTER_NODES_PER_ARTIFACT,
            max_observations_per_artifact: DEFAULT_MAX_OBSERVATIONS_PER_ARTIFACT,
            max_accumulated_string_bytes_per_artifact:
                DEFAULT_MAX_ACCUMULATED_STRING_BYTES_PER_ARTIFACT,
            max_serialized_output_bytes_per_artifact:
                DEFAULT_MAX_SERIALIZED_OUTPUT_BYTES_PER_ARTIFACT,
            max_string_bytes_per_value: DEFAULT_MAX_STRING_BYTES_PER_VALUE,
            max_portable_path_bytes_per_value: DEFAULT_MAX_PORTABLE_PATH_BYTES_PER_VALUE,
            max_identifier_bytes_per_value: DEFAULT_MAX_IDENTIFIER_BYTES_PER_VALUE,
            max_structured_wall_time_ms_per_artifact:
                DEFAULT_MAX_STRUCTURED_WALL_TIME_MS_PER_ARTIFACT,
            max_tree_sitter_wall_time_ms_per_artifact:
                DEFAULT_MAX_TREE_SITTER_WALL_TIME_MS_PER_ARTIFACT,
        }
    }
}

impl ExtractionBudgets {
    /// Resolves a partial override after validating every effective value.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidExtractionBudget`] when an effective value is zero or cannot be
    /// represented by the running build.
    pub fn resolve(
        overrides: Option<&ExtractionBudgetOverrides>,
    ) -> Result<Self, InvalidExtractionBudget> {
        let mut budgets = Self::default();
        if let Some(values) = overrides {
            macro_rules! apply {
                ($field:ident) => {
                    if let Some(value) = values.$field {
                        budgets.$field = value;
                    }
                };
            }
            apply!(max_input_bytes_per_artifact);
            apply!(max_structural_depth_per_artifact);
            apply!(max_ast_depth_per_artifact);
            apply!(max_work_units_per_artifact);
            apply!(max_tree_sitter_nodes_per_artifact);
            apply!(max_observations_per_artifact);
            apply!(max_accumulated_string_bytes_per_artifact);
            apply!(max_serialized_output_bytes_per_artifact);
            apply!(max_string_bytes_per_value);
            apply!(max_portable_path_bytes_per_value);
            apply!(max_identifier_bytes_per_value);
            apply!(max_structured_wall_time_ms_per_artifact);
            apply!(max_tree_sitter_wall_time_ms_per_artifact);
        }
        budgets.validate()?;
        Ok(budgets)
    }

    fn validate(&self) -> Result<(), InvalidExtractionBudget> {
        for (field, value) in self.canonical_values() {
            if value == 0 {
                return Err(InvalidExtractionBudget { field, value });
            }
            if field.contains("Bytes") {
                usize::try_from(value).map_err(|_| InvalidExtractionBudget { field, value })?;
            }
        }
        Ok(())
    }

    fn canonical_values(&self) -> [(&'static str, u64); 13] {
        [
            (
                "maxInputBytesPerArtifact",
                self.max_input_bytes_per_artifact,
            ),
            (
                "maxStructuralDepthPerArtifact",
                self.max_structural_depth_per_artifact,
            ),
            ("maxAstDepthPerArtifact", self.max_ast_depth_per_artifact),
            ("maxWorkUnitsPerArtifact", self.max_work_units_per_artifact),
            (
                "maxTreeSitterNodesPerArtifact",
                self.max_tree_sitter_nodes_per_artifact,
            ),
            (
                "maxObservationsPerArtifact",
                self.max_observations_per_artifact,
            ),
            (
                "maxAccumulatedStringBytesPerArtifact",
                self.max_accumulated_string_bytes_per_artifact,
            ),
            (
                "maxSerializedOutputBytesPerArtifact",
                self.max_serialized_output_bytes_per_artifact,
            ),
            ("maxStringBytesPerValue", self.max_string_bytes_per_value),
            (
                "maxPortablePathBytesPerValue",
                self.max_portable_path_bytes_per_value,
            ),
            (
                "maxIdentifierBytesPerValue",
                self.max_identifier_bytes_per_value,
            ),
            (
                "maxStructuredWallTimeMsPerArtifact",
                self.max_structured_wall_time_ms_per_artifact,
            ),
            (
                "maxTreeSitterWallTimeMsPerArtifact",
                self.max_tree_sitter_wall_time_ms_per_artifact,
            ),
        ]
    }

    /// Returns the stable canonical fingerprint used to authorize batch reuse.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let canonical = self
            .canonical_values()
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(";");
        stable_id("extraction-budgets", &canonical)
    }
}

/// Invalid operator-owned extraction budget.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("extraction budget `{field}` must be positive and representable; received {value}")]
pub struct InvalidExtractionBudget {
    /// Manifest field containing the invalid value.
    pub field: &'static str,
    /// Rejected numeric value.
    pub value: u64,
}

/// Resource charged by one extraction invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionResource {
    /// Source input bytes.
    InputBytes,
    /// Structured parser nesting depth.
    StructuralDepth,
    /// Syntax-tree nesting depth.
    AstDepth,
    /// Deterministic structured work.
    WorkUnits,
    /// Visited Tree-sitter nodes.
    TreeSitterNodes,
    /// Materialized observations.
    Observations,
    /// Accumulated extracted string bytes.
    AccumulatedStringBytes,
    /// Serialized JSON output bytes.
    SerializedOutputBytes,
    /// Bytes in one extracted string.
    StringBytesPerValue,
    /// Bytes in one portable path.
    PortablePathBytesPerValue,
    /// Bytes in one identifier.
    IdentifierBytesPerValue,
    /// Structured extraction wall time.
    StructuredWallTimeMs,
    /// Tree-sitter parsing and traversal wall time.
    TreeSitterWallTimeMs,
}

impl fmt::Display for ExtractionResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = serde_json::to_string(self).map_err(|_| fmt::Error)?;
        formatter.write_str(name.trim_matches('"'))
    }
}

/// Typed fail-closed extraction limit error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Error)]
#[error(
    "extraction limit exceeded for `{extractor}` at `{artifact}`: {resource} observed {observed}, maximum {maximum}"
)]
pub struct ExtractionLimitExceeded {
    /// Checkout-relative artifact display path.
    pub artifact: String,
    /// Extractor identity.
    pub extractor: String,
    /// Resource whose maximum was exceeded.
    pub resource: ExtractionResource,
    /// First rejected observed value.
    pub observed: u64,
    /// Effective configured maximum.
    pub maximum: u64,
}

/// Monotonic elapsed-time source used by extraction trackers.
pub trait ExtractionClock: Send {
    /// Returns elapsed monotonic time since the invocation started.
    fn elapsed(&self) -> Duration;
}

struct SystemClock(Instant);

impl ExtractionClock for SystemClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Mutable accounting state owned by one artifact-extractor invocation.
pub struct ExtractionTracker {
    budgets: ExtractionBudgets,
    artifact: String,
    extractor: String,
    work_units: u64,
    tree_sitter_nodes: u64,
    observations: u64,
    accumulated_string_bytes: u64,
    sampled_units: u64,
    clock: Box<dyn ExtractionClock>,
}

impl fmt::Debug for ExtractionTracker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExtractionTracker")
            .field("budgets", &self.budgets)
            .field("artifact", &self.artifact)
            .field("extractor", &self.extractor)
            .field("work_units", &self.work_units)
            .field("tree_sitter_nodes", &self.tree_sitter_nodes)
            .field("observations", &self.observations)
            .field("accumulated_string_bytes", &self.accumulated_string_bytes)
            .finish_non_exhaustive()
    }
}

impl ExtractionTracker {
    /// Starts a fresh tracker for one physical artifact and one extractor.
    #[must_use]
    pub fn new(
        artifact: impl Into<String>,
        extractor: impl Into<String>,
        budgets: &ExtractionBudgets,
    ) -> Self {
        Self {
            budgets: budgets.clone(),
            artifact: artifact.into(),
            extractor: extractor.into(),
            work_units: 0,
            tree_sitter_nodes: 0,
            observations: 0,
            accumulated_string_bytes: 0,
            sampled_units: 0,
            clock: Box::new(SystemClock(Instant::now())),
        }
    }

    /// Starts a tracker with an injected monotonic clock.
    #[must_use]
    pub fn with_clock(
        artifact: impl Into<String>,
        extractor: impl Into<String>,
        budgets: &ExtractionBudgets,
        clock: Box<dyn ExtractionClock>,
    ) -> Self {
        let mut tracker = Self::new(artifact, extractor, budgets);
        tracker.clock = clock;
        tracker
    }

    /// Returns the effective limits for this invocation.
    #[must_use]
    pub fn budgets(&self) -> &ExtractionBudgets {
        &self.budgets
    }

    /// Checks source bytes before parsing or decoding them.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] when `observed` exceeds the input-byte maximum.
    pub fn check_input_bytes(&self, observed: u64) -> Result<(), ExtractionLimitExceeded> {
        if observed > self.budgets.max_input_bytes_per_artifact {
            return Err(self.exceeded(
                ExtractionResource::InputBytes,
                observed,
                self.budgets.max_input_bytes_per_artifact,
            ));
        }
        Ok(())
    }

    fn exceeded(
        &self,
        resource: ExtractionResource,
        observed: u64,
        maximum: u64,
    ) -> ExtractionLimitExceeded {
        ExtractionLimitExceeded {
            artifact: self.artifact.clone(),
            extractor: self.extractor.clone(),
            resource,
            observed,
            maximum,
        }
    }

    fn charge_counter(
        &self,
        current: u64,
        amount: u64,
        maximum: u64,
        resource: ExtractionResource,
    ) -> Result<u64, ExtractionLimitExceeded> {
        let observed = current
            .checked_add(amount)
            .ok_or_else(|| self.exceeded(resource, u64::MAX, maximum))?;
        if observed > maximum {
            return Err(self.exceeded(resource, observed, maximum));
        }
        Ok(observed)
    }

    /// Charges deterministic work before the associated allocation or traversal.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] before accepting work beyond the configured maximum or
    /// when the sampled structured deadline has elapsed.
    pub fn charge_work(&mut self, amount: u64) -> Result<(), ExtractionLimitExceeded> {
        self.work_units = self.charge_counter(
            self.work_units,
            amount,
            self.budgets.max_work_units_per_artifact,
            ExtractionResource::WorkUnits,
        )?;
        self.sample_time(false)
    }

    /// Charges one Tree-sitter node before visiting its children.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] for excessive AST depth, node count, or sampled
    /// Tree-sitter wall time.
    pub fn charge_tree_sitter_node(&mut self, depth: u64) -> Result<(), ExtractionLimitExceeded> {
        self.check_ast_depth(depth)?;
        self.tree_sitter_nodes = self.charge_counter(
            self.tree_sitter_nodes,
            1,
            self.budgets.max_tree_sitter_nodes_per_artifact,
            ExtractionResource::TreeSitterNodes,
        )?;
        self.sample_time(true)
    }

    /// Charges observations before inserting them into a collection.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] before the observation maximum is exceeded.
    pub fn charge_observation(&mut self, amount: u64) -> Result<(), ExtractionLimitExceeded> {
        self.observations = self.charge_counter(
            self.observations,
            amount,
            self.budgets.max_observations_per_artifact,
            ExtractionResource::Observations,
        )?;
        Ok(())
    }

    /// Ensures at least the supplied number of output observations has been charged.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] when the minimum would exceed the observation maximum.
    pub fn ensure_observations(&mut self, minimum: u64) -> Result<(), ExtractionLimitExceeded> {
        if self.observations < minimum {
            self.charge_observation(minimum - self.observations)?;
        }
        Ok(())
    }

    /// Charges extracted string bytes and validates the per-value ceiling before cloning.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] for an oversized value or accumulated string total.
    pub fn charge_string(&mut self, value: &str) -> Result<(), ExtractionLimitExceeded> {
        self.charge_value(
            value,
            self.budgets.max_string_bytes_per_value,
            ExtractionResource::StringBytesPerValue,
        )
    }

    /// Charges a portable path before it is materialized.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] for an oversized path or accumulated string total.
    pub fn charge_portable_path(&mut self, value: &str) -> Result<(), ExtractionLimitExceeded> {
        self.charge_value(
            value,
            self.budgets.max_portable_path_bytes_per_value,
            ExtractionResource::PortablePathBytesPerValue,
        )
    }

    /// Charges an identifier before it is materialized.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] for an oversized identifier or accumulated string
    /// total.
    pub fn charge_identifier(&mut self, value: &str) -> Result<(), ExtractionLimitExceeded> {
        self.charge_value(
            value,
            self.budgets.max_identifier_bytes_per_value,
            ExtractionResource::IdentifierBytesPerValue,
        )
    }

    fn charge_value(
        &mut self,
        value: &str,
        maximum: u64,
        resource: ExtractionResource,
    ) -> Result<(), ExtractionLimitExceeded> {
        let bytes = u64::try_from(value.len()).unwrap_or(u64::MAX);
        if bytes > maximum {
            return Err(self.exceeded(resource, bytes, maximum));
        }
        self.accumulated_string_bytes = self.charge_counter(
            self.accumulated_string_bytes,
            bytes,
            self.budgets.max_accumulated_string_bytes_per_artifact,
            ExtractionResource::AccumulatedStringBytes,
        )?;
        Ok(())
    }

    /// Checks structured nesting before entering the next level.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] when `depth` exceeds the structural maximum.
    pub fn check_structural_depth(&self, depth: u64) -> Result<(), ExtractionLimitExceeded> {
        if depth > self.budgets.max_structural_depth_per_artifact {
            return Err(self.exceeded(
                ExtractionResource::StructuralDepth,
                depth,
                self.budgets.max_structural_depth_per_artifact,
            ));
        }
        Ok(())
    }

    /// Checks AST nesting before visiting a syntax node.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] when `depth` exceeds the AST maximum.
    pub fn check_ast_depth(&self, depth: u64) -> Result<(), ExtractionLimitExceeded> {
        if depth > self.budgets.max_ast_depth_per_artifact {
            return Err(self.exceeded(
                ExtractionResource::AstDepth,
                depth,
                self.budgets.max_ast_depth_per_artifact,
            ));
        }
        Ok(())
    }

    /// Checks structured wall time immediately, including parser callbacks.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] after the structured wall-time deadline.
    pub fn check_structured_time(&self) -> Result<(), ExtractionLimitExceeded> {
        self.check_elapsed(false)
    }

    /// Checks Tree-sitter wall time immediately, including parser callbacks.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionLimitExceeded`] after the Tree-sitter wall-time deadline.
    pub fn check_tree_sitter_time(&self) -> Result<(), ExtractionLimitExceeded> {
        self.check_elapsed(true)
    }

    fn sample_time(&mut self, tree_sitter: bool) -> Result<(), ExtractionLimitExceeded> {
        let units = self.work_units.saturating_add(self.tree_sitter_nodes);
        if units / 1_024 > self.sampled_units / 1_024 {
            self.sampled_units = units;
            self.check_elapsed(tree_sitter)?;
        }
        Ok(())
    }

    fn check_elapsed(&self, tree_sitter: bool) -> Result<(), ExtractionLimitExceeded> {
        let observed = u64::try_from(self.clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (resource, maximum) = if tree_sitter {
            (
                ExtractionResource::TreeSitterWallTimeMs,
                self.budgets.max_tree_sitter_wall_time_ms_per_artifact,
            )
        } else {
            (
                ExtractionResource::StructuredWallTimeMs,
                self.budgets.max_structured_wall_time_ms_per_artifact,
            )
        };
        if observed > maximum {
            return Err(self.exceeded(resource, observed, maximum));
        }
        Ok(())
    }

    /// Creates a JSON writer that refuses the first byte beyond the output ceiling.
    #[must_use]
    pub fn bounded_json_writer(&self) -> BoundedJsonWriter {
        BoundedJsonWriter {
            bytes: Vec::new(),
            maximum: self.budgets.max_serialized_output_bytes_per_artifact,
            exceeded: None,
        }
    }

    /// Converts an output-writer overflow into the invocation's typed limit error.
    #[must_use]
    pub fn output_limit_error(
        &self,
        writer: &BoundedJsonWriter,
    ) -> Option<ExtractionLimitExceeded> {
        writer.exceeded.map(|observed| {
            self.exceeded(
                ExtractionResource::SerializedOutputBytes,
                observed,
                self.budgets.max_serialized_output_bytes_per_artifact,
            )
        })
    }
}

/// JSON sink that never retains bytes beyond its configured maximum.
#[derive(Debug)]
pub struct BoundedJsonWriter {
    bytes: Vec<u8>,
    maximum: u64,
    exceeded: Option<u64>,
}

impl BoundedJsonWriter {
    /// Returns the bounded serialized bytes.
    #[must_use]
    pub fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let current = u64::try_from(self.bytes.len()).unwrap_or(u64::MAX);
        let amount = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        let observed = current.saturating_add(amount);
        if observed > self.maximum {
            self.exceeded = Some(observed);
            return Err(io::Error::other(
                "serialized extraction output exceeds its limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FixedClock(Duration);

    impl ExtractionClock for FixedClock {
        fn elapsed(&self) -> Duration {
            self.0
        }
    }

    fn budgets_with_small_limits() -> ExtractionBudgets {
        ExtractionBudgets {
            max_input_bytes_per_artifact: 2,
            max_structural_depth_per_artifact: 2,
            max_ast_depth_per_artifact: 2,
            max_work_units_per_artifact: 2,
            max_tree_sitter_nodes_per_artifact: 2,
            max_observations_per_artifact: 2,
            max_accumulated_string_bytes_per_artifact: 2,
            max_serialized_output_bytes_per_artifact: 2,
            max_string_bytes_per_value: 2,
            max_portable_path_bytes_per_value: 2,
            max_identifier_bytes_per_value: 2,
            max_structured_wall_time_ms_per_artifact: 2,
            max_tree_sitter_wall_time_ms_per_artifact: 2,
        }
    }

    #[test]
    fn effective_fingerprint_should_ignore_absent_vs_explicit_defaults() {
        let defaults = ExtractionBudgets::default();
        let explicit = ExtractionBudgets::resolve(Some(&ExtractionBudgetOverrides {
            max_input_bytes_per_artifact: Some(DEFAULT_MAX_INPUT_BYTES_PER_ARTIFACT),
            ..ExtractionBudgetOverrides::default()
        }))
        .expect("default override should be valid");

        assert_eq!(defaults.fingerprint(), explicit.fingerprint());
    }

    #[test]
    fn tracker_should_accept_exact_work_limit_and_reject_one_above() {
        let budgets = ExtractionBudgets::resolve(Some(&ExtractionBudgetOverrides {
            max_work_units_per_artifact: Some(2),
            ..ExtractionBudgetOverrides::default()
        }))
        .expect("override should be valid");
        let mut tracker = ExtractionTracker::new("a.graphql", "graphql", &budgets);

        assert!(tracker.charge_work(2).is_ok());
        assert!(matches!(
            tracker.charge_work(1),
            Err(ExtractionLimitExceeded {
                resource: ExtractionResource::WorkUnits,
                observed: 3,
                maximum: 2,
                ..
            })
        ));
    }

    #[test]
    fn direct_size_and_depth_checks_should_accept_below_and_exact_but_reject_above() {
        let tracker = ExtractionTracker::new("a", "structured", &budgets_with_small_limits());
        for check in [
            ExtractionTracker::check_input_bytes,
            ExtractionTracker::check_structural_depth,
            ExtractionTracker::check_ast_depth,
        ] {
            assert!(check(&tracker, 1).is_ok());
            assert!(check(&tracker, 2).is_ok());
            assert!(check(&tracker, 3).is_err());
        }
    }

    #[test]
    fn cumulative_counters_should_charge_before_accepting_materialization() {
        let budgets = budgets_with_small_limits();

        let mut work = ExtractionTracker::new("a", "work", &budgets);
        assert!(work.charge_work(1).is_ok());
        assert!(work.charge_work(1).is_ok());
        assert!(matches!(
            work.charge_work(1),
            Err(ExtractionLimitExceeded {
                resource: ExtractionResource::WorkUnits,
                observed: 3,
                maximum: 2,
                ..
            })
        ));

        let mut nodes = ExtractionTracker::new("a", "tree-sitter", &budgets);
        assert!(nodes.charge_tree_sitter_node(1).is_ok());
        assert!(nodes.charge_tree_sitter_node(2).is_ok());
        assert!(matches!(
            nodes.charge_tree_sitter_node(2),
            Err(ExtractionLimitExceeded {
                resource: ExtractionResource::TreeSitterNodes,
                observed: 3,
                maximum: 2,
                ..
            })
        ));

        let mut observations = ExtractionTracker::new("a", "facts", &budgets);
        assert!(observations.charge_observation(1).is_ok());
        assert!(observations.charge_observation(1).is_ok());
        assert!(matches!(
            observations.charge_observation(1),
            Err(ExtractionLimitExceeded {
                resource: ExtractionResource::Observations,
                observed: 3,
                maximum: 2,
                ..
            })
        ));

        let mut strings = ExtractionTracker::new("a", "strings", &budgets);
        assert!(strings.charge_string("a").is_ok());
        assert!(strings.charge_string("b").is_ok());
        assert!(matches!(
            strings.charge_string("c"),
            Err(ExtractionLimitExceeded {
                resource: ExtractionResource::AccumulatedStringBytes,
                observed: 3,
                maximum: 2,
                ..
            })
        ));
    }

    #[test]
    fn per_value_limits_should_accept_below_and_exact_but_reject_above() {
        let budgets = budgets_with_small_limits();
        let checks = [
            ExtractionTracker::charge_string,
            ExtractionTracker::charge_portable_path,
            ExtractionTracker::charge_identifier,
        ];
        for check in checks {
            assert!(check(&mut ExtractionTracker::new("a", "value", &budgets), "a").is_ok());
            assert!(check(&mut ExtractionTracker::new("a", "value", &budgets), "ab").is_ok());
            assert!(check(&mut ExtractionTracker::new("a", "value", &budgets), "abc").is_err());
        }
    }

    #[test]
    fn bounded_writer_should_never_retain_a_byte_above_the_limit() {
        let tracker = ExtractionTracker::new("a", "json", &budgets_with_small_limits());
        let mut writer = tracker.bounded_json_writer();
        assert!(writer.write_all(b"a").is_ok());
        assert!(writer.write_all(b"b").is_ok());
        assert!(writer.write_all(b"c").is_err());
        assert!(matches!(
            tracker.output_limit_error(&writer),
            Some(ExtractionLimitExceeded {
                resource: ExtractionResource::SerializedOutputBytes,
                observed: 3,
                maximum: 2,
                ..
            })
        ));
        assert_eq!(writer.into_inner(), b"ab");
    }

    #[test]
    fn monotonic_time_should_sample_every_1024_units_and_preserve_counter_precedence() {
        let mut budgets = ExtractionBudgets {
            max_work_units_per_artifact: 2_048,
            max_structured_wall_time_ms_per_artifact: 5,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::with_clock(
            "a",
            "structured",
            &budgets,
            Box::new(FixedClock(Duration::from_millis(6))),
        );
        assert!(tracker.charge_work(1_023).is_ok());
        assert!(matches!(
            tracker.charge_work(1),
            Err(ExtractionLimitExceeded {
                resource: ExtractionResource::StructuredWallTimeMs,
                observed: 6,
                maximum: 5,
                ..
            })
        ));

        budgets.max_work_units_per_artifact = 1_023;
        let mut deterministic = ExtractionTracker::with_clock(
            "a",
            "structured",
            &budgets,
            Box::new(FixedClock(Duration::from_millis(6))),
        );
        assert!(matches!(
            deterministic.charge_work(1_024),
            Err(ExtractionLimitExceeded {
                resource: ExtractionResource::WorkUnits,
                observed: 1_024,
                maximum: 1_023,
                ..
            })
        ));
    }

    #[test]
    fn wall_time_should_accept_exact_millisecond_limit_and_reject_one_above() {
        let budgets = budgets_with_small_limits();
        let exact = ExtractionTracker::with_clock(
            "a",
            "structured",
            &budgets,
            Box::new(FixedClock(Duration::from_millis(2))),
        );
        let above = ExtractionTracker::with_clock(
            "a",
            "tree-sitter",
            &budgets,
            Box::new(FixedClock(Duration::from_millis(3))),
        );
        assert!(exact.check_structured_time().is_ok());
        assert!(matches!(
            above.check_tree_sitter_time(),
            Err(ExtractionLimitExceeded {
                resource: ExtractionResource::TreeSitterWallTimeMs,
                observed: 3,
                maximum: 2,
                ..
            })
        ));
    }

    #[test]
    fn trackers_should_reset_between_files_and_extractors() {
        let budgets = ExtractionBudgets {
            max_work_units_per_artifact: 1,
            ..ExtractionBudgets::default()
        };
        for (artifact, extractor) in [
            ("one.graphql", "graphql"),
            ("two.graphql", "graphql"),
            ("one.graphql", "tree-sitter"),
        ] {
            let mut tracker = ExtractionTracker::new(artifact, extractor, &budgets);
            assert!(tracker.charge_work(1).is_ok());
        }
    }
}
