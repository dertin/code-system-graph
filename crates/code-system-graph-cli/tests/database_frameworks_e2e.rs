//! Cross-repository acceptance tests for shared database-table relationships.

use code_system_graph::scan_workspace;
use code_system_graph_model::{EdgeKind, NodeKind};
use code_system_graph_store_sqlite::SqliteStore;

#[test]
fn sqlx_mysql_async_and_python_should_link_to_one_cross_repo_table() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let schema = temporary.path().join("schema");
    let sqlx_api = temporary.path().join("sqlx-api");
    let mysql_api = temporary.path().join("mysql-api");
    let python_worker = temporary.path().join("python-worker");
    std::fs::create_dir_all(&schema)?;
    std::fs::create_dir_all(sqlx_api.join("src"))?;
    std::fs::create_dir_all(mysql_api.join("src"))?;
    std::fs::create_dir_all(&python_worker)?;

    std::fs::write(
        schema.join("schema.sql"),
        "CREATE TABLE users (id BIGINT PRIMARY KEY, active BOOLEAN);",
    )?;
    write_cargo_package(&sqlx_api, "sqlx-api", "sqlx = \"0.8\"")?;
    std::fs::write(
        sqlx_api.join("src/lib.rs"),
        r#"
async fn load_users(pool: &sqlx::MySqlPool) {
    sqlx::query("SELECT id FROM users").fetch_all(pool).await;
}
"#,
    )?;
    write_cargo_package(&mysql_api, "mysql-api", "mysql_async = \"0.37\"")?;
    std::fs::write(
        mysql_api.join("src/lib.rs"),
        r#"
use mysql_async::prelude::Queryable;

async fn update_users(conn: &mut mysql_async::Conn) {
    conn.exec_drop("UPDATE users SET active = true", ()).await;
}
"#,
    )?;
    std::fs::write(
        python_worker.join("worker.py"),
        r#"
def delete_users(cursor):
    cursor.execute("DELETE FROM users WHERE active = false")
"#,
    )?;

    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        r"
version: 1
name: database-frameworks-e2e
repos:
  schema:
    path: schema
  sqlx-api:
    path: sqlx-api
  mysql-api:
    path: mysql-api
  python-worker:
    path: python-worker
",
    )?;
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (nodes, edges) = store.load_current_graph("database-frameworks-e2e")?;
    let users = nodes
        .iter()
        .filter(|node| node.kind == NodeKind::DatabaseTable && node.label == "users")
        .collect::<Vec<_>>();

    assert_eq!(
        users.len(),
        1,
        "all repositories should share one table node"
    );
    assert!(has_symbol_table_edge(
        &nodes,
        &edges,
        "load_users",
        &users[0].id,
        EdgeKind::ReadsTable,
    ));
    assert!(has_symbol_table_edge(
        &nodes,
        &edges,
        "update_users",
        &users[0].id,
        EdgeKind::WritesTable,
    ));
    assert!(has_symbol_table_edge(
        &nodes,
        &edges,
        "delete_users",
        &users[0].id,
        EdgeKind::WritesTable,
    ));
    Ok(())
}

fn write_cargo_package(
    directory: &std::path::Path,
    name: &str,
    dependency: &str,
) -> anyhow::Result<()> {
    std::fs::write(
        directory.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n[dependencies]\n{dependency}\n"
        ),
    )?;
    Ok(())
}

fn has_symbol_table_edge(
    nodes: &[code_system_graph_model::Node],
    edges: &[code_system_graph_model::Edge],
    symbol: &str,
    table: &code_system_graph_model::NodeId,
    kind: EdgeKind,
) -> bool {
    edges.iter().any(|edge| {
        edge.kind == kind
            && &edge.target == table
            && nodes.iter().any(|node| {
                node.id == edge.source && node.kind == NodeKind::SymbolRef && node.label == symbol
            })
    })
}
