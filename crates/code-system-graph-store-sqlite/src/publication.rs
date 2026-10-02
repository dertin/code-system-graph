//! Delta publication of the current workspace graph.
//!
//! Each workspace owns exactly one current graph. Publication compares the candidate with the
//! stored rows by identifier and content digest, then writes only inserted, changed, and removed
//! rows inside the caller's transaction. Readers observe either the previous or the new graph.

use std::collections::{BTreeSet, HashMap};

use code_system_graph_model::{
    ArtifactFingerprint, CommunitySnapshot, Edge, Evidence, EvidenceId, ExtractorRun, HttpLinkReport, Node, NodeId, RepoFreshnessState, RepoId, RepositoryCoverageGap, StoredExtractorBatch, WorkspaceRecord
};
use rusqlite::types::Type;
use rusqlite::{OptionalExtension, Row, Transaction, params};

use super::{
    ArtifactDelta, ManualLinkRecord, StoreError, insert_community_snapshot, metric_to_i64
};

type Digest = [u8; 32];
type ArtifactRowKey = (String, String, String, Vec<u8>, String);

/// Candidate graph rows that replace the current workspace graph.
pub(crate) struct PublicationInput<'a> {
    pub(crate) workspace: &'a WorkspaceRecord,
    pub(crate) snapshot_id: &'a str,
    pub(crate) nodes: &'a [Node],
    pub(crate) edges: &'a [Edge],
    pub(crate) evidence: &'a [Evidence],
    pub(crate) fingerprints: &'a [ArtifactFingerprint],
    pub(crate) extractor_batches: &'a [StoredExtractorBatch],
    pub(crate) artifact_delta: Option<ArtifactDelta<'a>>,
    pub(crate) extractor_runs: &'a [ExtractorRun],
    pub(crate) manual_links: &'a [ManualLinkRecord],
    pub(crate) community_snapshot: Option<&'a CommunitySnapshot>,
    pub(crate) coverage_gaps: &'a [RepositoryCoverageGap],
    pub(crate) http_links: &'a HttpLinkReport,
}

/// Writes the candidate as the current graph of its workspace.
pub(crate) fn publish_current_graph<F>(
    transaction: &Transaction<'_>,
    input: &PublicationInput<'_>,
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    let workspace = input.workspace.name.as_str();
    let previous_snapshot_id = transaction
        .query_row(
            "SELECT id FROM repo_snapshots WHERE workspace_name = ?1",
            [workspace],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    transaction.execute(
        "DELETE FROM query_cache WHERE workspace_name = ?1",
        [workspace],
    )?;
    transaction.execute(
        "INSERT INTO repo_snapshots(workspace_name, id) VALUES (?1, ?2)
         ON CONFLICT(workspace_name) DO UPDATE SET
            id = excluded.id,
            created_at = CURRENT_TIMESTAMP",
        params![workspace, input.snapshot_id],
    )?;

    let removed_evidence = upsert_evidence(transaction, workspace, input.evidence, progress)?;
    let removed_nodes = upsert_nodes(transaction, workspace, input.nodes, progress)?;
    replace_changed_edges(transaction, workspace, input.edges, progress)?;
    replace_manual_links(transaction, workspace, input.manual_links)?;
    progress(u64::try_from(input.manual_links.len()).unwrap_or(u64::MAX));
    delete_by_id(
        transaction,
        "DELETE FROM nodes WHERE workspace_name = ?1 AND id = ?2",
        workspace,
        &removed_nodes,
        progress,
    )?;
    delete_by_id(
        transaction,
        "DELETE FROM evidence WHERE workspace_name = ?1 AND id = ?2",
        workspace,
        &removed_evidence,
        progress,
    )?;

    if let Some(delta) = input.artifact_delta {
        apply_artifact_delta(transaction, workspace, delta, progress)?;
    } else {
        upsert_fingerprints(transaction, workspace, input.fingerprints, progress)?;
        upsert_extractor_batches(transaction, workspace, input.extractor_batches, progress)?;
    }
    replace_extractor_runs(transaction, workspace, input.extractor_runs, progress)?;
    replace_freshness(transaction, input.workspace, input.coverage_gaps)?;
    replace_http_links(transaction, workspace, input.http_links)?;

    if let Some(community_snapshot) = input.community_snapshot {
        transaction.execute(
            "DELETE FROM community_snapshots WHERE snapshot_id = ?1",
            [&community_snapshot.snapshot_id],
        )?;
        insert_community_snapshot(transaction, workspace, community_snapshot, progress)?;
    }
    let retained_previous = previous_snapshot_id
        .as_deref()
        .filter(|previous| *previous != input.snapshot_id)
        .unwrap_or(input.snapshot_id);
    transaction.execute(
        "DELETE FROM community_snapshots
         WHERE workspace_name = ?1 AND snapshot_id NOT IN (?2, ?3)",
        params![workspace, input.snapshot_id, retained_previous],
    )?;
    Ok(())
}

struct DigestHasher(blake3::Hasher);

impl DigestHasher {
    fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    fn text(&mut self, value: &str) -> &mut Self {
        self.bytes(value.as_bytes())
    }

    fn optional_text(&mut self, value: Option<&str>) -> &mut Self {
        if let Some(value) = value {
            self.0.update(&[1]);
            self.text(value)
        } else {
            self.0.update(&[0]);
            self
        }
    }

    fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.0.update(&(value.len() as u64).to_le_bytes());
        self.0.update(value);
        self
    }

    fn integer(&mut self, value: Option<i64>) -> &mut Self {
        match value {
            Some(value) => {
                self.0.update(&[1]);
                self.0.update(&value.to_le_bytes());
            }
            None => {
                self.0.update(&[0]);
            }
        }
        self
    }

    fn finish(&self) -> Digest {
        *self.0.finalize().as_bytes()
    }
}

