//! Acceptance tests for absolute-URL authority resolution across repositories.

use std::collections::BTreeMap;
use std::path::Path;

use code_system_graph::scan_workspace;
use code_system_graph_model::{EdgeKind, Node, RepoId};
use code_system_graph_store_sqlite::SqliteStore;

const SOURCES: [(&str, &str); 6] = [
    (
        "api/src/server.ts",
        "import express from 'express';\nconst app = express();\nfunction getOrder(req, res) {}\nfunction health(req, res) {}\napp.get('/v1/orders/:id', getOrder);\napp.get('/v1/health', health);\n",
    ),
    (
        "api/docker-compose.yml",
        "services:\n  orders-api:\n    build: .\n    ports:\n      - \"8080:8080\"\n",
    ),
    (
        "billing/src/server.ts",
        "import express from 'express';\nconst app = express();\nfunction readOrder(req, res) {}\napp.get('/v1/orders/:orderId', readOrder);\n",
    ),
    (
        "web/src/client.ts",
        "export async function load() {\n  await fetch('http://orders-api:8080/v1/orders/42');\n  await fetch('https://payments.example.com/v1/orders/42');\n}\n",
    ),
    (
        "tests/tests/test_health.py",
        "import requests\n\ndef test_health():\n    requests.get(\"http://localhost:3000/v1/health\")\n",
    ),
    (
        "tests/tests/test_billing.py",
        "import requests\n\ndef test_billing_order():\n    requests.get(\"http://billing.internal/v1/orders/7\")\n",
    ),
];

const MANIFEST: &str = "version: 1\nname: authorities\nrepos:\n  api:\n    path: api\n  billing:\n    path: billing\n    authorities: [billing.internal]\n  web:\n    path: web\n  tests:\n    path: tests\n";

fn write_workspace(root: &Path) -> anyhow::Result<std::path::PathBuf> {
    for (path, contents) in SOURCES {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
    }
    let manifest = root.join("code-system-graph.yaml");
    std::fs::write(&manifest, MANIFEST)?;
    Ok(manifest)
}

fn alias(aliases: &BTreeMap<RepoId, String>, node: &Node) -> String {
    node.repo_id
        .as_ref()
        .and_then(|repo| aliases.get(repo))
        .cloned()
        .unwrap_or_default()
}

#[test]
fn absolute_urls_should_resolve_through_declared_inferred_and_loopback_authorities()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = write_workspace(temporary.path())?;
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (nodes, edges) = store.load_current_graph("authorities")?;
    let aliases = store
        .load_workspace_registry("authorities")?
        .repositories
        .into_iter()
        .map(|repository| (repository.id, repository.alias))
        .collect::<BTreeMap<_, _>>();
    let nodes = nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect::<BTreeMap<_, _>>();
    let mut links = edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::CallsRemote | EdgeKind::Validates))
        .map(|edge| {
            let source = nodes[&edge.source];
            let target = nodes[&edge.target];
            (
                edge.kind,
                alias(&aliases, source),
                format!("{} -> {}", source.label, target.label),
                alias(&aliases, target),
            )
        })
        .collect::<Vec<_>>();
    links.sort();

    let mut summary = links
        .iter()
        .map(|(kind, source, _, target)| format!("{kind:?} {source} -> {target}"))
        .collect::<Vec<_>>();
    summary.sort();
    assert_eq!(
        summary,
        vec![
            "CallsRemote tests -> api".to_owned(),
            "CallsRemote tests -> billing".to_owned(),
            "CallsRemote web -> api".to_owned(),
            "Validates tests -> api".to_owned(),
            "Validates tests -> billing".to_owned(),
        ],
        "{links:#?}"
    );
    Ok(())
}
