//! Acceptance tests for tests that exercise their own application through in-process test
//! clients, linked to the provider of their repository even when another repository serves the
//! same route.

use std::collections::BTreeMap;
use std::path::Path;

use code_system_graph::scan_workspace;
use code_system_graph_model::{EdgeKind, RepoId};
use code_system_graph_store_sqlite::SqliteStore;

const SOURCES: [(&str, &str); 9] = [
    (
        "orders-py/app/main.py",
        r#"from fastapi import FastAPI

app = FastAPI()

@app.get("/orders/{order_id}")
def read_order(order_id: str):
    return {}
"#,
    ),
    (
        "orders-py/tests/conftest.py",
        r"import pytest
from fastapi.testclient import TestClient
from app.main import app

@pytest.fixture
def client():
    return TestClient(app)
",
    ),
    (
        "orders-py/tests/test_orders.py",
        r#"def test_read_order(client):
    assert client.get("/orders/42").status_code == 200
"#,
    ),
    (
        "orders-js/src/app.js",
        r#"const express = require("express");
const orders = require("./orders");
const app = express();
app.get("/orders/:id", orders.read);
module.exports = app;
"#,
    ),
    (
        "orders-js/test/orders.test.js",
        r#"const request = require("supertest");
const app = require("../src/app");

describe("orders", () => {
  it("reads one", async () => {
    await request(app).get("/orders/7").expect(200);
  });
});
"#,
    ),
    (
        "inventory/main.go",
        r#"package main

import "github.com/gin-gonic/gin"

func router() *gin.Engine {
	r := gin.Default()
	r.GET("/stock/:sku", handlers.GetStock)
	return r
}
"#,
    ),
    (
        "inventory/main_test.go",
        r#"package main

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestGetStock(t *testing.T) {
	req := httptest.NewRequest(http.MethodGet, "/stock/A1", nil)
	rec := httptest.NewRecorder()
	router().ServeHTTP(rec, req)
}
"#,
    ),
    (
        "billing/src/main/java/com/shop/InvoicesController.java",
        r#"package com.shop;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/invoices")
public class InvoicesController {
    @GetMapping("/{id}")
    public Invoice read(@PathVariable String id) {
        return null;
    }
}
"#,
    ),
    (
        "billing/src/test/java/com/shop/InvoicesControllerTest.java",
        r#"package com.shop;

import org.junit.jupiter.api.Test;
import org.springframework.test.web.servlet.MockMvc;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.get;

class InvoicesControllerTest {
    private MockMvc mockMvc;

    @Test
    void readsInvoice() throws Exception {
        mockMvc.perform(get("/invoices/{id}", "inv-1"));
    }
}
"#,
    ),
];

const MANIFEST: &str = "version: 1\nname: inproc\nrepos:\n  orders-py:\n    path: orders-py\n  orders-js:\n    path: orders-js\n  inventory:\n    path: inventory\n  billing:\n    path: billing\n";

#[test]
fn absolute_urls_in_process_should_validate_only_the_local_provider() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = write_workspace(temporary.path())?;
    std::fs::write(
        temporary.path().join("orders-py/tests/test_absolute.py"),
        r#"import httpx
from fastapi.testclient import TestClient
from app.main import app

client = TestClient(app)

def test_absolute_url():
    client.get("http://testserver/orders/42")

async def test_asgi_absolute_url():
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app)) as api:
        await api.get("http://testserver/orders/43")
"#,
    )?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (nodes, edges) = store.load_current_graph("inproc")?;
    let nodes = nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect::<BTreeMap<_, _>>();
    for name in ["test_absolute_url", "test_asgi_absolute_url"] {
        let links = edges
            .iter()
            .filter(|edge| {
                edge.kind == EdgeKind::Validates && nodes[&edge.source].label.ends_with(name)
            })
            .collect::<Vec<_>>();
        assert_eq!(links.len(), 1, "missing local validation for {name}");
        let source = nodes[&links[0].source];
        let target = nodes[&links[0].target];
        assert_eq!(source.repo_id, target.repo_id);
        assert_eq!(target.label, "GET /orders/{order_id}");
    }
    Ok(())
}

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
fn in_process_tests_should_validate_the_provider_of_their_own_repository() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = write_workspace(temporary.path())?;
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (nodes, edges) = store.load_current_graph("inproc")?;
    let aliases = store
        .load_workspace_registry("inproc")?
        .repositories
        .into_iter()
        .map(|repository| (repository.id, repository.alias))
        .collect::<BTreeMap<RepoId, String>>();
    let nodes = nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect::<BTreeMap<_, _>>();
    let alias = |repo: Option<&RepoId>| {
        repo.and_then(|repo| aliases.get(repo))
            .cloned()
            .unwrap_or_default()
    };
    let mut links = edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Validates)
        .map(|edge| {
            let source = nodes[&edge.source];
            let target = nodes[&edge.target];
            format!(
                "{}:{} -> {}:{}",
                alias(source.repo_id.as_ref()),
                source.label,
                alias(target.repo_id.as_ref()),
                target.label
            )
        })
        .collect::<Vec<_>>();
    links.sort();

    assert_eq!(
        links,
        [
            "billing:java/junit::readsInvoice -> billing:GET /invoices/{id}",
            "inventory:go/go-test::TestGetStock -> inventory:GET /stock/:sku",
            "orders-js:javascript/jest::orders > reads one -> orders-js:GET /orders/:id",
            "orders-py:python/pytest::test_read_order -> orders-py:GET /orders/{order_id}",
        ],
    );
    Ok(())
}
