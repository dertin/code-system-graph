fn schema_value(
    schema: &serde_json::Value,
    root: &serde_json::Value,
    depth: usize,
) -> serde_json::Value {
    assert!(
        depth < 64,
        "fixture schema should not require unbounded recursion"
    );
    if let Some(reference) = schema.get("$ref").and_then(serde_json::Value::as_str) {
        let mut target = root;
        for component in reference.trim_start_matches("#/").split('/') {
            let component = component.replace("~1", "/").replace("~0", "~");
            target = &target[&component];
        }
        return schema_value(target, root, depth + 1);
    }
    if let Some(value) = schema.get("const") {
        return value.clone();
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .and_then(|values| values.first())
    {
        return value.clone();
    }
    for alternatives in ["oneOf", "anyOf"] {
        if let Some(values) = schema
            .get(alternatives)
            .and_then(serde_json::Value::as_array)
        {
            let selected = values
                .iter()
                .find(|value| {
                    value.get("type") != Some(&serde_json::Value::String("null".to_owned()))
                })
                .unwrap_or(&values[0]);
            return schema_value(selected, root, depth + 1);
        }
    }
    if let Some(value) = schema.get("default") {
        return value.clone();
    }
    if let Some(types) = schema.get("type").and_then(serde_json::Value::as_array) {
        let selected = types
            .iter()
            .find(|value| value.as_str() != Some("null"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("null");
        let mut selected_schema = schema.clone();
        selected_schema["type"] = serde_json::Value::String(selected.to_owned());
        return schema_value(&selected_schema, root, depth + 1);
    }
    match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("object") => {
            let properties = schema
                .get("properties")
                .and_then(serde_json::Value::as_object);
            properties
                .into_iter()
                .flatten()
                .map(|name| (name.0.to_owned(), schema_value(name.1, root, depth + 1)))
                .collect::<serde_json::Map<_, _>>()
                .into()
        }
        Some("array") => schema
            .get("items")
            .map(|items| vec![schema_value(items, root, depth + 1)])
            .unwrap_or_default()
            .into(),
        Some("string") => serde_json::Value::String("fixture".to_owned()),
        Some("integer") => serde_json::json!(1),
        Some("number") => serde_json::json!(1.0),
        Some("boolean") => serde_json::Value::Bool(false),
        Some("null") => serde_json::Value::Null,
        _ => serde_json::Value::Object(serde_json::Map::new()),
    }
}

fn envelope<T: serde::de::DeserializeOwned>(
    name: &str,
) -> code_system_graph_model::ToolEnvelope<T> {
    let catalog = mcp_support::schema_catalog();
    let schema = &catalog["schemas"][name];
    let mut value = schema_value(schema, schema, 0);
    let data_schema = &schema["properties"]["data"];
    value["data"] = schema_value(data_schema, schema, 0);
    value["status"] = serde_json::json!("ok");
    let fixture = value.clone();
    let envelope: code_system_graph_model::ToolEnvelope<T> = serde_json::from_value(value)
        .unwrap_or_else(|error| {
            panic!("generated real typed fixture {name} failed: {error}; {fixture}")
        });
    assert!(
        envelope.data.is_some(),
        "{name} fixture must contain report data: {fixture}"
    );
    envelope
}

fn golden_fingerprint(name: &str) -> &'static str {
    match name {
        "trace.result" => {
            "mcp-markdown-golden:ff3e0e1d64e278a8df3ee2ca2562662ea7f72cea21ff8081c787989bfe023b77"
        }
        "query.result" => {
            "mcp-markdown-golden:77ff3d78b5ad7075229a96ecd54e493c73599e9d7e4a9ac1df00dd825f1e0cdb"
        }
        "explore.result" => {
            "mcp-markdown-golden:a9413e442659f3ea76c434c7bb58a08918856e630e6ad30f5727e8dde8ad32d8"
        }
        "communities.result" => {
            "mcp-markdown-golden:5a1ef5125a028637658d28ca62b9166af11bf31dc73e2dd857a322ed5cea5b56"
        }
        "impact.result" => {
            "mcp-markdown-golden:94149d2320df0d0a26b7b396b3ec663ac3763deb8b58a678101efc093214d0da"
        }
        "analyze_changes.result" => {
            "mcp-markdown-golden:2b4db983ec52fd967c491915d8f344b25134dccaa2eb8885206c598a9550900b"
        }
        "analyze_pull_request.result" => {
            "mcp-markdown-golden:bf108cc2c6d7faa94be192110cad226f8c37bc75a0cdd1c55a822fa0e70633bc"
        }
        "status.result" => {
            "mcp-markdown-golden:3af15ee97798f51ac2a4e2604d02d91a48f25963758d95347240d0df5baf4a61"
        }
        "contracts.result" => {
            "mcp-markdown-golden:ab73b99fabe2c87f7c61c1a588311c690a9b15fe48d2817b5182cce245981b19"
        }
        "source_context.result" => {
            "mcp-markdown-golden:f845b69a6bad8ff15e647167657992e43d7c00a7516ce2b8f8eec99fac3a060c"
        }
        "scan.result" => {
            "mcp-markdown-golden:f765efe1f85cb3f5bff0e82ea8cf57932f15e234f31f53aa5a6e708360620659"
        }
        "update_workspace.result" => {
            "mcp-markdown-golden:db792a79b966c5de817b846801f36020df1cbf9913bdc545a82e64ec156d70fd"
        }
        "write_manual_link.result" => {
            "mcp-markdown-golden:bcc6350ac77602746acf02b06dfeaa6e892b81de3583cc7327bbe905744e3c90"
        }
        "clean_cache.result" => {
            "mcp-markdown-golden:6b9d3ffa7ddc57985ff3218911602e16ff586ec8a68437a47a605b05b5565623"
        }
        "recompute_communities.result" => {
            "mcp-markdown-golden:0e9913c024e667c3aca870f23bb8c4dbc23577f0ac2e01d103bf0832b2438f3a"
        }
        other => panic!("missing Markdown golden for {other}"),
    }
}

macro_rules! assert_golden {
    ($variant:ident, $report:ty, $schema:literal, $heading:literal, $field:literal) => {{
        let envelope = envelope::<$report>($schema);
        let (markdown, is_error) = AgentToolResult::$variant(&envelope).render(1_048_576);
        let expected_prefix = concat!("# ", $heading, "\n\n## Status\n\n- State: `ok`");
        assert!(markdown.starts_with(expected_prefix), "{markdown}");
        assert!(markdown.contains($field), "{}: {markdown}", $schema);
        assert_eq!(
            code_system_graph_model::stable_id("mcp-markdown-golden", &markdown),
            golden_fingerprint($schema),
            "{} full Markdown changed",
            $schema
        );
        assert!(!is_error);
    }};
}

#[test]
fn read_only_tools_should_match_full_markdown_goldens() {
    assert_golden!(
        Trace,
        code_system_graph_model::TraceReport,
        "trace.result",
        "trace",
        "## Segments"
    );
    assert_golden!(
        Query,
        code_system_graph_core::SearchReport,
        "query.result",
        "query",
        "## Hits"
    );
    assert_golden!(
        Explore,
        crate::ExploreReport,
        "explore.result",
        "explore",
        "## Repository"
    );
    assert_golden!(
        Communities,
        crate::CommunityReport,
        "communities.result",
        "communities",
        "## Snapshot"
    );
    assert_golden!(
        Impact,
        code_system_graph_core::ImpactReport,
        "impact.result",
        "impact",
        "## Risk model version"
    );
    assert_golden!(
        AnalyzeChanges,
        code_system_graph_core::ChangeImpactReport,
        "analyze_changes.result",
        "analyze changes",
        "## Analyzer version"
    );
    assert_golden!(
        AnalyzePullRequest,
        code_system_graph_core::PullRequestInspection,
        "analyze_pull_request.result",
        "analyze pull request",
        "## Coordinates"
    );
    assert_golden!(
        Status,
        mcp_support::GraphStatusReport,
        "status.result",
        "status",
        "## Workspace"
    );
    assert_golden!(
        Contracts,
        code_system_graph_core::ContractReport,
        "contracts.result",
        "contracts",
        "## Result version"
    );
    assert_golden!(
        SourceContext,
        mcp_support::SourceContextReport,
        "source_context.result",
        "source context",
        "## Workspace"
    );
}

#[test]
fn administrative_tools_should_match_full_markdown_goldens() {
    assert_golden!(
        Scan,
        mcp_support::AdminAudit<crate::ScanSummary>,
        "scan.result",
        "scan",
        "## Operation"
    );
    assert_golden!(
        UpdateWorkspace,
        mcp_support::AdminAudit<mcp_support::ManifestAdminReport>,
        "update_workspace.result",
        "update workspace",
        "## Mutation"
    );
    assert_golden!(
        WriteManualLink,
        mcp_support::AdminAudit<mcp_support::ManifestAdminReport>,
        "write_manual_link.result",
        "write manual link",
        "## Mutation"
    );
    assert_golden!(
        CleanCache,
        mcp_support::AdminAudit<mcp_support::CacheCleanReport>,
        "clean_cache.result",
        "clean cache",
        "## Removed entries"
    );
    assert_golden!(
        RecomputeCommunities,
        mcp_support::AdminAudit<crate::ScanSummary>,
        "recompute_communities.result",
        "recompute communities",
        "## Operation"
    );
}

#[test]
fn compact_error_should_report_absent_coverage() {
    let error = code_system_graph_model::ToolEnvelope::<code_system_graph_core::SearchReport> {
        schema_version: 2,
        status: code_system_graph_model::ToolStatus::Error,
        data: None,
        freshness: code_system_graph_model::FreshnessSummary {
            overall: code_system_graph_model::OverallFreshness::Unknown,
            stale_repositories: Vec::new(),
            reasons: vec!["fixture".to_owned()],
        },
        warnings: vec!["typed fixture".to_owned()],
    };
    let (compact, is_error) = AgentToolResult::Query(&error).render(256);
    assert!(compact.contains("coverage=absent"), "{compact}");
    assert!(is_error);
}
use super::{self as mcp_support, AgentToolResult};
