#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use code_system_graph_core::ExecutionPolicy;
use code_system_graph_store_sqlite::SqliteStore;
use rmcp::ServerHandler;
use rmcp::handler::server::wrapper::Parameters;

use super::mcp_support::{ContractsInput, ContractsOperation};
use super::{CodeSystemGraphServer, mcp_support};
#[cfg(unix)]
use crate::{CODEGRAPH_DISABLED_CODE, ExploreInput};
use crate::{QueryActionCapabilities, SearchInput, scan_workspace, search_workspace_for_delivery};

fn call_text(result: &rmcp::model::CallToolResult) -> &str {
    match result.content.first() {
        Some(rmcp::model::ContentBlock::Text(content)) => &content.text,
        _ => panic!("tool result did not contain Markdown text"),
    }
}

#[test]
fn server_should_publish_read_only_tools() {
    let server = CodeSystemGraphServer::new(PathBuf::from("graph.db"), "commerce".to_owned())
        .with_codegraph(true, None);
    let tools = server.tool_router.list_all();
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        vec![
            "analyze_changes",
            "analyze_pull_request",
            "communities",
            "contracts",
            "explore",
            "impact",
            "query",
            "source_context",
            "status",
            "trace"
        ]
    );
    assert!(tools.iter().all(|tool| {
        tool.annotations.as_ref().is_some_and(|annotations| {
            annotations.read_only_hint == Some(true) && annotations.destructive_hint == Some(false)
        })
    }));
    assert!(tools.iter().all(|tool| tool.output_schema.is_none()));
    let impact = tools
        .iter()
        .find(|tool| tool.name == "impact")
        .expect("impact tool");
    let description = impact.description.as_deref().expect("impact description");
    assert!(description.contains("{\"node_id\":\"node:...\"}"));
    assert!(description.contains("{\"stable_key\":\"table:::payments\"}"));
}

#[test]
fn explore_should_be_hidden_until_codegraph_is_enabled() {
    let disabled = CodeSystemGraphServer::new(PathBuf::from("graph.db"), "commerce".to_owned());
    let enabled = disabled.clone().with_codegraph(true, None);

    assert!(
        disabled
            .tool_router
            .list_all()
            .iter()
            .all(|tool| tool.name != "explore")
    );
    assert!(
        enabled
            .tool_router
            .list_all()
            .iter()
            .any(|tool| tool.name == "explore")
    );
}

#[tokio::test]
async fn query_actions_should_match_mcp_codegraph_capability()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml");
    scan_workspace(&manifest, &database)?;
    let disabled = CodeSystemGraphServer::new(database.clone(), "commerce-platform".to_owned());
    let enabled = disabled.clone().with_codegraph(true, None);
    let input = |query: &str| SearchInput {
        query: query.to_owned(),
        node_kinds: Vec::new(),
        repo_ids: Vec::new(),
        service_ids: Vec::new(),
        community_ids: Vec::new(),
        offset: 0,
        limit: 5,
    };

    let hit = disabled.query(Parameters(input("orders"))).await;
    assert!(hit.structured_content.as_ref().is_some_and(|content| {
        content["data"]["next_actions"]
            .as_array()
            .is_some_and(|actions| {
                actions
                    .iter()
                    .any(|action| action["tool"] == "source_context")
            })
    }));

    let missing = disabled
        .query(Parameters(input("api source_literal_that_does_not_exist")))
        .await;
    assert!(!missing.structured_content.as_ref().is_some_and(|content| {
        content["data"]["next_actions"]
            .as_array()
            .is_some_and(|actions| actions.iter().any(|action| action["tool"] == "explore"))
    }));

    let enabled_missing = enabled
        .query(Parameters(input("api source_literal_that_does_not_exist")))
        .await;
    assert!(
        enabled_missing
            .structured_content
            .as_ref()
            .is_some_and(|content| {
                content["data"]["next_actions"]
                    .as_array()
                    .is_some_and(|actions| actions.iter().any(|action| action["tool"] == "explore"))
            })
    );
    Ok(())
}

