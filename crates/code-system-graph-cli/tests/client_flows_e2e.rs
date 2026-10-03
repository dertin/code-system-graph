//! Acceptance tests for tests linked to endpoints they reach through wrappers, helpers, and
//! fixtures in other files.

use std::collections::BTreeMap;
use std::path::Path;

use code_system_graph::scan_workspace;
use code_system_graph_model::{EdgeKind, RepoId};
use code_system_graph_store_sqlite::SqliteStore;

const SOURCES: [(&str, &str); 5] = [
    (
        "payments/src/main/java/com/shop/PaymentsController.java",
        r#"package com.shop;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/payments")
public class PaymentsController {
    @PostMapping("/{id}/refunds")
    public Refund refund(@PathVariable String id) {
        return null;
    }
}
"#,
    ),
    (
        "catalog/src/routes.js",
        r#"const express = require("express");
const app = express();
app.get("/products/:sku", getProduct);
function getProduct(req, res) {}
"#,
    ),
    (
        "tests/tests/conftest.py",
        r#"import pytest
import requests

CATALOG = "http://localhost:3000"
SKU = "sku-1"

@pytest.fixture
def product():
    return requests.get(CATALOG + "/products/" + SKU).json()
"#,
    ),
    (
        "tests/tests/client.py",
        r#"import requests

PAYMENTS = "http://localhost:8080"

def post_json(path, body):
    return requests.post(PAYMENTS + path, json=body)

def refund(payment_id):
    return post_json(f"/payments/{payment_id}/refunds", {})
"#,
    ),
    (
        "tests/tests/test_refunds.py",
        r#"from tests.client import refund

def test_refund(product):
    refund(product["payment"])
"#,
    ),
];

const MANIFEST: &str = "version: 1\nname: flows\nrepos:\n  payments:\n    path: payments\n  catalog:\n    path: catalog\n  tests:\n    path: tests\n";

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
fn tests_should_link_to_endpoints_reached_through_wrappers_and_fixtures() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = write_workspace(temporary.path())?;
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (nodes, edges) = store.load_current_graph("flows")?;
    let aliases = store
        .load_workspace_registry("flows")?
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
            "python/pytest::test_refund -> catalog:GET /products/:sku",
            "python/pytest::test_refund -> payments:POST /payments/{id}/refunds",
        ],
    );
    Ok(())
}
