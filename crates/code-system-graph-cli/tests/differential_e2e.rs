//! Differential acceptance tests: an incrementally maintained graph equals a fresh scan after every
//! mutation, and the published graph does not depend on repository order, file creation order, or
//! extraction worker count.

use std::path::{Path, PathBuf};

use code_system_graph::scan_workspace;
use code_system_graph_store_sqlite::SqliteStore;

const WORKSPACE: &str = "cross-language-matrix";

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/cross-language-matrix")
}

fn fixture_files(root: &Path) -> anyhow::Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name() != Some("code-system-graph.yaml".as_ref()) {
                files.push((
                    path.strip_prefix(root)?.to_path_buf(),
                    std::fs::read(&path)?,
                ));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn write_files(root: &Path, files: &[(PathBuf, Vec<u8>)]) -> anyhow::Result<()> {
    for (path, contents) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
    }
    Ok(())
}

fn write_manifest(root: &Path, reversed: bool, workers: u64) -> anyhow::Result<PathBuf> {
    let template = std::fs::read_to_string(fixture_root().join("code-system-graph.yaml"))?;
    let manifest = root.join("code-system-graph.yaml");
    std::fs::write(&manifest, manifest_contents(&template, reversed, workers)?)?;
    Ok(manifest)
}

fn manifest_contents(template: &str, reversed: bool, workers: u64) -> anyhow::Result<String> {
    let template = template.replace("\r\n", "\n");
    let (header, repositories) = template
        .split_once("repos:\n")
        .ok_or_else(|| anyhow::anyhow!("fixture manifest has no repositories"))?;
    let mut entries = Vec::<String>::new();
    for line in repositories.lines() {
        if line.starts_with("  ") && !line.starts_with("    ") {
            entries.push(String::new());
        }
        if let Some(entry) = entries.last_mut() {
            entry.push_str(line);
            entry.push('\n');
        }
    }
    if reversed {
        entries.reverse();
    }
    Ok(format!(
        "{header}executionPolicy:\n  maxExtractionWorkers: {workers}\nrepos:\n{}",
        entries.concat()
    ))
}

#[test]
fn fixture_manifest_should_support_crlf_checkouts() -> anyhow::Result<()> {
    let template = std::fs::read_to_string(fixture_root().join("code-system-graph.yaml"))?
        .replace("\r\n", "\n");
    for reversed in [false, true] {
        assert_eq!(
            manifest_contents(&template.replace('\n', "\r\n"), reversed, 4)?,
            manifest_contents(&template, reversed, 4)?,
        );
    }
    Ok(())
}

/// Canonical serialization of everything a scan publishes for queries and for later reuse.
fn published(database: &Path) -> anyhow::Result<String> {
    let store = SqliteStore::open_read_only(database)?;
    let (nodes, edges) = store.load_current_graph(WORKSPACE)?;
    let evidence = store.load_current_evidence(WORKSPACE)?;
    let links = store.load_http_link_report(WORKSPACE, usize::MAX)?;
    let fingerprints = store.load_current_artifact_fingerprints(WORKSPACE)?;
    let batches = store.load_current_extractor_batches(WORKSPACE)?;
    Ok(serde_json::to_string(&(
        nodes,
        edges,
        evidence,
        links,
        fingerprints,
        batches,
    ))?)
}

fn edit(root: &Path, path: &str, from: &str, to: &str) -> anyhow::Result<()> {
    let path = root.join(path);
    let contents = std::fs::read_to_string(&path)?;
    anyhow::ensure!(
        contents.contains(from),
        "`{from}` is not in {}",
        path.display()
    );
    std::fs::write(&path, contents.replace(from, to))?;
    Ok(())
}

type Mutation = fn(&Path) -> anyhow::Result<()>;

const MUTATIONS: [(&str, Mutation); 8] = [
    ("change a provider route", |root| {
        edit(
            root,
            "gin/internal/items/items.go",
            "\"/:id\"",
            "\"/:id/detail\"",
        )
    }),
    ("change a mount prefix in another file", |root| {
        edit(
            root,
            "fastapi/app/main.py",
            "prefix=\"/fastapi\"",
            "prefix=\"/fast\"",
        )
    }),
    ("follow the prefix in a test", |root| {
        edit(
            root,
            "tests-python/tests/test_matrix.py",
            "/fastapi/items/42",
            "/fast/items/42",
        )
    }),
    ("add a provider file", |root| {
        std::fs::write(
            root.join("chi/stock.go"),
            "package main\n\nimport \"github.com/go-chi/chi/v5\"\n\nfunc stock(r chi.Router) {\n\tr.Get(\"/stock/{sku}\", getStock)\n}\n",
        )?;
        Ok(())
    }),
    ("add a test for the new route", |root| {
        std::fs::write(
            root.join("tests-go/stock_test.go"),
            "package matrix\n\nimport (\n\t\"net/http\"\n\t\"testing\"\n)\n\nfunc TestStock(t *testing.T) {\n\thttp.Get(chiURL + \"/stock/A1\")\n}\n",
        )?;
        Ok(())
    }),
    ("rename a test", |root| {
        edit(
            root,
            "tests-rust/tests/matrix.rs",
            "fn reads_axum_item",
            "fn reads_one_axum_item",
        )
    }),
    ("delete a provider file", |root| {
        std::fs::remove_file(root.join("express/src/routes/items.js"))?;
        Ok(())
    }),
    ("restore the original route", |root| {
        edit(
            root,
            "gin/internal/items/items.go",
            "\"/:id/detail\"",
            "\"/:id\"",
        )
    }),
];

#[test]
fn incremental_scans_should_equal_a_fresh_scan_after_every_mutation() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("workspace");
    write_files(&root, &fixture_files(&fixture_root())?)?;
    let manifest = write_manifest(&root, false, 4)?;
    let incremental = temporary.path().join("incremental.db");
    scan_workspace(&manifest, &incremental)?;

    for (step, (name, mutate)) in MUTATIONS.iter().enumerate() {
        mutate(&root)?;
        let summary = scan_workspace(&manifest, &incremental)?;
        let fresh = temporary.path().join(format!("fresh-{step}.db"));
        scan_workspace(&manifest, &fresh)?;

        assert!(!summary.reused_snapshot, "{name} was not observed");
        assert_eq!(
            published(&incremental)?,
            published(&fresh)?,
            "incremental scan diverged after: {name}"
        );
    }
    Ok(())
}

#[test]
fn published_graph_should_not_depend_on_repository_order_file_order_or_workers()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("workspace");
    let files = fixture_files(&fixture_root())?;
    let mut outputs = Vec::new();
    for (reversed, workers) in [(false, 1), (true, 8), (true, 2)] {
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        let mut ordered = files.clone();
        if reversed {
            ordered.reverse();
        }
        write_files(&root, &ordered)?;
        let manifest = write_manifest(&root, reversed, workers)?;
        let database = temporary
            .path()
            .join(format!("graph-{reversed}-{workers}.db"));
        scan_workspace(&manifest, &database)?;
        outputs.push(published(&database)?);
    }

    assert_eq!(outputs[0], outputs[1]);
    assert_eq!(outputs[0], outputs[2]);
    Ok(())
}