#[tokio::test]
async fn presentation_context_should_be_cached_by_arc_and_fail_closed_across_snapshots()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml");
    scan_workspace(&manifest, &database)?;
    let server = CodeSystemGraphServer::new(database.clone(), "commerce-platform".to_owned());
    let snapshot = SqliteStore::open_read_only(&database)?
        .current_snapshot_summary("commerce-platform")?
        .snapshot_id;
    let first = server.presentation_context_for_snapshot(&snapshot).await;
    let second = server.presentation_context_for_snapshot(&snapshot).await;
    assert!(std::sync::Arc::ptr_eq(&first, &second));

    let input = SearchInput {
        query: "orders".to_owned(),
        node_kinds: Vec::new(),
        repo_ids: Vec::new(),
        service_ids: Vec::new(),
        community_ids: Vec::new(),
        offset: 0,
        limit: 5,
    };
    let envelope = search_workspace_for_delivery(
        &database,
        "commerce-platform",
        &input,
        &ExecutionPolicy::default(),
        QueryActionCapabilities {
            source_context: true,
            explore: false,
        },
    )?;
    let result = server
        .contextual_markdown_result(
            mcp_support::AgentToolResult::Query(&envelope),
            Some("snapshot:replaced".to_owned()),
        )
        .await;

    assert!(
        call_text(&result).contains("snapshot changed"),
        "{}",
        call_text(&result)
    );
    assert!(result.structured_content.as_ref().is_some_and(|content| {
        content["status"] == "degraded"
            && content["warnings"].as_array().is_some_and(|warnings| {
                warnings.iter().any(|warning| {
                    warning
                        .as_str()
                        .is_some_and(|warning| warning.contains("snapshot changed"))
                })
            })
    }));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_presentation_cache_misses_should_publish_one_canonical_arc()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml");
    scan_workspace(&manifest, &database)?;
    let server = CodeSystemGraphServer::new(database.clone(), "commerce-platform".to_owned());
    let snapshot = SqliteStore::open_read_only(&database)?
        .current_snapshot_summary("commerce-platform")?
        .snapshot_id;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let handles: [_; 8] = std::array::from_fn(|_| {
        let server = server.clone();
        let snapshot = snapshot.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            server.presentation_context_for_snapshot(&snapshot).await
        })
    });
    let mut contexts = Vec::new();
    for handle in handles {
        contexts.push(handle.await.expect("context loader task"));
    }

    assert!(
        contexts[1..]
            .iter()
            .all(|context| std::sync::Arc::ptr_eq(&contexts[0], context))
    );
    assert_eq!(server.presentation_cache.lock().await.load_count, 1);
    Ok(())
}

#[tokio::test]
async fn contract_list_should_render_direct_repository_aliases_without_full_context()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml");
    scan_workspace(&manifest, &database)?;
    let server = CodeSystemGraphServer::new(database, "commerce-platform".to_owned());

    let result = server
        .contracts(Parameters(ContractsInput {
            workspace: None,
            operation: ContractsOperation::List { limit: 10 },
        }))
        .await;
    let markdown = call_text(&result);

    assert!(markdown.contains("repository `api`"), "{markdown}");
    assert_eq!(server.presentation_cache.lock().await.load_count, 0);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn explore_handler_should_reject_disabled_codegraph_before_execution()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml");
    scan_workspace(&manifest, &database)?;
    let binary = temporary.path().join("codegraph-marker");
    let marker = temporary.path().join("codegraph-invoked");
    std::fs::write(
        &binary,
        "#!/bin/sh\n: > \"$(dirname \"$0\")/codegraph-invoked\"\nexit 1\n",
    )?;
    let mut permissions = std::fs::metadata(&binary)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions)?;
    let server = CodeSystemGraphServer::new(database, "commerce-platform".to_owned())
        .with_codegraph(false, Some(binary.into_os_string()));

    let result = server
        .explore(Parameters(ExploreInput {
            workspace: Some("commerce-platform".to_owned()),
            repository: Some("orders".to_owned()),
            query: "create_order callers".to_owned(),
            max_files: Some(4),
        }))
        .await;
    let Err(error) = result else {
        panic!("disabled CodeGraph policy must reject direct handler calls");
    };

    assert_eq!(
        (
            error.data.as_ref().and_then(|data| data["code"].as_str()),
            marker.exists()
        ),
        (Some(CODEGRAPH_DISABLED_CODE), false)
    );
    Ok(())
}

