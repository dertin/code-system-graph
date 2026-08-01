PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_metadata (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS workspaces (
    name TEXT PRIMARY KEY,
    manifest_hash TEXT NOT NULL,
    id TEXT,
    config_path_encoding TEXT,
    config_path BLOB,
    config_path_display TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS workspaces_id_idx
ON workspaces(id)
WHERE id IS NOT NULL;

CREATE TABLE IF NOT EXISTS repositories (
    id TEXT PRIMARY KEY,
    normalized_remote TEXT,
    git_common_path_encoding TEXT,
    git_common_path BLOB,
    git_common_path_display TEXT
);

CREATE TABLE IF NOT EXISTS repository_checkouts (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    path_encoding TEXT NOT NULL,
    canonical_path BLOB NOT NULL,
    path_display TEXT NOT NULL,
    head_commit TEXT,
    is_linked_worktree INTEGER NOT NULL CHECK (is_linked_worktree IN (0, 1)),
    working_tree_dirty INTEGER NOT NULL CHECK (working_tree_dirty IN (0, 1))
);

CREATE INDEX IF NOT EXISTS repository_checkouts_repo_idx
ON repository_checkouts(repo_id);

CREATE TABLE IF NOT EXISTS workspace_repositories (
    workspace_name TEXT NOT NULL REFERENCES workspaces(name) ON DELETE CASCADE,
    alias TEXT NOT NULL,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    checkout_id TEXT NOT NULL REFERENCES repository_checkouts(id),
    PRIMARY KEY (workspace_name, alias)
);

CREATE INDEX IF NOT EXISTS workspace_repositories_repo_idx
ON workspace_repositories(repo_id);

CREATE TABLE IF NOT EXISTS repo_snapshots (
    id TEXT PRIMARY KEY,
    workspace_name TEXT NOT NULL REFERENCES workspaces(name) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_current INTEGER NOT NULL DEFAULT 0 CHECK (is_current IN (0, 1))
);

CREATE UNIQUE INDEX IF NOT EXISTS one_current_snapshot_per_workspace
ON repo_snapshots(workspace_name)
WHERE is_current = 1;

CREATE UNIQUE INDEX IF NOT EXISTS repo_snapshots_id_workspace_idx
ON repo_snapshots(id, workspace_name);

CREATE TABLE IF NOT EXISTS nodes (
    snapshot_id TEXT NOT NULL REFERENCES repo_snapshots(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    kind TEXT NOT NULL,
    repo_id TEXT,
    stable_key TEXT NOT NULL,
    label TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, id)
);

CREATE INDEX IF NOT EXISTS nodes_kind_idx ON nodes(kind);
CREATE INDEX IF NOT EXISTS nodes_repo_idx ON nodes(repo_id);
CREATE INDEX IF NOT EXISTS nodes_stable_key_idx ON nodes(stable_key);

CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    snapshot_id UNINDEXED,
    node_id UNINDEXED,
    label,
    stable_key
);

CREATE TABLE IF NOT EXISTS evidence (
    snapshot_id TEXT NOT NULL REFERENCES repo_snapshots(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    repo_id TEXT,
    file_path TEXT,
    start_line INTEGER CHECK (start_line IS NULL OR start_line > 0),
    end_line INTEGER CHECK (
        end_line IS NULL OR (
            end_line > 0
            AND (start_line IS NULL OR end_line >= start_line)
        )
    ),
    extractor TEXT NOT NULL,
    extractor_version TEXT NOT NULL,
    provenance TEXT NOT NULL,
    confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    content_hash TEXT,
    observed_at_commit TEXT,
    PRIMARY KEY (snapshot_id, id)
);

CREATE INDEX IF NOT EXISTS evidence_snapshot_file_lines_idx
ON evidence(snapshot_id, repo_id, file_path, start_line, end_line);

CREATE TABLE IF NOT EXISTS edges (
    snapshot_id TEXT NOT NULL REFERENCES repo_snapshots(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    source_node_id TEXT NOT NULL,
    target_node_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    epistemic_status TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, id),
    FOREIGN KEY (snapshot_id, source_node_id) REFERENCES nodes(snapshot_id, id),
    FOREIGN KEY (snapshot_id, target_node_id) REFERENCES nodes(snapshot_id, id)
);

CREATE INDEX IF NOT EXISTS edges_source_idx ON edges(source_node_id);
CREATE INDEX IF NOT EXISTS edges_target_idx ON edges(target_node_id);
CREATE INDEX IF NOT EXISTS edges_kind_idx ON edges(kind);
CREATE INDEX IF NOT EXISTS edges_confidence_idx ON edges(confidence);

CREATE TABLE IF NOT EXISTS edge_evidence (
    snapshot_id TEXT NOT NULL,
    edge_id TEXT NOT NULL,
    evidence_id TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, edge_id, evidence_id),
    FOREIGN KEY (snapshot_id, edge_id) REFERENCES edges(snapshot_id, id) ON DELETE CASCADE,
    FOREIGN KEY (snapshot_id, evidence_id) REFERENCES evidence(snapshot_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS repository_snapshot_freshness (
    snapshot_id TEXT NOT NULL REFERENCES repo_snapshots(id) ON DELETE CASCADE,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    checkout_id TEXT NOT NULL REFERENCES repository_checkouts(id),
    head_commit TEXT,
    manifest_hash TEXT NOT NULL,
    state TEXT NOT NULL,
    reason TEXT,
    PRIMARY KEY (snapshot_id, repo_id, checkout_id)
);

CREATE TABLE IF NOT EXISTS extractor_runs (
    id TEXT PRIMARY KEY,
    snapshot_id TEXT NOT NULL REFERENCES repo_snapshots(id) ON DELETE CASCADE,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    checkout_id TEXT REFERENCES repository_checkouts(id),
    extractor TEXT NOT NULL,
    extractor_version TEXT NOT NULL,
    status TEXT NOT NULL,
    discovered_files INTEGER NOT NULL DEFAULT 0,
    parsed_files INTEGER NOT NULL DEFAULT 0,
    skipped_files INTEGER NOT NULL DEFAULT 0,
    elapsed_ms INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS extractor_runs_snapshot_idx
ON extractor_runs(snapshot_id);

CREATE TABLE IF NOT EXISTS audit_events (
    id TEXT PRIMARY KEY,
    workspace_name TEXT REFERENCES workspaces(name) ON DELETE SET NULL,
    operation TEXT NOT NULL,
    outcome TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    details TEXT
);

CREATE TABLE IF NOT EXISTS artifact_fingerprints (
    snapshot_id TEXT NOT NULL REFERENCES repo_snapshots(id) ON DELETE CASCADE,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    checkout_id TEXT NOT NULL REFERENCES repository_checkouts(id),
    path_encoding TEXT NOT NULL,
    relative_path BLOB NOT NULL,
    path_display TEXT NOT NULL,
    extractor TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    PRIMARY KEY (
        snapshot_id,
        repo_id,
        checkout_id,
        path_encoding,
        relative_path,
        extractor
    )
);

CREATE INDEX IF NOT EXISTS artifact_fingerprints_checkout_idx
ON artifact_fingerprints(checkout_id, extractor);

CREATE TABLE IF NOT EXISTS extractor_run_inputs (
    run_id TEXT NOT NULL REFERENCES extractor_runs(id) ON DELETE CASCADE,
    repo_id TEXT NOT NULL,
    checkout_id TEXT NOT NULL,
    path_encoding TEXT NOT NULL,
    relative_path BLOB NOT NULL,
    extractor TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    PRIMARY KEY (
        run_id,
        repo_id,
        checkout_id,
        path_encoding,
        relative_path,
        extractor
    )
);

CREATE TABLE IF NOT EXISTS extractor_batches (
    snapshot_id TEXT NOT NULL REFERENCES repo_snapshots(id) ON DELETE CASCADE,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    checkout_id TEXT NOT NULL REFERENCES repository_checkouts(id),
    path_encoding TEXT NOT NULL,
    relative_path BLOB NOT NULL,
    path_display TEXT NOT NULL,
    extractor TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    extractor_version TEXT NOT NULL,
    budget_fingerprint TEXT NOT NULL,
    source_was_lossy INTEGER NOT NULL CHECK (source_was_lossy IN (0, 1)),
    output_count INTEGER NOT NULL CHECK (output_count >= 0),
    payload BLOB NOT NULL CHECK (json_valid(CAST(payload AS TEXT))),
    PRIMARY KEY (
        snapshot_id,
        repo_id,
        checkout_id,
        path_encoding,
        relative_path,
        extractor
    )
);

CREATE INDEX IF NOT EXISTS extractor_batches_source_idx
ON extractor_batches(repo_id, checkout_id, extractor);

CREATE TABLE IF NOT EXISTS community_snapshots (
    snapshot_id TEXT PRIMARY KEY REFERENCES repo_snapshots(id) ON DELETE CASCADE,
    engine_version TEXT NOT NULL,
    algorithm TEXT NOT NULL
        CHECK (json_valid(algorithm))
        CHECK (length(CAST(algorithm AS BLOB)) <= 256),
    config_json TEXT NOT NULL
        CHECK (json_valid(config_json))
        CHECK (length(CAST(config_json AS BLOB)) <= 65536)
);

CREATE INDEX IF NOT EXISTS community_snapshots_snapshot_idx
ON community_snapshots(snapshot_id);

CREATE TABLE IF NOT EXISTS communities (
    snapshot_id TEXT NOT NULL,
    id TEXT NOT NULL,
    label TEXT NOT NULL,
    metrics_json TEXT NOT NULL
        CHECK (json_valid(metrics_json))
        CHECK (length(CAST(metrics_json AS BLOB)) <= 65536),
    explanation_json TEXT NOT NULL
        CHECK (json_valid(explanation_json))
        CHECK (length(CAST(explanation_json AS BLOB)) <= 1048576),
    PRIMARY KEY (snapshot_id, id),
    FOREIGN KEY (snapshot_id) REFERENCES community_snapshots(snapshot_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS communities_snapshot_idx
ON communities(snapshot_id);

CREATE INDEX IF NOT EXISTS communities_snapshot_community_idx
ON communities(snapshot_id, id);

CREATE TABLE IF NOT EXISTS community_memberships (
    snapshot_id TEXT NOT NULL,
    community_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    member_order INTEGER NOT NULL CHECK (member_order >= 0),
    PRIMARY KEY (snapshot_id, community_id, node_id),
    UNIQUE (snapshot_id, community_id, member_order),
    FOREIGN KEY (snapshot_id, community_id)
        REFERENCES communities(snapshot_id, id) ON DELETE CASCADE,
    FOREIGN KEY (snapshot_id, node_id)
        REFERENCES nodes(snapshot_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS community_memberships_snapshot_idx
ON community_memberships(snapshot_id);

CREATE INDEX IF NOT EXISTS community_memberships_community_idx
ON community_memberships(snapshot_id, community_id);

CREATE INDEX IF NOT EXISTS community_memberships_member_idx
ON community_memberships(snapshot_id, node_id);

CREATE TABLE IF NOT EXISTS manual_links (
    snapshot_id TEXT NOT NULL,
    id TEXT NOT NULL
        CHECK (length(CAST(id AS BLOB)) BETWEEN 1 AND 2048)
        CHECK (length(trim(id)) > 0),
    source_node_id TEXT NOT NULL
        CHECK (length(CAST(source_node_id AS BLOB)) BETWEEN 1 AND 2048)
        CHECK (length(trim(source_node_id)) > 0),
    target_node_id TEXT NOT NULL
        CHECK (length(CAST(target_node_id AS BLOB)) BETWEEN 1 AND 2048)
        CHECK (length(trim(target_node_id)) > 0),
    kind TEXT NOT NULL
        CHECK (length(CAST(kind AS BLOB)) BETWEEN 1 AND 256)
        CHECK (length(trim(kind)) > 0),
    disposition TEXT NOT NULL
        CHECK (disposition IN ('active', 'suppression')),
    reason TEXT NOT NULL
        CHECK (length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096)
        CHECK (length(trim(reason)) > 0),
    decision_json BLOB NOT NULL
        CHECK (length(decision_json) BETWEEN 2 AND 262144)
        CHECK (json_valid(CAST(decision_json AS TEXT))),
    config_version INTEGER NOT NULL
        CHECK (config_version BETWEEN 1 AND 2147483647),
    PRIMARY KEY (snapshot_id, id),
    UNIQUE (snapshot_id, source_node_id, target_node_id, kind),
    FOREIGN KEY (snapshot_id) REFERENCES repo_snapshots(id) ON DELETE CASCADE,
    FOREIGN KEY (snapshot_id, source_node_id)
        REFERENCES nodes(snapshot_id, id) ON DELETE CASCADE,
    FOREIGN KEY (snapshot_id, target_node_id)
        REFERENCES nodes(snapshot_id, id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS manual_links_source_idx
ON manual_links(snapshot_id, source_node_id);

CREATE INDEX IF NOT EXISTS manual_links_target_idx
ON manual_links(snapshot_id, target_node_id);

CREATE INDEX IF NOT EXISTS manual_links_disposition_idx
ON manual_links(snapshot_id, disposition, kind);

CREATE TABLE IF NOT EXISTS provider_capabilities (
    workspace_name TEXT NOT NULL
        REFERENCES workspaces(name) ON DELETE CASCADE,
    repo_id TEXT NOT NULL
        REFERENCES repositories(id) ON DELETE CASCADE,
    provider TEXT NOT NULL
        CHECK (length(CAST(provider AS BLOB)) BETWEEN 1 AND 256)
        CHECK (length(trim(provider)) > 0),
    provider_version TEXT NOT NULL
        CHECK (length(CAST(provider_version AS BLOB)) BETWEEN 1 AND 256)
        CHECK (length(trim(provider_version)) > 0),
    capabilities_json TEXT NOT NULL
        CHECK (json_valid(capabilities_json))
        CHECK (json_type(capabilities_json) = 'array')
        CHECK (length(CAST(capabilities_json AS BLOB)) <= 65536),
    observed_at_unix_ms INTEGER NOT NULL
        CHECK (observed_at_unix_ms >= 0),
    PRIMARY KEY (workspace_name, repo_id, provider, provider_version)
) STRICT, WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS provider_capabilities_repo_provider_idx
ON provider_capabilities(repo_id, provider, provider_version);

CREATE TRIGGER IF NOT EXISTS provider_capabilities_require_registration
BEFORE INSERT ON provider_capabilities
WHEN NOT EXISTS (
    SELECT 1
    FROM workspace_repositories
    WHERE workspace_name = NEW.workspace_name
      AND repo_id = NEW.repo_id
)
BEGIN
    SELECT RAISE(ABORT, 'provider capability repository is not registered in workspace');
END;

CREATE TRIGGER IF NOT EXISTS provider_capabilities_update_require_registration
BEFORE UPDATE OF workspace_name, repo_id ON provider_capabilities
WHEN NOT EXISTS (
    SELECT 1
    FROM workspace_repositories
    WHERE workspace_name = NEW.workspace_name
      AND repo_id = NEW.repo_id
)
BEGIN
    SELECT RAISE(ABORT, 'provider capability repository is not registered in workspace');
END;

CREATE TABLE IF NOT EXISTS query_cache (
    workspace_name TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    input_fingerprint TEXT NOT NULL
        CHECK (length(CAST(input_fingerprint AS BLOB)) BETWEEN 1 AND 256)
        CHECK (length(trim(input_fingerprint)) > 0),
    result_summary_json BLOB NOT NULL
        CHECK (length(result_summary_json) BETWEEN 1 AND 1048576)
        CHECK (json_valid(CAST(result_summary_json AS TEXT))),
    stored_at_unix_ms INTEGER NOT NULL
        CHECK (stored_at_unix_ms >= 0),
    expires_at_unix_ms INTEGER
        CHECK (
            expires_at_unix_ms IS NULL
            OR expires_at_unix_ms >= stored_at_unix_ms
        ),
    PRIMARY KEY (workspace_name, snapshot_id, input_fingerprint),
    FOREIGN KEY (snapshot_id, workspace_name)
        REFERENCES repo_snapshots(id, workspace_name) ON DELETE CASCADE
) STRICT;

CREATE INDEX IF NOT EXISTS query_cache_snapshot_idx
ON query_cache(snapshot_id);

CREATE INDEX IF NOT EXISTS query_cache_workspace_expiry_idx
ON query_cache(workspace_name, expires_at_unix_ms);

CREATE INDEX IF NOT EXISTS query_cache_workspace_stored_idx
ON query_cache(workspace_name, stored_at_unix_ms DESC);

CREATE TRIGGER IF NOT EXISTS query_cache_bound_workspace_entries
AFTER INSERT ON query_cache
BEGIN
    DELETE FROM query_cache
    WHERE rowid IN (
        SELECT rowid
        FROM query_cache
        WHERE workspace_name = NEW.workspace_name
        ORDER BY stored_at_unix_ms DESC, snapshot_id DESC, input_fingerprint DESC
        LIMIT -1 OFFSET 1024
    );
END;

CREATE TRIGGER IF NOT EXISTS repo_snapshots_delete_fts
AFTER DELETE ON repo_snapshots
BEGIN
    DELETE FROM nodes_fts WHERE snapshot_id = OLD.id;
END;
