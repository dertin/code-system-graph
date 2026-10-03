use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::{
    LocalNeighbor, ProviderOperation, ProviderOperationCapability, ProviderStatus, ProviderTransport, ResolvedSymbol
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiscoveredTool {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: String,
    pub(crate) input_schema: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusContract {
    pub(crate) initialized: bool,
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) pending_changes: Option<PendingChangesContract>,
    #[serde(default)]
    pub(crate) worktree_mismatch: Option<Value>,
    #[serde(default)]
    pub(crate) index: Option<IndexContract>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PendingChangesContract {
    #[serde(default)]
    added: usize,
    #[serde(default)]
    modified: usize,
    #[serde(default)]
    removed: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IndexContract {
    #[serde(default)]
    reindex_recommended: bool,
    #[serde(default)]
    state: Option<String>,
}

impl StatusContract {
    pub(crate) fn status(&self) -> ProviderStatus {
        if !self.initialized {
            return ProviderStatus::IndexMissing;
        }
        let has_pending_changes = self.pending_changes.as_ref().is_some_and(|changes| {
            changes.added > 0 || changes.modified > 0 || changes.removed > 0
        });
        let stale_index = self.index.as_ref().is_none_or(|index| {
            index.reindex_recommended
                || index
                    .state
                    .as_deref()
                    .is_some_and(|state| !state.is_empty() && state != "complete")
        });
        if has_pending_changes || self.worktree_mismatch.is_some() || stale_index {
            ProviderStatus::Stale
        } else {
            ProviderStatus::Available
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SymbolQueryContract {
    pub(crate) node: SymbolNodeContract,
    #[serde(default)]
    pub(crate) score: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SymbolNodeContract {
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) qualified_name: Option<String>,
    pub(crate) kind: String,
    pub(crate) file_path: String,
    pub(crate) start_line: usize,
}

impl SymbolQueryContract {
    pub(crate) fn into_symbol(self) -> ResolvedSymbol {
        ResolvedSymbol {
            local_id: self.node.id,
            name: self.node.name,
            qualified_name: self.node.qualified_name,
            kind: self.node.kind,
            file_path: self.node.file_path,
            start_line: self.node.start_line,
            score: self.score,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct NeighborContract {
    pub(crate) name: String,
    pub(crate) kind: String,
    #[serde(rename = "filePath")]
    pub(crate) file_path: String,
    #[serde(rename = "startLine")]
    pub(crate) start_line: usize,
}

impl NeighborContract {
    pub(crate) fn into_neighbor(self) -> LocalNeighbor {
        LocalNeighbor {
            name: self.name,
            kind: self.kind,
            file_path: self.file_path,
            start_line: self.start_line,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct NeighborsContract {
    pub(crate) symbol: String,
    #[serde(default)]
    pub(crate) callers: Vec<NeighborContract>,
    #[serde(default)]
    pub(crate) callees: Vec<NeighborContract>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImpactContract {
    pub(crate) symbol: String,
    pub(crate) depth: usize,
    pub(crate) node_count: usize,
    #[serde(default)]
    pub(crate) affected: Vec<NeighborContract>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AffectedTestsContract {
    #[serde(default)]
    pub(crate) changed_files: Vec<String>,
    #[serde(default)]
    pub(crate) affected_tests: Vec<String>,
    #[serde(default)]
    pub(crate) total_dependents_traversed: usize,
}

pub(crate) fn map_mcp_tools(tools: &[DiscoveredTool]) -> Vec<ProviderOperationCapability> {
    let mut operations = BTreeMap::new();
    for tool in tools {
        let parameters = schema_parameters(&tool.input_schema);
        let name = tool.name.to_ascii_lowercase();
        let description = tool.description.to_ascii_lowercase();
        let searchable = format!("{name} {description}");
        let operation = if supports_parameter(&parameters, &["query"])
            && contains_any(&searchable, &["explore", "context", "source"])
        {
            Some(ProviderOperation::LocalContext)
        } else if supports_parameter(&parameters, &["query", "search", "symbol"])
            && contains_any(&searchable, &["query", "search", "resolve", "symbol"])
        {
            Some(ProviderOperation::ResolveSymbols)
        } else if supports_parameter(&parameters, &["symbol", "name"])
            && contains_any(&searchable, &["caller", "callee", "neighbor"])
        {
            Some(ProviderOperation::LocalNeighbors)
        } else if supports_parameter(&parameters, &["symbol", "name"])
            && searchable.contains("impact")
        {
            Some(ProviderOperation::LocalImpact)
        } else if supports_parameter(&parameters, &["files", "changedFiles"])
            && contains_any(&searchable, &["affected", "test"])
        {
            Some(ProviderOperation::AffectedTests)
        } else {
            None
        };
        if let Some(operation) = operation {
            operations
                .entry(operation)
                .or_insert_with(|| ProviderOperationCapability {
                    operation,
                    transport: ProviderTransport::Mcp,
                    public_name: tool.name.clone(),
                    parameters,
                });
        }
    }
    operations.into_values().collect()
}

pub(crate) fn cli_operations(version: &str) -> Vec<ProviderOperationCapability> {
    if !supports_cli_contract(version) {
        return Vec::new();
    }
    [
        (
            ProviderOperation::ResolveSymbols,
            "query",
            &["path", "limit", "json", "search"][..],
        ),
        (
            ProviderOperation::LocalNeighbors,
            "callers/callees",
            &["path", "limit", "json", "symbol"],
        ),
        (
            ProviderOperation::LocalImpact,
            "impact",
            &["path", "depth", "json", "symbol"],
        ),
        (
            ProviderOperation::LocalContext,
            "explore",
            &["path", "max-files", "query"],
        ),
        (
            ProviderOperation::AffectedTests,
            "affected",
            &["path", "depth", "json", "files"],
        ),
    ]
    .into_iter()
    .map(
        |(operation, public_name, parameters)| ProviderOperationCapability {
            operation,
            transport: ProviderTransport::Cli,
            public_name: public_name.to_owned(),
            parameters: parameters.iter().map(|value| (*value).to_owned()).collect(),
        },
    )
    .collect()
}

/// The structured CLI adapter is validated against `CodeGraph` 1.6.1; pre-releases and other minor
/// versions are rejected until their fixtures are captured.
pub(crate) fn supports_cli_contract(version: &str) -> bool {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    major == "1" && minor == "6" && patch.parse::<u32>().is_ok_and(|patch| patch >= 1)
}

fn schema_parameters(schema: &Value) -> Vec<String> {
    let mut parameters = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    parameters.sort();
    parameters
}

fn supports_parameter(parameters: &[String], candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| parameters.iter().any(|parameter| parameter == candidate))
}

fn contains_any(value: &str, candidates: &[&str]) -> bool {
    candidates.iter().any(|candidate| value.contains(candidate))
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{
        AffectedTestsContract, DiscoveredTool, ImpactContract, NeighborsContract, StatusContract, SymbolQueryContract, map_mcp_tools, supports_cli_contract
    };
    use crate::{ProviderOperation, ProviderStatus};

    #[test]
    fn codegraph_1_6_tools_should_map_context_by_name_and_schema() {
        let tools: Vec<DiscoveredTool> = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/1.6.1/tools-list.json"
        ))
        .expect("fixture should be valid");
        let operations = map_mcp_tools(&tools);

        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].operation, ProviderOperation::LocalContext);
        assert!(operations[0].parameters.contains(&"projectPath".to_owned()));
    }

    #[test]
    fn codegraph_1_6_initialize_should_negotiate_the_tested_protocol() {
        let initialize: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/1.6.1/initialize.json"
        ))
        .expect("fixture should be valid");

        assert_eq!(initialize["protocolVersion"], "2024-11-05");
        assert_eq!(initialize["serverInfo"]["version"], "1.6.1");
        assert!(initialize["capabilities"]["tools"].is_object());
    }

    #[test]
    fn codegraph_1_6_cli_json_should_match_the_structured_contracts() {
        let symbols: Vec<SymbolQueryContract> = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/1.6.1/query.json"
        ))
        .expect("query fixture should parse");
        let neighbors: NeighborsContract = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/1.6.1/neighbors.json"
        ))
        .expect("neighbors fixture should parse");
        let impact: ImpactContract = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/1.6.1/impact.json"
        ))
        .expect("impact fixture should parse");
        let affected: AffectedTestsContract = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/1.6.1/affected.json"
        ))
        .expect("affected fixture should parse");

        let symbol = symbols
            .into_iter()
            .next()
            .expect("query fixture should resolve one symbol")
            .into_symbol();
        assert_eq!(
            (
                symbol.name.as_str(),
                symbol.file_path.as_str(),
                symbol.start_line
            ),
            ("scan_workspace", "src/lib.rs", 3)
        );
        assert_eq!(neighbors.symbol, "discover");
        assert_eq!(neighbors.callers.len(), 1);
        assert_eq!(neighbors.callers[0].name, "scan_workspace");
        assert!(neighbors.callees.is_empty());
        assert_eq!((impact.symbol.as_str(), impact.depth), ("discover", 2));
        assert_eq!(impact.node_count, impact.affected.len());
        assert_eq!(affected.changed_files, ["src/provider.ts"]);
        assert_eq!(affected.affected_tests, ["tests/provider.test.ts"]);
        assert_eq!(affected.total_dependents_traversed, 1);
    }

    #[test]
    fn missing_optional_tool_should_not_create_false_capability() {
        let tools: Vec<DiscoveredTool> = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/degraded/missing-optional-tool.json"
        ))
        .expect("fixture should be valid");

        assert_eq!(map_mcp_tools(&tools), Vec::new());
    }

    #[test]
    fn status_contract_should_distinguish_missing_and_stale_indexes() {
        let missing: StatusContract = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/degraded/missing-index-status.json"
        ))
        .expect("fixture should be valid");
        let stale: StatusContract = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/degraded/stale-index-status.json"
        ))
        .expect("fixture should be valid");

        assert_eq!(missing.status(), ProviderStatus::IndexMissing);
        assert_eq!(stale.status(), ProviderStatus::Stale);
    }

    #[test]
    fn status_contract_should_treat_a_null_index_state_as_stale() {
        let status: StatusContract = serde_json::from_str(
            r#"{
                "initialized": true,
                "version": "1.6.1",
                "worktreeMismatch": null,
                "pendingChanges": {"added": 0, "modified": 0, "removed": 0},
                "index": {"reindexRecommended": true, "state": null}
            }"#,
        )
        .expect("live-compatible status should parse");

        assert_eq!(status.status(), ProviderStatus::Stale);
    }

    #[test]
    fn status_contract_should_recognize_current_index() {
        let status: StatusContract = serde_json::from_str(include_str!(
            "../../../../fixtures/codegraph/1.6.1/status.json"
        ))
        .expect("fixture should be valid");

        assert_eq!(status.version.as_deref(), Some("1.6.1"));
        assert_eq!(status.status(), ProviderStatus::Available);
    }

    #[test]
    fn cli_contract_should_accept_only_validated_versions() {
        for version in ["1.6.1", "v1.6.1", "1.6.2", " 1.6.10\n"] {
            assert!(
                supports_cli_contract(version),
                "{version} should be accepted"
            );
        }
        for version in [
            "1.5.0",
            "1.6",
            "1.6.0",
            "1.6.1-rc.1",
            "1.6.1.0",
            "1.7.0",
            "2.6.1",
            "",
        ] {
            assert!(
                !supports_cli_contract(version),
                "{version} should be rejected"
            );
        }
    }
}