#[test]
fn initialize_contract_should_align_instructions_with_advertised_tools() {
    let disabled = CodeSystemGraphServer::new(PathBuf::from("graph.db"), "commerce".to_owned());
    let enabled = disabled.clone().with_codegraph(true, None);
    let disabled_info = disabled.get_info();

    assert!(disabled_info.capabilities.tools.is_some());
    assert!(disabled_info.capabilities.resources.is_some());
    assert_eq!(disabled_info.server_info.name, "code_system_graph");

    for (server, codegraph_enabled) in [(&disabled, false), (&enabled, true)] {
        let explore_advertised = server
            .tool_router
            .list_all()
            .iter()
            .any(|tool| tool.name == "explore");
        let instructions = server
            .get_info()
            .instructions
            .expect("server instructions should be present");

        assert_eq!(explore_advertised, codegraph_enabled);
        assert_eq!(instructions.contains("explore"), codegraph_enabled);
        assert!(instructions.contains("semantic Markdown"));
        assert!(instructions.contains("structuredContent"));
        assert!(instructions.contains("schema version 5"));
        assert!(!instructions.contains("versioned JSON"));
    }

    let source_context = disabled
        .tool_router
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "source_context")
        .expect("source_context should be advertised");
    let description = source_context
        .description
        .as_deref()
        .expect("source_context description should be present");
    assert!(!description.contains("explore"));
}

#[test]
fn admin_tools_should_be_hidden_until_constructor_profile_is_enabled() {
    let hidden = CodeSystemGraphServer::new(PathBuf::from("graph.db"), "commerce".to_owned());
    assert!(
        hidden
            .tool_router
            .list_all()
            .iter()
            .all(|tool| !mcp_support::ADMIN_TOOL_NAMES.contains(&tool.name.as_ref()))
    );

    let enabled = hidden.with_admin_profile(true);
    for name in mcp_support::ADMIN_TOOL_NAMES {
        let tool = enabled
            .tool_router
            .list_all()
            .into_iter()
            .find(|tool| tool.name == name);
        assert!(tool.is_some());
        assert!(tool.is_some_and(|tool| {
            tool.annotations.as_ref().is_some_and(|annotations| {
                annotations.read_only_hint == Some(false)
                    && annotations.destructive_hint == Some(true)
            })
        }));
    }
}

#[test]
fn resource_list_should_publish_stable_workspace_policy_uris() {
    let uris = mcp_support::resource_uris("commerce")
        .into_iter()
        .map(|(uri, _, _)| uri)
        .collect::<Vec<_>>();

    assert_eq!(uris.len(), 9);
    assert!(uris.contains(&"code-system-graph://workspaces".to_owned()));
    assert!(uris.contains(&"code-system-graph://workspace/commerce/schema".to_owned()));
    assert!(!uris.iter().any(|uri| uri.contains("{id}")));

    let templates = mcp_support::resource_templates();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].0, "code-system-graph://evidence/{id}");
}

#[test]
fn resource_read_should_reject_wrong_workspace_before_store_access() {
    let error = mcp_support::read_resource(
        &PathBuf::from("missing.db"),
        "commerce",
        "code-system-graph://workspace/payments/status",
        &ExecutionPolicy::default(),
    );

    assert!(error.is_err());
    assert!(error.is_err_and(|error| {
        error.kind == mcp_support::ResourceErrorKind::Invalid
            && error.message.contains("outside this server's configured")
    }));
}

#[test]
fn resource_read_should_return_versioned_source_free_markdown()
-> Result<(), Box<dyn std::error::Error>> {
    let text = mcp_support::read_resource(
        &PathBuf::from("missing.db"),
        "commerce",
        "code-system-graph://workspaces",
        &ExecutionPolicy::default(),
    )
    .map_err(|error| std::io::Error::other(error.message))?;
    assert!(text.starts_with("# Configured workspaces"));
    assert!(text.contains("commerce"));
    assert!(text.contains("Delivery schema"));
    assert!(!text.contains("source_body"));
    Ok(())
}

#[test]
fn evidence_template_read_should_require_concrete_identifier() {
    let error = mcp_support::read_resource(
        &PathBuf::from("missing.db"),
        "commerce",
        "code-system-graph://evidence/{id}",
        &ExecutionPolicy::default(),
    );

    assert!(error.is_err_and(|error| {
        error.kind == mcp_support::ResourceErrorKind::Invalid
            && error.message.contains("concrete evidence identifier")
    }));
}

#[test]
fn evidence_read_should_report_missing_metadata() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    drop(SqliteStore::open(&database)?);

    let error = mcp_support::read_resource(
        &database,
        "commerce",
        "code-system-graph://evidence/evidence:missing",
        &ExecutionPolicy::default(),
    );

    assert!(error.is_err_and(|error| {
        error.kind == mcp_support::ResourceErrorKind::Missing
            && error.message.contains("no evidence snapshot")
    }));
    Ok(())
}