fn node_digest(kind: &str, repo_id: Option<&str>, stable_key: &str, label: &str) -> Digest {
    DigestHasher::new()
        .text(kind)
        .optional_text(repo_id)
        .text(stable_key)
        .text(label)
        .finish()
}

fn upsert_nodes<F>(
    transaction: &Transaction<'_>,
    workspace: &str,
    nodes: &[Node],
    progress: &mut F,
) -> Result<Vec<String>, StoreError>
where
    F: FnMut(u64),
{
    let mut stored = HashMap::<String, Digest>::new();
    {
        let mut statement = transaction.prepare_cached(
            "SELECT id, kind, repo_id, stable_key, label FROM nodes WHERE workspace_name = ?1",
        )?;
        let mut rows = statement.query([workspace])?;
        while let Some(row) = rows.next()? {
            let digest = node_digest(
                column_text(row, 1)?,
                optional_column_text(row, 2)?,
                column_text(row, 3)?,
                column_text(row, 4)?,
            );
            stored.insert(row.get(0)?, digest);
        }
    }
    let mut insert = transaction.prepare_cached(
        "INSERT INTO nodes(workspace_name, id, kind, repo_id, stable_key, label)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(workspace_name, id) DO UPDATE SET
            kind = excluded.kind,
            repo_id = excluded.repo_id,
            stable_key = excluded.stable_key,
            label = excluded.label",
    )?;
    for node in nodes {
        let kind = serde_json::to_string(&node.kind)?;
        let repo_id = node.repo_id.as_ref().map(RepoId::as_str);
        let digest = node_digest(&kind, repo_id, &node.stable_key, &node.label);
        if stored.remove(node.id.as_str()) == Some(digest) {
            continue;
        }
        insert.execute(params![
            workspace,
            node.id.as_str(),
            kind,
            repo_id,
            node.stable_key,
            node.label,
        ])?;
        progress(1);
    }
    Ok(sorted_keys(stored))
}

