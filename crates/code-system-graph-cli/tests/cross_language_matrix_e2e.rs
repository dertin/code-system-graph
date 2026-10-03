//! Acceptance tests for the cross-language matrix: tests in five languages call providers in
//! eleven frameworks through base URLs, and every pair must be linked to the handler behind the
//! composed route.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use code_system_graph::{scan_workspace, status_workspace};
use code_system_graph_model::{
    Edge, EdgeKind, HttpLinkCoverage, HttpLinkGapReason, Node, NodeId, RepoId
};
use code_system_graph_store_sqlite::SqliteStore;

const WORKSPACE: &str = "cross-language-matrix";

const PROVIDERS: [&str; 11] = [
    "actix", "axum", "chi", "express", "fastapi", "flask", "gin", "nest", "nethttp", "next",
    "spring",
];

const TEST_REPOSITORIES: [&str; 5] = [
    "tests-go",
    "tests-java",
    "tests-python",
    "tests-rust",
    "tests-ts",
];

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/cross-language-matrix/code-system-graph.yaml")
}

struct Graph {
    nodes: BTreeMap<NodeId, Node>,
    edges: Vec<Edge>,
    aliases: BTreeMap<RepoId, String>,
}

impl Graph {
    fn load(database: &Path) -> anyhow::Result<Self> {
        let store = SqliteStore::open_read_only(database)?;
        let (nodes, edges) = store.load_current_graph(WORKSPACE)?;
        let aliases = store
            .load_workspace_registry(WORKSPACE)?
            .repositories
            .into_iter()
            .map(|repository| (repository.id, repository.alias))
            .collect();
        Ok(Self {
            nodes: nodes
                .into_iter()
                .map(|node| (node.id.clone(), node))
                .collect(),
            edges,
            aliases,
        })
    }

    fn alias(&self, id: &NodeId) -> String {
        self.nodes[id]
            .repo_id
            .as_ref()
            .and_then(|repo| self.aliases.get(repo))
            .cloned()
            .unwrap_or_default()
    }

    fn links(&self, kind: EdgeKind) -> Vec<(String, String, String, String)> {
        let mut links = self
            .edges
            .iter()
            .filter(|edge| edge.kind == kind)
            .map(|edge| {
                (
                    self.alias(&edge.source),
                    self.nodes[&edge.source].label.clone(),
                    self.alias(&edge.target),
                    self.nodes[&edge.target].label.clone(),
                )
            })
            .collect::<Vec<_>>();
        links.sort();
        links
    }
}

#[test]
fn every_test_language_should_validate_every_provider_framework() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest(), &database)?;
    let graph = Graph::load(&database)?;
    let validates = graph.links(EdgeKind::Validates);
    let implemented = graph.links(EdgeKind::ImplementedBy);
    let pairs = validates
        .iter()
        .map(|(source, _, target, _)| (source.as_str(), target.as_str()))
        .collect::<BTreeSet<_>>();
    let expected = TEST_REPOSITORIES
        .iter()
        .flat_map(|tests| PROVIDERS.iter().map(move |provider| (*tests, *provider)))
        .collect::<BTreeSet<_>>();
    let handlers = implemented
        .iter()
        .map(|(provider, route, _, handler)| format!("{provider}:{route} -> {handler}"))
        .collect::<Vec<_>>();

    assert_eq!(pairs, expected);
    assert_eq!(validates.len(), expected.len());
    assert_eq!(
        handlers,
        [
            "actix:GET /actix/items/{id} -> rust::read_item",
            "axum:GET /axum/items/:id -> rust::read_item",
            "chi:GET /chi/items/{id} -> go::getItem",
            "chi:GET /health -> go::health",
            "express:GET /express/items/:id -> javascript::items.read",
            "fastapi:GET /fastapi/items/{item_id} -> python::read_item",
            "flask:GET /flask/items/<int:item_id> -> python::read_item",
            "gin:GET /gin/items/:id -> go::getItem",
            "gin:GET /health -> go::health",
            "nest:GET /nest/items/:id -> typescript::findOne",
            "nethttp:GET /nethttp/items/{id} -> go::getItem",
            "next:GET /api/next/items/{id} -> typescript::GET",
            "spring:GET /spring/items/{id} -> java::read",
        ]
    );
    Ok(())
}

#[test]
fn unlinked_calls_should_be_reported_with_their_reason() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = manifest();
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest, &database)?;
    let status = status_workspace(&manifest, &database)?;
    let graph = Graph::load(&database)?;
    let gaps = status
        .http_links
        .gaps
        .iter()
        .map(|gap| {
            let mut candidates = gap
                .candidates
                .iter()
                .map(|candidate| graph.alias(candidate))
                .collect::<Vec<_>>();
            candidates.sort();
            (
                gap.reason,
                format!(
                    "{}:{}",
                    graph.alias(&gap.caller),
                    graph.nodes[&gap.caller].label
                ),
                format!("{} {}", gap.method, gap.path),
                candidates,
            )
        })
        .collect::<BTreeSet<_>>();
    let gap = |reason, caller: &str, route: &str, candidates: &[&str]| {
        (
            reason,
            caller.to_owned(),
            route.to_owned(),
            candidates
                .iter()
                .map(|&candidate| candidate.to_owned())
                .collect(),
        )
    };

    assert_eq!(
        status.http_links.coverage,
        HttpLinkCoverage {
            linked: 110,
            no_provider: 2,
            ambiguous: 2,
            external: 2,
        }
    );
    assert_eq!(status.http_links.gap_count, 6);
    assert_eq!(
        gaps,
        BTreeSet::from([
            gap(
                HttpLinkGapReason::NoProvider,
                "tests-python:python/pytest::test_missing_item",
                "GET /missing/items/42",
                &[],
            ),
            gap(
                HttpLinkGapReason::NoProvider,
                "tests-python:GET /missing/items/42",
                "GET /missing/items/42",
                &[],
            ),
            gap(
                HttpLinkGapReason::Ambiguous,
                "tests-python:python/pytest::test_health",
                "GET /health",
                &["chi", "gin"],
            ),
            gap(
                HttpLinkGapReason::Ambiguous,
                "tests-python:GET /health",
                "GET /health",
                &["chi", "gin"],
            ),
            gap(
                HttpLinkGapReason::External,
                "tests-python:python/pytest::test_public_status",
                "GET /api/v2/status",
                &[],
            ),
            gap(
                HttpLinkGapReason::External,
                "tests-python:GET /api/v2/status",
                "GET /api/v2/status",
                &[],
            ),
        ])
    );
    Ok(())
}