#[test]
fn schema_resource_should_catalog_every_tool_input_and_result()
-> Result<(), Box<dyn std::error::Error>> {
    let catalog = mcp_support::schema_catalog();
    let schemas = catalog
        .get("schemas")
        .and_then(serde_json::Value::as_object);

    assert!(schemas.is_some());
    let schemas = schemas.map_or(0, serde_json::Map::len);
    assert_eq!(schemas, 30);
    for name in [
        "explore.input",
        "explore.result",
        "update_workspace.input",
        "update_workspace.result",
        "write_manual_link.input",
        "write_manual_link.result",
        "clean_cache.input",
        "clean_cache.result",
    ] {
        assert!(catalog["schemas"].get(name).is_some(), "missing {name}");
    }
    assert_eq!(catalog["schema_version"], 5);
    assert!(catalog["application_interfaces"]["schemas"].is_array());
    assert!(serde_json::to_vec(&catalog)?.len() <= 2 * 1024 * 1024);
    Ok(())
}

#[test]
fn tool_schemas_should_keep_codegraph_policy_out_of_llm_inputs() {
    let catalog = mcp_support::schema_catalog();
    let impact = catalog["schemas"]["impact.input"].to_string();
    let scan = catalog["schemas"]["scan.input"].to_string();

    assert!(!impact.contains("local_enrichment"));
    assert!(!scan.contains("codegraph"));
    assert!(!scan.contains("codegraph_binary"));
}

#[test]
fn conditional_tool_inputs_should_require_action_specific_fields() {
    let contract: Result<mcp_support::ContractsInput, _> =
        serde_json::from_value(serde_json::json!({
            "action": "diff",
            "workspace": "commerce",
            "contract": "contract:a"
        }));
    let workspace_update: Result<mcp_support::WorkspaceUpdateInput, _> =
        serde_json::from_value(serde_json::json!({
            "action": "remove_repository",
            "workspace": "commerce",
            "alias": "api",
            "repository_path": "api"
        }));
    let manual_link: Result<mcp_support::ManualLinkWriteInput, _> =
        serde_json::from_value(serde_json::json!({
            "workspace": "commerce",
            "from": "a",
            "to": "b",
            "relation": "documents",
            "reason": "Operator decision"
        }));
    let communities: Result<mcp_support::CommunitiesInput, _> =
        serde_json::from_value(serde_json::json!({
            "action": "show",
            "workspace": "commerce",
            "community_id": "community:a",
            "snapshot_id": "snapshot:old"
        }));

    assert!(contract.is_err());
    assert!(workspace_update.is_err());
    assert!(manual_link.is_err());
    assert!(communities.is_err());
}

#[test]
fn contract_runtime_bounds_should_reject_values_beyond_the_advertised_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let input: mcp_support::ContractsInput = serde_json::from_value(serde_json::json!({
        "action": "list",
        "limit": 101
    }))?;
    let result = mcp_support::contracts_envelope(
        std::path::Path::new("database-must-not-be-opened.db"),
        "commerce",
        &input,
    );

    assert_eq!(result.status, code_system_graph_model::ToolStatus::Error);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("between 1 and 100"))
    );
    Ok(())
}

#[test]
fn bound_read_only_tools_should_not_require_workspace_repetition() {
    let status = serde_json::from_value::<mcp_support::ReadWorkspaceInput>(serde_json::json!({}));
    let contracts = serde_json::from_value::<mcp_support::ContractsInput>(serde_json::json!({
        "action": "list"
    }));
    let communities = serde_json::from_value::<mcp_support::CommunitiesInput>(serde_json::json!({
        "action": "list"
    }));
    let source_context =
        serde_json::from_value::<mcp_support::SourceContextInput>(serde_json::json!({
            "node_id": "node:a"
        }));
    let explore = serde_json::from_value::<crate::ExploreInput>(serde_json::json!({
        "repository": "api",
        "query": "handler"
    }));
    let administrative =
        serde_json::from_value::<mcp_support::WorkspaceInput>(serde_json::json!({}));

    assert!(status.is_ok());
    assert!(contracts.is_ok());
    assert!(communities.is_ok());
    assert!(source_context.is_ok());
    assert!(explore.is_ok());
    assert!(administrative.is_err());
}