fn evidence_digest(item: &Evidence, provenance: &str) -> Digest {
    DigestHasher::new()
        .optional_text(item.repo_id.as_ref().map(RepoId::as_str))
        .optional_text(item.file_path.as_deref())
        .integer(item.start_line.map(i64::from))
        .integer(item.end_line.map(i64::from))
        .text(&item.extractor)
        .text(&item.extractor_version)
        .text(provenance)
        .integer(Some(i64::from(item.confidence.to_bits())))
        .optional_text(item.content_hash.as_deref())
        .optional_text(item.observed_at_commit.as_deref())
        .finish()
}

fn upsert_evidence<F>(
    transaction: &Transaction<'_>,
    workspace: &str,
    evidence: &[Evidence],
    progress: &mut F,
) -> Result<Vec<String>, StoreError>
where
    F: FnMut(u64),
{
    let mut stored = HashMap::<String, Digest>::new();
    {
        let mut statement = transaction.prepare_cached(
            "SELECT id, repo_id, file_path, start_line, end_line, extractor, extractor_version,
                    provenance, confidence, content_hash, observed_at_commit
             FROM evidence WHERE workspace_name = ?1",
        )?;
        let mut rows = statement.query([workspace])?;
        while let Some(row) = rows.next()? {
            let confidence = row.get::<_, f32>(8)?;
            let digest = DigestHasher::new()
                .optional_text(optional_column_text(row, 1)?)
                .optional_text(optional_column_text(row, 2)?)
                .integer(row.get::<_, Option<i64>>(3)?)
                .integer(row.get::<_, Option<i64>>(4)?)
                .text(column_text(row, 5)?)
                .text(column_text(row, 6)?)
                .text(column_text(row, 7)?)
                .integer(Some(i64::from(confidence.to_bits())))
                .optional_text(optional_column_text(row, 9)?)
                .optional_text(optional_column_text(row, 10)?)
                .finish();
            stored.insert(row.get(0)?, digest);
        }
    }
    let mut insert = transaction.prepare_cached(
        "INSERT INTO evidence(
            workspace_name, id, repo_id, file_path, start_line, end_line, extractor,
            extractor_version, provenance, confidence, observed_at_commit, content_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(workspace_name, id) DO UPDATE SET
            repo_id = excluded.repo_id,
            file_path = excluded.file_path,
            start_line = excluded.start_line,
            end_line = excluded.end_line,
            extractor = excluded.extractor,
            extractor_version = excluded.extractor_version,
            provenance = excluded.provenance,
            confidence = excluded.confidence,
            observed_at_commit = excluded.observed_at_commit,
            content_hash = excluded.content_hash",
    )?;
    for item in evidence {
        let provenance = serde_json::to_string(&item.provenance)?;
        let digest = evidence_digest(item, &provenance);
        if stored.remove(item.id.as_str()) == Some(digest) {
            continue;
        }
        insert.execute(params![
            workspace,
            item.id.as_str(),
            item.repo_id.as_ref().map(RepoId::as_str),
            item.file_path,
            item.start_line,
            item.end_line,
            item.extractor,
            item.extractor_version,
            provenance,
            item.confidence,
            item.observed_at_commit,
            item.content_hash,
        ])?;
        progress(1);
    }
    Ok(sorted_keys(stored))
}

fn edge_digest<'a>(
    source: &str,
    target: &str,
    kind: &str,
    confidence: f32,
    status: &str,
    evidence: impl Iterator<Item = &'a str>,
) -> Digest {
    let mut hasher = DigestHasher::new();
    hasher
        .text(source)
        .text(target)
        .text(kind)
        .integer(Some(i64::from(confidence.to_bits())))
        .text(status);
    for evidence_id in evidence {
        hasher.text(evidence_id);
    }
    hasher.finish()
}

