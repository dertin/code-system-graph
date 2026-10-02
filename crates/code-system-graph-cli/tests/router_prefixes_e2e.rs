//! Acceptance tests for router prefixes composed across files before cross-language linking.

use std::collections::BTreeMap;
use std::path::Path;

use code_system_graph::scan_workspace;
use code_system_graph_model::{EdgeKind, RepoId};
use code_system_graph_store_sqlite::SqliteStore;

const SOURCES: [(&str, &str); 6] = [
    (
        "users/app/routers/users.py",
        "from fastapi import APIRouter\n\nrouter = APIRouter(prefix=\"/users\")\n\n@router.get(\"/{user_id}\")\ndef read_user(user_id: int):\n    return {}\n",
    ),
    (
        "users/app/main.py",
        "from fastapi import FastAPI\nfrom app.routers import users\n\napp = FastAPI()\napp.include_router(users.router, prefix=\"/v1\")\n",
    ),
    (
        "orders/internal/http/orders.go",
        "package http\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc RegisterOrders(rg *gin.RouterGroup) {\n\trg.GET(\"/:id\", getOrder)\n}\n\nfunc getOrder(c *gin.Context) {}\n",
    ),
    (
        "orders/cmd/server/main.go",
        "package main\n\nimport (\n\t\"github.com/gin-gonic/gin\"\n\torders \"example.com/orders/internal/http\"\n)\n\nfunc main() {\n\tr := gin.Default()\n\tapi := r.Group(\"/api\")\n\torders.RegisterOrders(api.Group(\"/orders\"))\n\tr.Run()\n}\n",
    ),
    (
        "tests/tests/test_users.py",
        "import httpx\n\ndef test_read_user():\n    httpx.get(\"http://localhost:8000/v1/users/42\")\n",
    ),
    (
        "tests/tests/test_orders.py",
        "import requests\n\ndef test_read_order():\n    requests.get(\"http://localhost:8080/api/orders/7\")\n",
    ),
];

const MANIFEST: &str = "version: 1\nname: routers\nrepos:\n  users:\n    path: users\n  orders:\n    path: orders\n  tests:\n    path: tests\n";

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

#[test]
fn tests_should_link_to_handlers_whose_paths_are_composed_across_files() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = write_workspace(temporary.path())?;
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (nodes, edges) = store.load_current_graph("routers")?;
    let aliases = store
        .load_workspace_registry("routers")?
        .repositories
        .into_iter()
        .map(|repository| (repository.id, repository.alias))
        .collect::<BTreeMap<RepoId, String>>();
    let nodes = nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect::<BTreeMap<_, _>>();
    let mut links = edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Validates)
        .map(|edge| {
            let target = nodes[&edge.target];
            let repo = target
                .repo_id
                .as_ref()
                .and_then(|repo| aliases.get(repo))
                .cloned()
                .unwrap_or_default();
            format!("{} -> {repo}:{}", nodes[&edge.source].label, target.label)
        })
        .collect::<Vec<_>>();
    links.sort();

    assert_eq!(
        links,
        [
            "python/pytest::test_read_order -> orders:GET /api/orders/:id",
            "python/pytest::test_read_user -> users:GET /v1/users/{user_id}",
        ],
    );
    Ok(())
}
