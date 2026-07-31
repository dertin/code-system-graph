//! End-to-end acceptance tests for `SQLx` source, query-file, and incremental graph support.

use code_system_graph::scan_workspace;
use code_system_graph_model::{EdgeKind, NodeKind};
use code_system_graph_store_sqlite::SqliteStore;

#[test]
fn sqlx_query_file_change_should_relink_calling_symbol_without_rust_changes() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(repository.join("src"))?;
    std::fs::create_dir_all(repository.join("queries"))?;
    std::fs::create_dir_all(repository.join("db/migrations-v1"))?;
    std::fs::create_dir_all(repository.join("db/migrations-v2"))?;
    std::fs::write(
        repository.join("Cargo.toml"),
        r#"
[package]
name = "sqlx-api"
version = "1.0.0"
edition = "2024"

[dependencies]
sqlx = { version = "0.8", features = ["postgres", "macros", "migrate"] }
"#,
    )?;
    std::fs::write(
        repository.join("src/users.rs"),
        r#"
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

async fn load(pool: &sqlx::PgPool) {
    sqlx::query_file!("queries/current.sql")
        .fetch_all(pool)
        .await;
}
"#,
    )?;
    std::fs::write(
        repository.join("db/migrations-v1/0001_tables.sql"),
        "CREATE TABLE users (id INTEGER); CREATE TABLE teams (id INTEGER);",
    )?;
    std::fs::write(
        repository.join("db/migrations-v2/0002_audits.sql"),
        "CREATE TABLE audits (id INTEGER);",
    )?;
    let sqlx_configuration = repository.join("sqlx.toml");
    std::fs::write(
        &sqlx_configuration,
        "[migrate]\nmigrations-dir = \"db/migrations-v1\"\n",
    )?;
    let query = repository.join("queries/current.sql");
    std::fs::write(&query, "SELECT id FROM users;")?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: sqlx-e2e\nrepos:\n  api:\n    path: api\n",
    )?;
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (nodes, edges) = store.load_current_graph("sqlx-e2e")?;
    assert!(has_symbol_table_edge(&nodes, &edges, "load", "users"));
    assert!(has_artifact_consumes(
        &nodes,
        &edges,
        "src/users.rs",
        "db/migrations-v1/0001_tables.sql"
    ));

    std::fs::write(&query, "SELECT id FROM teams;")?;
    std::fs::write(
        &sqlx_configuration,
        "[migrate]\nmigrations-dir = \"db/migrations-v2\"\n",
    )?;
    let second = scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (nodes, edges) = store.load_current_graph("sqlx-e2e")?;

    assert!(
        second.changed_input_count > 0
            && has_symbol_table_edge(&nodes, &edges, "load", "teams")
            && !has_symbol_table_edge(&nodes, &edges, "load", "users")
    );
    assert!(has_artifact_consumes(
        &nodes,
        &edges,
        "src/users.rs",
        "db/migrations-v2/0002_audits.sql"
    ));
    assert!(!has_artifact_consumes(
        &nodes,
        &edges,
        "src/users.rs",
        "db/migrations-v1/0001_tables.sql"
    ));
    assert!(edges.iter().any(|edge| {
        edge.kind == EdgeKind::Consumes
            && nodes.iter().any(|node| {
                node.id == edge.source && node.kind == NodeKind::SymbolRef && node.label == "load"
            })
            && nodes.iter().any(|node| {
                node.id == edge.target
                    && node.kind == NodeKind::Artifact
                    && node.label == "queries/current.sql"
            })
    }));
    Ok(())
}

fn has_artifact_consumes(
    nodes: &[code_system_graph_model::Node],
    edges: &[code_system_graph_model::Edge],
    source: &str,
    target: &str,
) -> bool {
    edges.iter().any(|edge| {
        edge.kind == EdgeKind::Consumes
            && nodes.iter().any(|node| {
                node.id == edge.source && node.kind == NodeKind::Artifact && node.label == source
            })
            && nodes.iter().any(|node| {
                node.id == edge.target && node.kind == NodeKind::Artifact && node.label == target
            })
    })
}

fn has_symbol_table_edge(
    nodes: &[code_system_graph_model::Node],
    edges: &[code_system_graph_model::Edge],
    symbol: &str,
    table: &str,
) -> bool {
    edges.iter().any(|edge| {
        edge.kind == EdgeKind::ReadsTable
            && nodes.iter().any(|node| {
                node.id == edge.source && node.kind == NodeKind::SymbolRef && node.label == symbol
            })
            && nodes.iter().any(|node| {
                node.id == edge.target
                    && node.kind == NodeKind::DatabaseTable
                    && node.label == table
            })
    })
}