fn replace_changed_edges<F>(
    transaction: &Transaction<'_>,
    workspace: &str,
    edges: &[Edge],
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    let mut stored_evidence = HashMap::<String, Vec<String>>::new();
    {
        let mut statement = transaction.prepare_cached(
            "SELECT edge_id, evidence_id FROM edge_evidence
             WHERE workspace_name = ?1 ORDER BY edge_id, evidence_id",
        )?;
        let mut rows = statement.query([workspace])?;
        while let Some(row) = rows.next()? {
            stored_evidence
                .entry(row.get(0)?)
                .or_default()
                .push(row.get(1)?);
        }
    }
    let mut stored = HashMap::<String, Digest>::new();
    {
        let mut statement = transaction.prepare_cached(
            "SELECT id, source_node_id, target_node_id, kind, confidence, epistemic_status
             FROM edges WHERE workspace_name = ?1",
        )?;
        let mut rows = statement.query([workspace])?;
        while let Some(row) = rows.next()? {
            let id = row.get::<_, String>(0)?;
            let evidence = stored_evidence.remove(&id).unwrap_or_default();
            let digest = edge_digest(
                column_text(row, 1)?,
                column_text(row, 2)?,
                column_text(row, 3)?,
                row.get::<_, f32>(4)?,
                column_text(row, 5)?,
                evidence.iter().map(String::as_str),
            );
            stored.insert(id, digest);
        }
    }
    let mut changed = Vec::new();
    for edge in edges {
        let kind = serde_json::to_string(&edge.kind)?;
        let status = serde_json::to_string(&edge.status)?;
        let mut evidence = edge
            .evidence
            .iter()
            .map(EvidenceId::as_str)
            .collect::<Vec<_>>();
        evidence.sort_unstable();
        let digest = edge_digest(
            edge.source.as_str(),
            edge.target.as_str(),
            &kind,
            edge.confidence,
            &status,
            evidence.into_iter(),
        );
        match stored.remove(edge.id.as_str()) {
            Some(previous) if previous == digest => {}
            Some(_) => changed.push((edge, kind, status, true)),
            None => changed.push((edge, kind, status, false)),
        }
    }
    let mut delete =
        transaction.prepare_cached("DELETE FROM edges WHERE workspace_name = ?1 AND id = ?2")?;
    for removed in sorted_keys(stored) {
        delete.execute(params![workspace, removed])?;
        progress(1);
    }
    let mut insert_edge = transaction.prepare_cached(
        "INSERT INTO edges(
            workspace_name, id, source_node_id, target_node_id, kind, confidence,
            epistemic_status
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut insert_evidence = transaction.prepare_cached(
        "INSERT INTO edge_evidence(workspace_name, edge_id, evidence_id) VALUES (?1, ?2, ?3)",
    )?;
    for (edge, kind, status, existed) in changed {
        if existed {
            delete.execute(params![workspace, edge.id.as_str()])?;
        }
        insert_edge.execute(params![
            workspace,
            edge.id.as_str(),
            edge.source.as_str(),
            edge.target.as_str(),
            kind,
            edge.confidence,
            status,
        ])?;
        progress(1);
        for evidence_id in &edge.evidence {
            insert_evidence.execute(params![workspace, edge.id.as_str(), evidence_id.as_str()])?;
            progress(1);
        }
    }
    Ok(())
}

fn insert_manual_links(
    transaction: &Transaction<'_>,
    workspace: &str,
    records: &[ManualLinkRecord],
) -> Result<(), StoreError> {
    let mut statement = transaction.prepare_cached(
        "INSERT INTO manual_links(
            workspace_name, id, source_node_id, target_node_id, kind, disposition, reason,
            decision_json, config_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for record in records {
        statement.execute(params![
            workspace,
            record.id,
            record.source_node_id.as_str(),
            record.target_node_id.as_str(),
            record.kind,
            record.disposition.as_str(),
            record.reason,
            serde_json::to_vec(&record.decision)?,
            i64::from(record.config_version),
        ])?;
    }
    Ok(())
}

/// Replaces manual link declarations of the current workspace graph.
pub(crate) fn replace_manual_links(
    transaction: &Transaction<'_>,
    workspace: &str,
    records: &[ManualLinkRecord],
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM manual_links WHERE workspace_name = ?1",
        [workspace],
    )?;
    insert_manual_links(transaction, workspace, records)
}

fn delete_by_id<F>(
    transaction: &Transaction<'_>,
    sql: &str,
    workspace: &str,
    ids: &[String],
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    let mut statement = transaction.prepare_cached(sql)?;
    for id in ids {
        statement.execute(params![workspace, id])?;
        progress(1);
    }
    Ok(())
}

fn artifact_key(fingerprint: &ArtifactFingerprint) -> Result<ArtifactRowKey, StoreError> {
    Ok((
        fingerprint.repo_id.as_str().to_owned(),
        fingerprint.checkout_id.as_str().to_owned(),
        serde_json::to_string(&fingerprint.path.encoding)?,
        fingerprint.path.bytes.clone(),
        fingerprint.extractor.clone(),
    ))
}

fn load_artifact_digests(
    transaction: &Transaction<'_>,
    sql: &str,
    workspace: &str,
    digest_columns: usize,
) -> Result<HashMap<ArtifactRowKey, Digest>, StoreError> {
    let mut stored = HashMap::new();
    let mut statement = transaction.prepare_cached(sql)?;
    let mut rows = statement.query([workspace])?;
    while let Some(row) = rows.next()? {
        let mut hasher = DigestHasher::new();
        for index in 5..5 + digest_columns {
            match row.get_ref(index)? {
                rusqlite::types::ValueRef::Integer(value) => {
                    hasher.integer(Some(value));
                }
                _ => {
                    hasher.text(column_text(row, index)?);
                }
            }
        }
        stored.insert(
            (
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ),
            hasher.finish(),
        );
    }
    Ok(stored)
}

fn delete_artifact_rows<F>(
    transaction: &Transaction<'_>,
    table: &str,
    workspace: &str,
    stored: HashMap<ArtifactRowKey, Digest>,
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    let mut removed = stored.into_keys().collect::<Vec<_>>();
    removed.sort_unstable();
    let mut statement = transaction.prepare_cached(&format!(
        "DELETE FROM {table}
         WHERE workspace_name = ?1 AND repo_id = ?2 AND checkout_id = ?3
           AND path_encoding = ?4 AND relative_path = ?5 AND extractor = ?6"
    ))?;
    for (repo_id, checkout_id, encoding, path, extractor) in removed {
        statement.execute(params![
            workspace,
            repo_id,
            checkout_id,
            encoding,
            path,
            extractor
        ])?;
        progress(1);
    }
    Ok(())
}

const UPSERT_FINGERPRINT_SQL: &str = "INSERT INTO artifact_fingerprints(
        workspace_name, repo_id, checkout_id, path_encoding, relative_path,
        path_display, extractor, content_hash, size_bytes
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
     ON CONFLICT(
        workspace_name, repo_id, checkout_id, path_encoding, relative_path, extractor
     ) DO UPDATE SET
        path_display = excluded.path_display,
        content_hash = excluded.content_hash,
        size_bytes = excluded.size_bytes";

const UPSERT_BATCH_SQL: &str = "INSERT INTO extractor_batches(
        workspace_name, repo_id, checkout_id, path_encoding, relative_path, path_display,
        extractor, content_hash, size_bytes, extractor_version, budget_fingerprint,
        source_was_lossy, output_count, payload_hash, payload
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
     ON CONFLICT(
        workspace_name, repo_id, checkout_id, path_encoding, relative_path, extractor
     ) DO UPDATE SET
        path_display = excluded.path_display,
        content_hash = excluded.content_hash,
        size_bytes = excluded.size_bytes,
        extractor_version = excluded.extractor_version,
        budget_fingerprint = excluded.budget_fingerprint,
        source_was_lossy = excluded.source_was_lossy,
        output_count = excluded.output_count,
        payload_hash = excluded.payload_hash,
        payload = excluded.payload";

fn write_fingerprint(
    insert: &mut rusqlite::CachedStatement<'_>,
    workspace: &str,
    fingerprint: &ArtifactFingerprint,
) -> Result<(), StoreError> {
    insert.execute(params![
        workspace,
        fingerprint.repo_id.as_str(),
        fingerprint.checkout_id.as_str(),
        serde_json::to_string(&fingerprint.path.encoding)?,
        fingerprint.path.bytes,
        fingerprint.path.display,
        fingerprint.extractor,
        fingerprint.content_hash,
        metric_to_i64("artifact_fingerprints.size_bytes", fingerprint.size_bytes)?,
    ])?;
    Ok(())
}

fn write_batch(
    insert: &mut rusqlite::CachedStatement<'_>,
    workspace: &str,
    batch: &StoredExtractorBatch,
    payload_hash: &str,
) -> Result<(), StoreError> {
    insert.execute(params![
        workspace,
        batch.source.repo_id.as_str(),
        batch.source.checkout_id.as_str(),
        serde_json::to_string(&batch.source.path.encoding)?,
        batch.source.path.bytes,
        batch.source.path.display,
        batch.source.extractor,
        batch.source.content_hash,
        metric_to_i64("extractor_batches.size_bytes", batch.source.size_bytes)?,
        batch.extractor_version,
        batch.budget_fingerprint,
        batch.source_was_lossy,
        metric_to_i64("extractor_batches.output_count", batch.output_count)?,
        payload_hash,
        batch.payload,
    ])?;
    Ok(())
}

/// Writes only the artifact rows that changed since the current graph.
fn apply_artifact_delta<F>(
    transaction: &Transaction<'_>,
    workspace: &str,
    delta: ArtifactDelta<'_>,
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    let mut insert_fingerprint = transaction.prepare_cached(UPSERT_FINGERPRINT_SQL)?;
    for fingerprint in delta.upserted_fingerprints {
        write_fingerprint(&mut insert_fingerprint, workspace, fingerprint)?;
        progress(1);
    }
    let mut insert_batch = transaction.prepare_cached(UPSERT_BATCH_SQL)?;
    for batch in delta.upserted_batches {
        let payload_hash = blake3::hash(&batch.payload).to_hex().to_string();
        write_batch(&mut insert_batch, workspace, batch, &payload_hash)?;
        progress(1);
    }
    for table in ["artifact_fingerprints", "extractor_batches"] {
        let mut delete = transaction.prepare_cached(&format!(
            "DELETE FROM {table}
             WHERE workspace_name = ?1 AND repo_id = ?2 AND checkout_id = ?3
               AND path_encoding = ?4 AND relative_path = ?5 AND extractor = ?6"
        ))?;
        for removed in delta.removed {
            delete.execute(params![
                workspace,
                removed.repo_id.as_str(),
                removed.checkout_id.as_str(),
                serde_json::to_string(&removed.path.encoding)?,
                removed.path.bytes,
                removed.extractor,
            ])?;
            progress(1);
        }
    }
    Ok(())
}

fn upsert_fingerprints<F>(
    transaction: &Transaction<'_>,
    workspace: &str,
    fingerprints: &[ArtifactFingerprint],
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    let mut stored = load_artifact_digests(
        transaction,
        "SELECT repo_id, checkout_id, path_encoding, relative_path, extractor,
                path_display, content_hash, size_bytes
         FROM artifact_fingerprints WHERE workspace_name = ?1",
        workspace,
        3,
    )?;
    let mut insert = transaction.prepare_cached(UPSERT_FINGERPRINT_SQL)?;
    for fingerprint in fingerprints {
        let size_bytes = metric_to_i64("artifact_fingerprints.size_bytes", fingerprint.size_bytes)?;
        let digest = DigestHasher::new()
            .text(&fingerprint.path.display)
            .text(&fingerprint.content_hash)
            .integer(Some(size_bytes))
            .finish();
        let key = artifact_key(fingerprint)?;
        if stored.remove(&key) == Some(digest) {
            continue;
        }
        write_fingerprint(&mut insert, workspace, fingerprint)?;
        progress(1);
    }
    delete_artifact_rows(
        transaction,
        "artifact_fingerprints",
        workspace,
        stored,
        progress,
    )
}

fn upsert_extractor_batches<F>(
    transaction: &Transaction<'_>,
    workspace: &str,
    batches: &[StoredExtractorBatch],
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    let mut stored = load_artifact_digests(
        transaction,
        "SELECT repo_id, checkout_id, path_encoding, relative_path, extractor,
                path_display, content_hash, size_bytes, extractor_version, budget_fingerprint,
                source_was_lossy, output_count, payload_hash
         FROM extractor_batches WHERE workspace_name = ?1",
        workspace,
        8,
    )?;
    let mut insert = transaction.prepare_cached(UPSERT_BATCH_SQL)?;
    for batch in batches {
        let size_bytes = metric_to_i64("extractor_batches.size_bytes", batch.source.size_bytes)?;
        let output_count = metric_to_i64("extractor_batches.output_count", batch.output_count)?;
        let payload_hash = blake3::hash(&batch.payload).to_hex().to_string();
        let digest = DigestHasher::new()
            .text(&batch.source.path.display)
            .text(&batch.source.content_hash)
            .integer(Some(size_bytes))
            .text(&batch.extractor_version)
            .text(&batch.budget_fingerprint)
            .integer(Some(i64::from(batch.source_was_lossy)))
            .integer(Some(output_count))
            .text(&payload_hash)
            .finish();
        let key = artifact_key(&batch.source)?;
        if stored.remove(&key) == Some(digest) {
            continue;
        }
        write_batch(&mut insert, workspace, batch, &payload_hash)?;
        progress(1);
    }
    delete_artifact_rows(
        transaction,
        "extractor_batches",
        workspace,
        stored,
        progress,
    )
}

fn replace_extractor_runs<F>(
    transaction: &Transaction<'_>,
    workspace: &str,
    runs: &[ExtractorRun],
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    transaction.execute(
        "DELETE FROM extractor_runs WHERE workspace_name = ?1",
        [workspace],
    )?;
    let mut statement = transaction.prepare_cached(
        "INSERT INTO extractor_runs(
            workspace_name, id, repo_id, extractor, extractor_version, status,
            discovered_files, parsed_files, skipped_files, elapsed_ms, checkout_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for run in runs {
        statement.execute(params![
            workspace,
            run.id,
            run.repo_id.as_str(),
            run.extractor,
            run.extractor_version,
            serde_json::to_string(&run.status)?,
            metric_to_i64("extractor_runs.discovered_files", run.discovered_files)?,
            metric_to_i64("extractor_runs.parsed_files", run.parsed_files)?,
            metric_to_i64("extractor_runs.skipped_files", run.skipped_files)?,
            metric_to_i64("extractor_runs.elapsed_ms", run.elapsed_ms)?,
            run.checkout_id.as_str(),
        ])?;
        progress(1);
    }
    Ok(())
}

/// Persisted key of one HTTP link gap: caller, method, path, reason, and candidates JSON.
type HttpLinkGapRow = (String, String, String, String, String);

/// Replaces the HTTP link coverage of `workspace`, writing only rows that changed.
fn replace_http_links(
    transaction: &Transaction<'_>,
    workspace: &str,
    report: &HttpLinkReport,
) -> Result<(), StoreError> {
    let coverage = report.coverage;
    let count = |value: u64, field: &'static str| {
        i64::try_from(value).map_err(|_| StoreError::IntegerOutOfRange {
            field,
            value: i128::from(value),
        })
    };
    transaction.execute(
        "INSERT INTO http_link_coverage(workspace_name, linked, no_provider, ambiguous, external)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(workspace_name) DO UPDATE SET
            linked = excluded.linked,
            no_provider = excluded.no_provider,
            ambiguous = excluded.ambiguous,
            external = excluded.external
         WHERE linked != excluded.linked
            OR no_provider != excluded.no_provider
            OR ambiguous != excluded.ambiguous
            OR external != excluded.external",
        params![
            workspace,
            count(coverage.linked, "http_link_coverage.linked")?,
            count(coverage.no_provider, "http_link_coverage.no_provider")?,
            count(coverage.ambiguous, "http_link_coverage.ambiguous")?,
            count(coverage.external, "http_link_coverage.external")?,
        ],
    )?;
    let mut current = BTreeSet::new();
    for gap in &report.gaps {
        let within_bounds = (1..=2048).contains(&gap.caller.as_str().len())
            && (1..=32).contains(&gap.method.len())
            && (1..=4096).contains(&gap.path.len());
        if !within_bounds {
            continue;
        }
        let candidates = gap
            .candidates
            .iter()
            .map(NodeId::as_str)
            .collect::<Vec<_>>();
        current.insert((
            gap.caller.as_str().to_owned(),
            gap.method.clone(),
            gap.path.clone(),
            gap.reason.as_str().to_owned(),
            serde_json::to_string(&candidates)?,
        ));
    }
    let previous = {
        let mut statement = transaction.prepare_cached(
            "SELECT caller_node_id, method, path, reason, candidates_json
             FROM http_link_gaps WHERE workspace_name = ?1",
        )?;
        statement
            .query_map([workspace], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<BTreeSet<HttpLinkGapRow>, _>>()?
    };
    let mut delete = transaction.prepare_cached(
        "DELETE FROM http_link_gaps
         WHERE workspace_name = ?1 AND caller_node_id = ?2 AND method = ?3 AND path = ?4
           AND reason = ?5",
    )?;
    for (caller, method, path, reason, _) in previous.difference(&current) {
        delete.execute(params![workspace, caller, method, path, reason])?;
    }
    let mut insert = transaction.prepare_cached(
        "INSERT INTO http_link_gaps(
            workspace_name, caller_node_id, method, path, reason, candidates_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (caller, method, path, reason, candidates) in current.difference(&previous) {
        insert.execute(params![workspace, caller, method, path, reason, candidates])?;
    }
    Ok(())
}

fn replace_freshness(
    transaction: &Transaction<'_>,
    workspace: &WorkspaceRecord,
    coverage_gaps: &[RepositoryCoverageGap],
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM repository_snapshot_freshness WHERE workspace_name = ?1",
        [&workspace.name],
    )?;
    let mut insert = transaction.prepare_cached(
        "INSERT INTO repository_snapshot_freshness(
            workspace_name, repo_id, checkout_id, head_commit, manifest_hash, state, reason
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for repository in &workspace.repositories {
        let mut coverage_reasons = coverage_gaps
            .iter()
            .filter(|gap| gap.repo_id == repository.id)
            .map(|gap| gap.reason.clone())
            .collect::<Vec<_>>();
        coverage_reasons.sort();
        coverage_reasons.dedup();
        let (freshness_state, reason) = if !coverage_reasons.is_empty() {
            if repository.working_tree_dirty {
                coverage_reasons.push(
                    "The repository working tree differed from HEAD when scanned.".to_owned(),
                );
            }
            (
                RepoFreshnessState::Partial,
                Some(coverage_reasons.join(" ")),
            )
        } else if repository.working_tree_dirty {
            (RepoFreshnessState::WorkingTreeChanged, None)
        } else {
            (RepoFreshnessState::Fresh, None)
        };
        insert.execute(params![
            workspace.name,
            repository.id.as_str(),
            repository.checkout_id.as_str(),
            repository.head_commit,
            workspace.manifest_hash,
            serde_json::to_string(&freshness_state)?,
            reason,
        ])?;
    }
    transaction.execute(
        "DELETE FROM provider_capabilities
         WHERE workspace_name = ?1
           AND NOT EXISTS (
                SELECT 1 FROM workspace_repositories
                WHERE workspace_name = ?1
                  AND repo_id = provider_capabilities.repo_id
           )",
        [&workspace.name],
    )?;
    Ok(())
}

fn column_text<'row>(row: &'row Row<'_>, index: usize) -> rusqlite::Result<&'row str> {
    row.get_ref(index)?
        .as_str()
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(index, Type::Text, error.into()))
}

fn optional_column_text<'row>(
    row: &'row Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<&'row str>> {
    row.get_ref(index)?
        .as_str_or_null()
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(index, Type::Text, error.into()))
}

fn sorted_keys(map: HashMap<String, Digest>) -> Vec<String> {
    let mut keys = map.into_keys().collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}
