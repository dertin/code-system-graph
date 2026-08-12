//! Deterministic graph conversion for extraction documents.

use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};

use crate::{
    DataAccessObservation, DataAccessRole, DataArtifactKind, DataArtifactReference, DataArtifactReferenceKind, DataDocument, DatabaseForeignKey, DatabaseTable, DeploymentKind, DeploymentUnit, DocumentKind, DocumentRecord, DocumentationDocument, ExplicitReference, ExplicitReferenceKind, InfrastructureDocument, InfrastructureEvidence, InfrastructureResource, InfrastructureResourceKind, OwnershipRule, SafeConfigDocument
};

/// Graph-ready facts assembled from supported extraction documents.
#[derive(Debug, Clone, Default)]
pub struct ExtractionGraphFacts {
    /// Repository, artifact, data, deployment, configuration, document, and owner nodes.
    pub nodes: Vec<Node>,
    /// Directly evidenced extraction relationships.
    pub edges: Vec<Edge>,
    /// Value-free evidence supporting the emitted relationships.
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone)]
struct TableInfo {
    node: Node,
    database: Option<String>,
    schema: Option<String>,
    name: String,
    columns: BTreeMap<String, Node>,
}

#[derive(Debug, Clone)]
struct PendingAccess {
    repo_id: RepoId,
    source_path: String,
    content_hash: String,
    access: DataAccessObservation,
}

#[derive(Debug, Clone)]
struct PendingForeignKey {
    repo_id: RepoId,
    source_path: String,
    content_hash: String,
    source_table_key: String,
    foreign_key: DatabaseForeignKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationDirection {
    Forward,
    Up,
    Down,
}

#[derive(Debug, Clone)]
struct MigrationArtifact {
    repo_id: RepoId,
    source_path: String,
    content_hash: String,
    artifact: Node,
    order_hint: Option<u64>,
    evidence_line: u32,
    direction: MigrationDirection,
}

#[derive(Debug, Clone)]
struct DeploymentInfo {
    repo_id: RepoId,
    node: Node,
    aliases: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct ResourceInfo {
    node: Node,
    aliases: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct PendingDependency {
    source: Node,
    target: String,
    repo_id: RepoId,
    source_path: String,
    content_hash: String,
    line: Option<u32>,
}

#[derive(Debug, Clone)]
struct PendingReference {
    document: Node,
    repo_id: RepoId,
    source_path: String,
    content_hash: String,
    reference: ExplicitReference,
}

#[derive(Default)]
struct GraphBuilder {
    facts: ExtractionGraphFacts,
}

impl GraphBuilder {
    fn node(&mut self, node: Node) {
        self.facts.nodes.push(node);
    }

    fn evidence(&mut self, evidence: Evidence) {
        self.facts.evidence.push(evidence);
    }

    fn edge(&mut self, source: &Node, target: &Node, kind: EdgeKind, evidence: &Evidence) {
        if source.id == target.id {
            return;
        }
        let key = format!("{}:{kind:?}:{}", source.id.as_str(), target.id.as_str());
        self.facts.edges.push(Edge {
            id: EdgeId::new(stable_id("edge", &key)),
            source: source.id.clone(),
            target: target.id.clone(),
            kind,
            confidence: 1.0,
            status: EpistemicStatus::Confirmed,
            evidence: vec![evidence.id.clone()],
        });
    }

    fn finish(mut self) -> ExtractionGraphFacts {
        let mut nodes = BTreeMap::new();
        for node in self.facts.nodes {
            nodes.entry(node.id.clone()).or_insert(node);
        }
        self.facts.nodes = nodes.into_values().collect();

        let mut edges: BTreeMap<EdgeId, Edge> = BTreeMap::new();
        for edge in self.facts.edges {
            if let Some(existing) = edges.get_mut(&edge.id) {
                existing.evidence.extend(edge.evidence);
                existing.confidence = existing.confidence.max(edge.confidence);
            } else {
                edges.insert(edge.id.clone(), edge);
            }
        }
        self.facts.edges = edges
            .into_values()
            .map(|mut edge| {
                edge.evidence.sort();
                edge.evidence.dedup();
                edge
            })
            .collect();

        let mut evidence = BTreeMap::new();
        for item in self.facts.evidence {
            evidence.entry(item.id.clone()).or_insert(item);
        }
        self.facts.evidence = evidence.into_values().collect();
        self.facts
    }
}

/// Converts all supported extractor outputs into deterministic, ambiguity-safe graph facts.
///
/// Only exact, unique targets are linked. Dynamic or incomplete observations that lack a concrete
/// target remain unlinked, and retained evidence contains locators and static notes rather than
/// source or configuration values.
#[must_use]
pub fn documents_to_graph(
    data_inputs: &[(&RepoId, &str, &str, &DataDocument)],
    infrastructure_inputs: &[(&RepoId, &str, &str, &InfrastructureDocument)],
    documentation_inputs: &[(&RepoId, &str, &str, &DocumentationDocument)],
    config_inputs: &[(&RepoId, &str, &str, &SafeConfigDocument)],
    known_nodes: &[Node],
    repository_aliases: &[(&str, &RepoId)],
) -> ExtractionGraphFacts {
    let mut builder = GraphBuilder::default();

    append_config_documents(&mut builder, config_inputs);
    let tables = append_data_documents(&mut builder, data_inputs);
    append_infrastructure_documents(&mut builder, infrastructure_inputs);
    append_documentation_documents(
        &mut builder,
        documentation_inputs,
        known_nodes,
        repository_aliases,
    );

    // Data observations can resolve across documents, so they are linked after every declaration
    // has entered the exact-match table index.
    link_data_observations(&mut builder, data_inputs, &tables);
    link_data_artifact_references(&mut builder, data_inputs, &tables);
    builder.finish()
}

fn append_config_documents(
    builder: &mut GraphBuilder,
    inputs: &[(&RepoId, &str, &str, &SafeConfigDocument)],
) {
    for (repo_id, source_path, content_hash, document) in inputs {
        if document.keys.is_empty() {
            continue;
        }
        let artifact = append_artifact(
            builder,
            repo_id,
            source_path,
            content_hash,
            "code-system-graph.extraction.config",
            "configuration artifact",
        );
        for key in &document.keys {
            let node = config_node(repo_id, &key.scope.join("."), &key.name);
            let evidence = extraction_evidence(
                repo_id,
                source_path,
                content_hash,
                line_from_usize(key.line),
                line_from_usize(key.line),
                "code-system-graph.extraction.config",
                "configuration key declaration",
            );
            builder.edge(&artifact, &node, EdgeKind::Configures, &evidence);
            builder.node(node);
            builder.evidence(evidence);
        }
    }
}

fn append_data_documents(
    builder: &mut GraphBuilder,
    inputs: &[(&RepoId, &str, &str, &DataDocument)],
) -> BTreeMap<String, TableInfo> {
    let mut tables = BTreeMap::new();
    let mut migrations = Vec::new();
    for (repo_id, source_path, content_hash, document) in inputs {
        if document.tables.is_empty()
            && document.accesses.is_empty()
            && document.references.is_empty()
            && document.artifact_kind != DataArtifactKind::SqlMigration
        {
            continue;
        }
        let artifact = append_artifact(
            builder,
            repo_id,
            source_path,
            content_hash,
            "code-system-graph.extraction.data",
            "data artifact",
        );
        if let Some(migration) = &document.migration {
            migrations.push(MigrationArtifact {
                repo_id: (*repo_id).clone(),
                source_path: (*source_path).to_owned(),
                content_hash: (*content_hash).to_owned(),
                artifact: artifact.clone(),
                order_hint: migration.order_hint,
                evidence_line: migration.evidence.get(),
                direction: migration_direction(source_path),
            });
        }
        for table in &document.tables {
            let info = table_info(document, table);
            let table_evidence = extraction_evidence(
                repo_id,
                source_path,
                content_hash,
                Some(table.evidence.get()),
                Some(table.evidence.get()),
                "code-system-graph.extraction.data",
                "database table declaration",
            );
            builder.edge(&artifact, &info.node, EdgeKind::Contains, &table_evidence);
            builder.node(info.node.clone());
            builder.evidence(table_evidence.clone());

            if let Some(database) = &info.database {
                let database = database_node(database);
                builder.edge(&database, &info.node, EdgeKind::Contains, &table_evidence);
                builder.node(database);
            }
            for column in &table.columns {
                let Some(column_node) = info.columns.get(&column.name) else {
                    continue;
                };
                let evidence = extraction_evidence(
                    repo_id,
                    source_path,
                    content_hash,
                    Some(column.evidence.get()),
                    Some(column.evidence.get()),
                    "code-system-graph.extraction.data",
                    "database column declaration",
                );
                builder.edge(&info.node, column_node, EdgeKind::Contains, &evidence);
                builder.node(column_node.clone());
                builder.evidence(evidence);
            }
            tables
                .entry(info.node.stable_key.clone())
                .and_modify(|existing: &mut TableInfo| {
                    existing.columns.extend(info.columns.clone());
                })
                .or_insert(info);
        }
    }
    link_migration_artifacts(builder, &migrations);
    tables
}

fn link_migration_artifacts(builder: &mut GraphBuilder, migrations: &[MigrationArtifact]) {
    let mut pairs: BTreeMap<(RepoId, String, String), Vec<&MigrationArtifact>> = BTreeMap::new();
    let mut ordered: BTreeMap<(RepoId, String), BTreeMap<u64, Vec<&MigrationArtifact>>> =
        BTreeMap::new();

    for migration in migrations {
        let (directory, family) = migration_family(&migration.source_path);
        pairs
            .entry((migration.repo_id.clone(), directory.clone(), family))
            .or_default()
            .push(migration);
        if migration.direction != MigrationDirection::Down
            && let Some(order_hint) = migration.order_hint
        {
            ordered
                .entry((migration.repo_id.clone(), directory))
                .or_default()
                .entry(order_hint)
                .or_default()
                .push(migration);
        }
    }

    for candidates in pairs.values() {
        let up = exactly_one_migration(
            candidates
                .iter()
                .filter(|migration| migration.direction == MigrationDirection::Up)
                .copied(),
        );
        let down = exactly_one_migration(
            candidates
                .iter()
                .filter(|migration| migration.direction == MigrationDirection::Down)
                .copied(),
        );
        let (Some(up), Some(down)) = (up, down) else {
            continue;
        };
        let evidence = extraction_evidence(
            &down.repo_id,
            &down.source_path,
            &down.content_hash,
            Some(down.evidence_line),
            Some(down.evidence_line),
            "code-system-graph.extraction.data",
            "reversible migration pair",
        );
        builder.edge(&down.artifact, &up.artifact, EdgeKind::Reverts, &evidence);
        builder.evidence(evidence);
    }

    for revisions in ordered.values() {
        let migrations = revisions
            .values()
            .map(|candidates| exactly_one_migration(candidates.iter().copied()))
            .collect::<Vec<_>>();
        for pair in migrations.windows(2) {
            let [Some(previous), Some(next)] = pair else {
                continue;
            };
            let evidence = extraction_evidence(
                &next.repo_id,
                &next.source_path,
                &next.content_hash,
                Some(next.evidence_line),
                Some(next.evidence_line),
                "code-system-graph.extraction.data",
                "migration order",
            );
            builder.edge(
                &previous.artifact,
                &next.artifact,
                EdgeKind::Precedes,
                &evidence,
            );
            builder.evidence(evidence);
        }
    }
}

fn exactly_one_migration<'a>(
    mut candidates: impl Iterator<Item = &'a MigrationArtifact>,
) -> Option<&'a MigrationArtifact> {
    let candidate = candidates.next()?;
    candidates.next().is_none().then_some(candidate)
}

fn migration_direction(source_path: &str) -> MigrationDirection {
    if source_path.ends_with(".up.sql") {
        MigrationDirection::Up
    } else if source_path.ends_with(".down.sql") {
        MigrationDirection::Down
    } else {
        MigrationDirection::Forward
    }
}

fn migration_family(source_path: &str) -> (String, String) {
    let (directory, filename) = source_path
        .rsplit_once('/')
        .map_or(("", source_path), |(directory, filename)| {
            (directory, filename)
        });
    let family = filename
        .strip_suffix(".up.sql")
        .or_else(|| filename.strip_suffix(".down.sql"))
        .or_else(|| filename.strip_suffix(".sql"))
        .unwrap_or(filename);
    (directory.to_owned(), family.to_owned())
}

fn link_data_observations(
    builder: &mut GraphBuilder,
    inputs: &[(&RepoId, &str, &str, &DataDocument)],
    tables: &BTreeMap<String, TableInfo>,
) {
    let mut accesses = Vec::new();
    let mut foreign_keys = Vec::new();
    for (repo_id, source_path, content_hash, document) in inputs {
        accesses.extend(
            document
                .accesses
                .iter()
                .cloned()
                .map(|access| PendingAccess {
                    repo_id: (*repo_id).clone(),
                    source_path: (*source_path).to_owned(),
                    content_hash: (*content_hash).to_owned(),
                    access,
                }),
        );
        for table in &document.tables {
            let info = table_info(document, table);
            foreign_keys.extend(table.foreign_keys.iter().cloned().map(|foreign_key| {
                PendingForeignKey {
                    repo_id: (*repo_id).clone(),
                    source_path: (*source_path).to_owned(),
                    content_hash: (*content_hash).to_owned(),
                    source_table_key: info.node.stable_key.clone(),
                    foreign_key,
                }
            }));
        }
    }
    link_accesses(builder, accesses, tables);
    link_foreign_keys(builder, foreign_keys, tables);
}

fn link_data_artifact_references(
    builder: &mut GraphBuilder,
    inputs: &[(&RepoId, &str, &str, &DataDocument)],
    tables: &BTreeMap<String, TableInfo>,
) {
    let mut referenced_accesses = Vec::new();
    for (repo_id, source_path, content_hash, document) in inputs {
        for reference in &document.references {
            let effective_path = effective_data_reference_path(reference, repo_id, inputs);
            let targets = inputs
                .iter()
                .filter(|(candidate_repo, candidate_path, _, candidate)| {
                    *candidate_repo == *repo_id
                        && match reference.kind {
                            DataArtifactReferenceKind::QueryFile => {
                                *candidate_path == reference.path
                                    && candidate.artifact_kind == DataArtifactKind::SqlQueryFile
                            }
                            DataArtifactReferenceKind::MigrationDirectory
                            | DataArtifactReferenceKind::SqlxDefaultMigrationDirectory => {
                                matches!(
                                    candidate.artifact_kind,
                                    DataArtifactKind::SqlMigration
                                        | DataArtifactKind::DeclarativeSqlSchema
                                        | DataArtifactKind::SqlQueryFile
                                ) && path_is_within(candidate_path, &effective_path)
                            }
                        }
                })
                .collect::<Vec<_>>();
            if reference.kind == DataArtifactReferenceKind::QueryFile && targets.len() != 1 {
                continue;
            }
            let source_artifact = append_artifact(
                builder,
                repo_id,
                source_path,
                content_hash,
                "code-system-graph.extraction.data",
                "data source artifact",
            );
            let source = reference.owner.as_deref().map_or_else(
                || source_artifact.clone(),
                |owner| data_symbol_node(repo_id, source_path, owner),
            );
            for (target_repo, target_path, target_hash, target_document) in targets {
                let target_artifact = append_artifact(
                    builder,
                    target_repo,
                    target_path,
                    target_hash,
                    "code-system-graph.extraction.data",
                    "referenced data artifact",
                );
                let evidence = extraction_evidence(
                    repo_id,
                    source_path,
                    content_hash,
                    Some(reference.evidence.get()),
                    Some(reference.evidence.get()),
                    "code-system-graph.extraction.data",
                    match reference.kind {
                        DataArtifactReferenceKind::QueryFile => "SQLx query file reference",
                        DataArtifactReferenceKind::MigrationDirectory => {
                            "SQLx migration directory reference"
                        }
                        DataArtifactReferenceKind::SqlxDefaultMigrationDirectory => {
                            "SQLx configured migration directory reference"
                        }
                    },
                );
                builder.edge(&source, &target_artifact, EdgeKind::Consumes, &evidence);
                builder.node(source.clone());
                builder.evidence(evidence);

                if reference.kind == DataArtifactReferenceKind::QueryFile {
                    referenced_accesses.extend(target_document.accesses.iter().map(|access| {
                        let mut access = access.clone();
                        access.owner.clone_from(&reference.owner);
                        access.evidence = reference.evidence;
                        PendingAccess {
                            repo_id: (*repo_id).clone(),
                            source_path: (*source_path).to_owned(),
                            content_hash: (*content_hash).to_owned(),
                            access,
                        }
                    }));
                }
            }
        }
    }
    link_accesses(builder, referenced_accesses, tables);
}

fn effective_data_reference_path(
    reference: &DataArtifactReference,
    repo_id: &RepoId,
    inputs: &[(&RepoId, &str, &str, &DataDocument)],
) -> String {
    if reference.kind != DataArtifactReferenceKind::SqlxDefaultMigrationDirectory {
        return reference.path.clone();
    }
    inputs
        .iter()
        .find(|(candidate_repo, candidate_path, _, candidate)| {
            *candidate_repo == repo_id
                && candidate.artifact_kind == DataArtifactKind::SqlxConfiguration
                && *candidate_path == sqlx_configuration_path(&reference.path)
        })
        .and_then(|(_, _, _, configuration)| {
            configuration
                .references
                .iter()
                .find(|candidate| candidate.kind == DataArtifactReferenceKind::MigrationDirectory)
                .map(|candidate| candidate.path.clone())
        })
        .unwrap_or_else(|| join_portable_path(&reference.path, "migrations"))
}

fn path_is_within(candidate: &str, directory: &str) -> bool {
    candidate == directory
        || candidate
            .strip_prefix(directory)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn sqlx_configuration_path(crate_root: &str) -> String {
    join_portable_path(crate_root, "sqlx.toml")
}

fn join_portable_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), child)
    }
}

fn link_accesses(
    builder: &mut GraphBuilder,
    accesses: Vec<PendingAccess>,
    tables: &BTreeMap<String, TableInfo>,
) {
    let mut model_tables = BTreeMap::<String, BTreeSet<String>>::new();
    for pending in &accesses {
        if pending.access.role != DataAccessRole::ModelBinding {
            continue;
        }
        let Some(model) = pending.access.owner.as_ref() else {
            continue;
        };
        model_tables
            .entry(model.clone())
            .or_default()
            .insert(pending.access.table.clone());
    }
    for pending in accesses {
        let Some(owner) = pending.access.owner.as_deref() else {
            continue;
        };
        let resolved_model_table = pending
            .access
            .model
            .as_ref()
            .and_then(|model| model_tables.get(model).cloned().and_then(exactly_one));
        let target_name = if pending.access.table.is_empty() {
            let Some(table) = resolved_model_table.as_deref() else {
                continue;
            };
            table
        } else {
            &pending.access.table
        };
        let Some(target) = resolve_table(tables, target_name) else {
            continue;
        };
        let symbol = data_symbol_node(&pending.repo_id, &pending.source_path, owner);
        let evidence = extraction_evidence(
            &pending.repo_id,
            &pending.source_path,
            &pending.content_hash,
            Some(pending.access.evidence.get()),
            Some(pending.access.evidence.get()),
            "code-system-graph.extraction.data",
            "database access observation",
        );
        match pending.access.role {
            DataAccessRole::Reader => {
                builder.edge(&symbol, &target.node, EdgeKind::ReadsTable, &evidence);
            }
            DataAccessRole::Writer => {
                builder.edge(&symbol, &target.node, EdgeKind::WritesTable, &evidence);
            }
            DataAccessRole::ModelBinding => {
                builder.edge(&target.node, &symbol, EdgeKind::ImplementedBy, &evidence);
            }
            DataAccessRole::Declaration => continue,
        }
        builder.node(symbol);
        builder.evidence(evidence);
    }
}

fn link_foreign_keys(
    builder: &mut GraphBuilder,
    foreign_keys: Vec<PendingForeignKey>,
    tables: &BTreeMap<String, TableInfo>,
) {
    for pending in foreign_keys {
        let Some(source) = tables.get(&pending.source_table_key) else {
            continue;
        };
        let Some(target) = resolve_table(tables, &pending.foreign_key.referenced_table) else {
            continue;
        };
        let evidence = extraction_evidence(
            &pending.repo_id,
            &pending.source_path,
            &pending.content_hash,
            Some(pending.foreign_key.evidence.get()),
            Some(pending.foreign_key.evidence.get()),
            "code-system-graph.extraction.data",
            "foreign key declaration",
        );
        if pending.foreign_key.referenced_columns.is_empty() {
            builder.edge(&source.node, &target.node, EdgeKind::Consumes, &evidence);
            builder.evidence(evidence);
            continue;
        }
        if pending.foreign_key.columns.len() != pending.foreign_key.referenced_columns.len() {
            continue;
        }
        let pairs = pending
            .foreign_key
            .columns
            .iter()
            .zip(&pending.foreign_key.referenced_columns)
            .filter_map(|(local, referenced)| {
                Some((source.columns.get(local)?, target.columns.get(referenced)?))
            })
            .collect::<Vec<_>>();
        if pairs.len() != pending.foreign_key.columns.len() {
            continue;
        }
        for (local, referenced) in pairs {
            builder.edge(local, referenced, EdgeKind::Consumes, &evidence);
        }
        builder.evidence(evidence);
    }
}

fn append_infrastructure_documents(
    builder: &mut GraphBuilder,
    inputs: &[(&RepoId, &str, &str, &InfrastructureDocument)],
) {
    let mut deployments = Vec::new();
    let mut resources = Vec::new();
    let mut dependencies = Vec::new();

    for (repo_id, source_path, content_hash, document) in inputs {
        if document.deployment_units.is_empty()
            && document.resources.is_empty()
            && document.environment_keys.is_empty()
        {
            continue;
        }
        let artifact = append_artifact(
            builder,
            repo_id,
            source_path,
            content_hash,
            "code-system-graph.extraction.infrastructure",
            "infrastructure artifact",
        );
        append_deployment_units(
            builder,
            repo_id,
            source_path,
            content_hash,
            &document.deployment_units,
            &mut deployments,
            &mut dependencies,
        );
        append_infrastructure_resources(
            builder,
            repo_id,
            source_path,
            content_hash,
            &artifact,
            &document.resources,
            &mut deployments,
            &mut resources,
            &mut dependencies,
        );
        append_document_environment_keys(
            builder,
            repo_id,
            source_path,
            content_hash,
            &artifact,
            &document.environment_keys,
        );
    }
    link_infrastructure_dependencies(builder, &deployments, &resources, dependencies);
}

fn link_infrastructure_dependencies(
    builder: &mut GraphBuilder,
    deployments: &[DeploymentInfo],
    resources: &[ResourceInfo],
    dependencies: Vec<PendingDependency>,
) {
    for dependency in dependencies {
        let target = resolve_infrastructure_target(deployments, resources, &dependency.target);
        let Some(target) = target else {
            continue;
        };
        let evidence = extraction_evidence(
            &dependency.repo_id,
            &dependency.source_path,
            &dependency.content_hash,
            dependency.line,
            dependency.line,
            "code-system-graph.extraction.infrastructure",
            "explicit infrastructure dependency",
        );
        builder.edge(
            &dependency.source,
            &target,
            EdgeKind::CallsRemote,
            &evidence,
        );
        builder.node(target);
        builder.evidence(evidence);
    }
}

fn append_deployment_units(
    builder: &mut GraphBuilder,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    units: &[DeploymentUnit],
    deployments: &mut Vec<DeploymentInfo>,
    dependencies: &mut Vec<PendingDependency>,
) {
    for unit in units {
        let technology = deployment_technology(unit.kind);
        let deployment = deployment_node(technology, unit.namespace.as_deref(), &unit.name);
        let line = infrastructure_line(&unit.evidence);
        let evidence = extraction_evidence(
            repo_id,
            source_path,
            content_hash,
            line,
            line,
            "code-system-graph.extraction.infrastructure",
            "deployment declaration",
        );
        let repository = repository_node(repo_id);
        builder.edge(&repository, &deployment, EdgeKind::Deploys, &evidence);
        builder.node(repository);
        builder.node(deployment.clone());
        builder.evidence(evidence.clone());
        for service_name in &unit.service_names {
            let service = service_node(repo_id, service_name);
            builder.edge(&deployment, &service, EdgeKind::Provides, &evidence);
            builder.node(service);
        }
        append_deployment_environment_keys(
            builder,
            repo_id,
            source_path,
            content_hash,
            &deployment,
            &unit.environment_keys,
            line,
        );
        dependencies.extend(unit.dependencies.iter().map(|target| PendingDependency {
            source: deployment.clone(),
            target: target.clone(),
            repo_id: repo_id.clone(),
            source_path: source_path.to_owned(),
            content_hash: content_hash.to_owned(),
            line,
        }));
        let aliases = BTreeSet::from([
            unit.name.clone(),
            deployment.stable_key.clone(),
            deployment
                .stable_key
                .strip_prefix("deployment:")
                .unwrap_or(&deployment.stable_key)
                .to_owned(),
        ]);
        deployments.push(DeploymentInfo {
            repo_id: repo_id.clone(),
            node: deployment,
            aliases,
        });
    }
}

fn append_deployment_environment_keys(
    builder: &mut GraphBuilder,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    deployment: &Node,
    key_names: &[String],
    line: Option<u32>,
) {
    for key_name in key_names {
        let config = resolve_or_create_config(builder, repo_id, &deployment.stable_key, key_name);
        let evidence = extraction_evidence(
            repo_id,
            source_path,
            content_hash,
            line,
            line,
            "code-system-graph.extraction.infrastructure",
            "environment key declaration",
        );
        builder.edge(deployment, &config, EdgeKind::Configures, &evidence);
        builder.node(config);
        builder.evidence(evidence);
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "The helper preserves explicit source provenance."
)]
fn append_infrastructure_resources(
    builder: &mut GraphBuilder,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    artifact: &Node,
    document_resources: &[InfrastructureResource],
    deployments: &mut [DeploymentInfo],
    resources: &mut Vec<ResourceInfo>,
    dependencies: &mut Vec<PendingDependency>,
) {
    for resource in document_resources {
        let line = infrastructure_line(&resource.evidence);
        let node = infrastructure_resource_node(repo_id, resource);
        if let Some(node) = node {
            let evidence = extraction_evidence(
                repo_id,
                source_path,
                content_hash,
                line,
                line,
                "code-system-graph.extraction.infrastructure",
                "infrastructure resource declaration",
            );
            builder.edge(artifact, &node, EdgeKind::Contains, &evidence);
            builder.node(node.clone());
            builder.evidence(evidence);
            let aliases = BTreeSet::from([
                resource.name.clone(),
                format!("{}.{}", resource.resource_type, resource.name),
                format!("{}/{}", resource.resource_type, resource.name),
                node.stable_key.clone(),
            ]);
            resources.push(ResourceInfo {
                node: node.clone(),
                aliases: aliases.clone(),
            });
            for deployment in &mut *deployments {
                if deployment.repo_id == *repo_id && deployment.node.label == resource.name {
                    deployment.aliases.extend(aliases.iter().cloned());
                }
            }
            dependencies.extend(
                resource
                    .dependencies
                    .iter()
                    .map(|target| PendingDependency {
                        source: node.clone(),
                        target: target.clone(),
                        repo_id: repo_id.clone(),
                        source_path: source_path.to_owned(),
                        content_hash: content_hash.to_owned(),
                        line,
                    }),
            );
        }
        append_resource_config_keys(
            builder,
            repo_id,
            source_path,
            content_hash,
            artifact,
            resource,
            line,
        );
    }
}

fn infrastructure_resource_node(
    repo_id: &RepoId,
    resource: &InfrastructureResource,
) -> Option<Node> {
    match resource.kind {
        InfrastructureResourceKind::Service => Some(service_node(repo_id, &resource.name)),
        InfrastructureResourceKind::Database => Some(database_node(&resource.name)),
        InfrastructureResourceKind::Topic | InfrastructureResourceKind::Queue => Some(
            event_channel_node(resource.namespace.as_deref(), &resource.name),
        ),
        InfrastructureResourceKind::Ingress | InfrastructureResourceKind::Other => None,
    }
}

fn append_resource_config_keys(
    builder: &mut GraphBuilder,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    artifact: &Node,
    resource: &InfrastructureResource,
    line: Option<u32>,
) {
    for key_name in &resource.key_names {
        let scope = format!(
            "resource:{}:{}:{}",
            resource.resource_type,
            resource.namespace.as_deref().unwrap_or(""),
            resource.name
        );
        let config = resolve_or_create_config(builder, repo_id, &scope, key_name);
        let evidence = extraction_evidence(
            repo_id,
            source_path,
            content_hash,
            line,
            line,
            "code-system-graph.extraction.infrastructure",
            "infrastructure configuration key",
        );
        builder.edge(artifact, &config, EdgeKind::Configures, &evidence);
        builder.node(config);
        builder.evidence(evidence);
    }
}

fn append_document_environment_keys(
    builder: &mut GraphBuilder,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    artifact: &Node,
    key_names: &[String],
) {
    for key_name in key_names {
        let config = resolve_or_create_config(builder, repo_id, "environment", key_name);
        let evidence = extraction_evidence(
            repo_id,
            source_path,
            content_hash,
            None,
            None,
            "code-system-graph.extraction.infrastructure",
            "environment key declaration",
        );
        builder.edge(artifact, &config, EdgeKind::Configures, &evidence);
        builder.node(config);
        builder.evidence(evidence);
    }
}

fn append_documentation_documents(
    builder: &mut GraphBuilder,
    inputs: &[(&RepoId, &str, &str, &DocumentationDocument)],
    known_nodes: &[Node],
    repository_aliases: &[(&str, &RepoId)],
) {
    let mut references = Vec::new();
    for (repo_id, source_path, content_hash, document) in inputs {
        let artifact = append_artifact(
            builder,
            repo_id,
            source_path,
            content_hash,
            "code-system-graph.extraction.documents",
            "documentation artifact",
        );
        append_document_records(
            builder,
            repo_id,
            source_path,
            content_hash,
            &artifact,
            &document.records,
            &mut references,
        );
        append_ownership_rules(
            builder,
            repo_id,
            source_path,
            content_hash,
            &document.ownership_rules,
        );
    }

    let mut candidates = builder.facts.nodes.clone();
    candidates.extend_from_slice(known_nodes);
    for (alias, repo_id) in repository_aliases {
        if !alias.is_empty() {
            candidates.push(repository_node(repo_id));
        }
    }
    link_document_references(builder, &candidates, repository_aliases, references);
}

fn link_document_references(
    builder: &mut GraphBuilder,
    candidates: &[Node],
    repository_aliases: &[(&str, &RepoId)],
    references: Vec<PendingReference>,
) {
    for pending in references {
        let Some(target) = resolve_reference(
            candidates,
            repository_aliases,
            &pending.repo_id,
            &pending.reference,
        ) else {
            continue;
        };
        let evidence = extraction_evidence(
            &pending.repo_id,
            &pending.source_path,
            &pending.content_hash,
            Some(pending.reference.evidence.start),
            Some(pending.reference.evidence.end),
            "code-system-graph.extraction.documents",
            "explicit document reference",
        );
        if pending.reference.kind == ExplicitReferenceKind::RepositoryDependency {
            let source = repository_node(&pending.repo_id);
            builder.edge(&source, &target, EdgeKind::DependsOnRepository, &evidence);
            builder.node(source);
        } else {
            builder.edge(&pending.document, &target, EdgeKind::Documents, &evidence);
        }
        builder.node(target);
        builder.evidence(evidence);
    }
}

fn append_document_records(
    builder: &mut GraphBuilder,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    artifact: &Node,
    records: &[DocumentRecord],
    references: &mut Vec<PendingReference>,
) {
    for record in records {
        let node = document_node(
            repo_id,
            &record.source_path,
            record.kind,
            record.title.as_deref(),
        );
        let record_evidence = extraction_evidence(
            repo_id,
            source_path,
            content_hash,
            None,
            None,
            "code-system-graph.extraction.documents",
            "document record",
        );
        builder.edge(artifact, &node, EdgeKind::Contains, &record_evidence);
        builder.node(node.clone());
        builder.evidence(record_evidence);
        let catalog_service = (record.kind == DocumentKind::ServiceCatalog)
            .then_some(record.title.as_deref())
            .flatten()
            .map(|name| service_node(repo_id, name));
        if let Some(service) = &catalog_service {
            builder.node(service.clone());
        }
        for owner_name in &record.owners {
            let owner = owner_node(owner_name);
            let owner_range = record
                .references
                .iter()
                .find(|reference| {
                    reference.kind == ExplicitReferenceKind::Owner
                        && reference.target == *owner_name
                })
                .map(|reference| reference.evidence);
            let evidence = extraction_evidence(
                repo_id,
                source_path,
                content_hash,
                owner_range.map(|range| range.start),
                owner_range.map(|range| range.end),
                "code-system-graph.extraction.documents",
                "explicit ownership declaration",
            );
            let ownership_subject = catalog_service.as_ref().unwrap_or(&node);
            builder.edge(ownership_subject, &owner, EdgeKind::OwnedBy, &evidence);
            builder.node(owner);
            builder.evidence(evidence);
        }
        references.extend(
            record
                .references
                .iter()
                .cloned()
                .map(|reference| PendingReference {
                    document: node.clone(),
                    repo_id: repo_id.clone(),
                    source_path: source_path.to_owned(),
                    content_hash: content_hash.to_owned(),
                    reference,
                }),
        );
    }
}

fn append_ownership_rules(
    builder: &mut GraphBuilder,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    rules: &[OwnershipRule],
) {
    let document = document_node(
        repo_id,
        source_path,
        DocumentKind::Codeowners,
        Some("CODEOWNERS"),
    );
    for rule in rules {
        for owner_name in &rule.owners {
            let owner = owner_node(owner_name);
            let evidence = extraction_evidence(
                repo_id,
                source_path,
                content_hash,
                Some(rule.line),
                Some(rule.line),
                "code-system-graph.extraction.documents",
                "CODEOWNERS owner declaration",
            );
            builder.edge(&document, &owner, EdgeKind::OwnedBy, &evidence);
            builder.node(document.clone());
            builder.node(owner);
            builder.evidence(evidence);
        }
    }
}

fn append_artifact(
    builder: &mut GraphBuilder,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    extractor: &str,
    note: &str,
) -> Node {
    let repository = repository_node(repo_id);
    let artifact = artifact_node(repo_id, source_path);
    let evidence = extraction_evidence(
        repo_id,
        source_path,
        content_hash,
        Some(1),
        Some(1),
        extractor,
        note,
    );
    builder.edge(&repository, &artifact, EdgeKind::Contains, &evidence);
    builder.node(repository);
    builder.node(artifact.clone());
    builder.evidence(evidence);
    artifact
}

fn table_info(document: &DataDocument, table: &DatabaseTable) -> TableInfo {
    let database = table
        .database
        .clone()
        .or_else(|| document.database_name.clone());
    let schema = table
        .schema
        .clone()
        .or_else(|| document.schema_name.clone());
    let node = table_node(database.as_deref(), schema.as_deref(), &table.name);
    let columns = table
        .columns
        .iter()
        .map(|column| {
            (
                column.name.clone(),
                column_node(&node.stable_key, &column.name),
            )
        })
        .collect();
    TableInfo {
        node,
        database,
        schema,
        name: table.name.clone(),
        columns,
    }
}

fn resolve_table<'a>(
    tables: &'a BTreeMap<String, TableInfo>,
    target: &str,
) -> Option<&'a TableInfo> {
    let matches = tables
        .values()
        .filter(|table| table_aliases(table).contains(target))
        .map(|table| table.node.id.clone())
        .collect::<BTreeSet<_>>();
    let id = exactly_one(matches)?;
    tables.values().find(|table| table.node.id == id)
}

fn table_aliases(table: &TableInfo) -> BTreeSet<String> {
    let mut aliases = BTreeSet::from([
        table.node.stable_key.clone(),
        table
            .node
            .stable_key
            .strip_prefix("table:")
            .unwrap_or(&table.node.stable_key)
            .to_owned(),
        table.name.clone(),
    ]);
    if let Some(schema) = &table.schema {
        aliases.insert(format!("{schema}.{}", table.name));
    }
    if let Some(database) = &table.database {
        aliases.insert(format!("{database}.{}", table.name));
        if let Some(schema) = &table.schema {
            aliases.insert(format!("{database}.{schema}.{}", table.name));
        }
    }
    aliases
}

fn resolve_or_create_config(
    builder: &GraphBuilder,
    repo_id: &RepoId,
    fallback_scope: &str,
    name: &str,
) -> Node {
    let matches = builder
        .facts
        .nodes
        .iter()
        .filter(|node| {
            node.kind == NodeKind::ConfigKey
                && node.repo_id.as_ref() == Some(repo_id)
                && node.label == name
        })
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    exactly_one(matches)
        .and_then(|id| {
            builder
                .facts
                .nodes
                .iter()
                .find(|node| node.id == id)
                .cloned()
        })
        .unwrap_or_else(|| config_node(repo_id, fallback_scope, name))
}

fn resolve_infrastructure_target(
    deployments: &[DeploymentInfo],
    resources: &[ResourceInfo],
    target: &str,
) -> Option<Node> {
    let deployment_matches = deployments
        .iter()
        .filter(|deployment| deployment.aliases.contains(target))
        .map(|deployment| deployment.node.id.clone())
        .collect::<BTreeSet<_>>();
    if !deployment_matches.is_empty() {
        let id = exactly_one(deployment_matches)?;
        return deployments
            .iter()
            .find(|deployment| deployment.node.id == id)
            .map(|deployment| deployment.node.clone());
    }
    let resource_matches = resources
        .iter()
        .filter(|resource| resource.aliases.contains(target))
        .map(|resource| resource.node.id.clone())
        .collect::<BTreeSet<_>>();
    let id = exactly_one(resource_matches)?;
    resources
        .iter()
        .find(|resource| resource.node.id == id)
        .map(|resource| resource.node.clone())
}

fn resolve_reference(
    candidates: &[Node],
    repository_aliases: &[(&str, &RepoId)],
    repo_id: &RepoId,
    reference: &ExplicitReference,
) -> Option<Node> {
    let matches = candidates
        .iter()
        .filter(|node| {
            reference_matches(
                node,
                repository_aliases,
                repo_id,
                reference.kind,
                &reference.target,
            )
        })
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let id = exactly_one(matches)?;
    candidates.iter().find(|node| node.id == id).cloned()
}

fn reference_matches(
    node: &Node,
    repository_aliases: &[(&str, &RepoId)],
    repo_id: &RepoId,
    kind: ExplicitReferenceKind,
    target: &str,
) -> bool {
    let expected = match kind {
        ExplicitReferenceKind::Repository | ExplicitReferenceKind::RepositoryDependency => {
            &[NodeKind::Repository][..]
        }
        ExplicitReferenceKind::Service => &[NodeKind::Service][..],
        ExplicitReferenceKind::HttpContract => &[NodeKind::HttpOperation][..],
        ExplicitReferenceKind::EventChannel => &[NodeKind::EventChannel][..],
        ExplicitReferenceKind::GraphqlOperation => &[NodeKind::GraphqlOperation][..],
        ExplicitReferenceKind::RpcMethod => &[NodeKind::RpcMethod][..],
        ExplicitReferenceKind::DatabaseTable => &[NodeKind::DatabaseTable][..],
        ExplicitReferenceKind::Deployment => &[NodeKind::Deployment][..],
        ExplicitReferenceKind::ConfigKey => &[NodeKind::ConfigKey][..],
        ExplicitReferenceKind::Document => &[NodeKind::Document, NodeKind::Adr][..],
        ExplicitReferenceKind::Owner => &[NodeKind::Owner][..],
    };
    if !expected.contains(&node.kind) {
        return false;
    }
    if matches!(
        kind,
        ExplicitReferenceKind::Repository | ExplicitReferenceKind::RepositoryDependency
    ) {
        return node.repo_id.as_ref().is_some_and(|candidate| {
            candidate.as_str() == target
                || repository_aliases
                    .iter()
                    .any(|(alias, repo)| *alias == target && *repo == candidate)
        });
    }
    if node.stable_key == target || node.label == target {
        return true;
    }
    let prefix = reference_prefix(kind);
    if node
        .stable_key
        .strip_prefix(prefix)
        .is_some_and(|suffix| suffix == target)
    {
        return true;
    }
    match kind {
        ExplicitReferenceKind::Service => {
            target
                .split_once(':')
                .and_then(|(alias, name)| {
                    repository_aliases
                        .iter()
                        .find(|(candidate, _)| *candidate == alias)
                        .map(|(_, repository)| (repository, name))
                })
                .is_some_and(|(repository, name)| {
                    node.stable_key == format!("service:{}:{name}", repository.as_str())
                })
                || node.stable_key == format!("service:{}:{target}", repo_id.as_str())
        }
        ExplicitReferenceKind::Document => {
            node.stable_key == format!("document:{}:{target}", repo_id.as_str())
        }
        ExplicitReferenceKind::ConfigKey => {
            node.stable_key == format!("config:{}:{target}", repo_id.as_str())
        }
        ExplicitReferenceKind::Repository
        | ExplicitReferenceKind::RepositoryDependency
        | ExplicitReferenceKind::HttpContract
        | ExplicitReferenceKind::EventChannel
        | ExplicitReferenceKind::GraphqlOperation
        | ExplicitReferenceKind::RpcMethod
        | ExplicitReferenceKind::DatabaseTable
        | ExplicitReferenceKind::Deployment
        | ExplicitReferenceKind::Owner => false,
    }
}

const fn reference_prefix(kind: ExplicitReferenceKind) -> &'static str {
    match kind {
        ExplicitReferenceKind::Repository | ExplicitReferenceKind::RepositoryDependency => {
            "repository:"
        }
        ExplicitReferenceKind::Service => "service:",
        ExplicitReferenceKind::HttpContract => "http:",
        ExplicitReferenceKind::EventChannel => "event:",
        ExplicitReferenceKind::GraphqlOperation => "graphql:",
        ExplicitReferenceKind::RpcMethod => "rpc:",
        ExplicitReferenceKind::DatabaseTable => "table:",
        ExplicitReferenceKind::Deployment => "deployment:",
        ExplicitReferenceKind::ConfigKey => "config:",
        ExplicitReferenceKind::Document => "document:",
        ExplicitReferenceKind::Owner => "owner:",
    }
}

fn exactly_one<T: Ord>(mut values: BTreeSet<T>) -> Option<T> {
    (values.len() == 1).then(|| values.pop_first()).flatten()
}

fn repository_node(repo_id: &RepoId) -> Node {
    graph_node(
        format!("repository:{}", repo_id.as_str()),
        NodeKind::Repository,
        Some(repo_id.clone()),
        repo_id.as_str(),
    )
}

fn artifact_node(repo_id: &RepoId, source_path: &str) -> Node {
    graph_node(
        format!("artifact:{}:{source_path}", repo_id.as_str()),
        NodeKind::Artifact,
        Some(repo_id.clone()),
        source_path,
    )
}

fn database_node(name: &str) -> Node {
    graph_node(format!("database:{name}"), NodeKind::Database, None, name)
}

fn table_node(database: Option<&str>, schema: Option<&str>, name: &str) -> Node {
    graph_node(
        format!(
            "table:{}:{}:{name}",
            database.unwrap_or(""),
            schema.unwrap_or("")
        ),
        NodeKind::DatabaseTable,
        None,
        name,
    )
}

fn column_node(table_key: &str, name: &str) -> Node {
    graph_node(
        format!("column:{table_key}:{name}"),
        NodeKind::DatabaseColumn,
        None,
        name,
    )
}

fn data_symbol_node(repo_id: &RepoId, source_path: &str, owner: &str) -> Node {
    graph_node(
        format!("data-symbol:{}:{source_path}:{owner}", repo_id.as_str()),
        NodeKind::SymbolRef,
        Some(repo_id.clone()),
        owner,
    )
}

fn deployment_node(technology: &str, namespace: Option<&str>, name: &str) -> Node {
    graph_node(
        format!("deployment:{technology}:{}:{name}", namespace.unwrap_or("")),
        NodeKind::Deployment,
        None,
        name,
    )
}

fn service_node(repo_id: &RepoId, name: &str) -> Node {
    graph_node(
        format!("service:{}:{name}", repo_id.as_str()),
        NodeKind::Service,
        Some(repo_id.clone()),
        name,
    )
}

fn config_node(repo_id: &RepoId, scope: &str, name: &str) -> Node {
    graph_node(
        format!("config:{}:{scope}:{name}", repo_id.as_str()),
        NodeKind::ConfigKey,
        Some(repo_id.clone()),
        name,
    )
}

fn event_channel_node(namespace: Option<&str>, name: &str) -> Node {
    graph_node(
        format!("event:infrastructure:{}:{name}", namespace.unwrap_or("")),
        NodeKind::EventChannel,
        None,
        name,
    )
}

fn document_node(
    repo_id: &RepoId,
    source_path: &str,
    kind: DocumentKind,
    _title: Option<&str>,
) -> Node {
    graph_node(
        format!("document:{}:{source_path}", repo_id.as_str()),
        if kind == DocumentKind::Adr {
            NodeKind::Adr
        } else {
            NodeKind::Document
        },
        Some(repo_id.clone()),
        source_path,
    )
}

fn owner_node(name: &str) -> Node {
    graph_node(format!("owner:{name}"), NodeKind::Owner, None, name)
}

fn graph_node(stable_key: String, kind: NodeKind, repo_id: Option<RepoId>, label: &str) -> Node {
    Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind,
        repo_id,
        stable_key,
        label: label.to_owned(),
    }
}

fn extraction_evidence(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
    extractor: &str,
    note: &str,
) -> Evidence {
    let key = format!(
        "{}:{source_path}:{}:{}:{extractor}:{note}:{content_hash}",
        repo_id.as_str(),
        start_line.map_or_else(String::new, |line| line.to_string()),
        end_line.map_or_else(String::new, |line| line.to_string())
    );
    Evidence {
        id: EvidenceId::new(stable_id("evidence", &key)),
        repo_id: Some(repo_id.clone()),
        file_path: Some(source_path.to_owned()),
        start_line,
        end_line,
        extractor: extractor.to_owned(),
        extractor_version: "1.0.0".to_owned(),
        provenance: Provenance::Extracted,
        confidence: 1.0,
        observed_at_commit: None,
        content_hash: Some(content_hash.to_owned()),
        note: Some(note.to_owned()),
    }
}

const fn deployment_technology(kind: DeploymentKind) -> &'static str {
    match kind {
        DeploymentKind::DockerComposeService => "docker_compose_service",
        DeploymentKind::KubernetesDeployment => "kubernetes_deployment",
        DeploymentKind::KubernetesStatefulSet => "kubernetes_stateful_set",
        DeploymentKind::KubernetesDaemonSet => "kubernetes_daemon_set",
        DeploymentKind::KubernetesJob => "kubernetes_job",
        DeploymentKind::KubernetesCronJob => "kubernetes_cron_job",
        DeploymentKind::KubernetesPod => "kubernetes_pod",
        DeploymentKind::HelmTemplate => "helm_template",
        DeploymentKind::TerraformResource => "terraform_resource",
    }
}

fn infrastructure_line(evidence: &[InfrastructureEvidence]) -> Option<u32> {
    evidence.iter().filter_map(|item| item.line).min()
}

fn line_from_usize(line: usize) -> Option<u32> {
    u32::try_from(line).ok()
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{EdgeKind, NodeKind, RepoId};

    use super::documents_to_graph;
    use crate::{
        DataAccessObservation, DataAccessRole, DataArtifactKind, DataDocument, DataEvidenceLine, DataOperation, DatabaseTable, DeploymentKind, DeploymentUnit, InfrastructureArtifactKind, InfrastructureDocument, InfrastructureEvidence, InfrastructureEvidenceKind, SourceLanguage, extract_data_artifact, extract_markdown, extract_safe_config, extract_service_catalog, parse_literal_sql_source_at_root
    };

    #[test]
    fn data_reader_links_source_symbol_to_unique_table() {
        let repo = RepoId::new("repo:data");
        let line = DataEvidenceLine::new(7).expect("valid evidence line");
        let declaration = DataDocument {
            source_path: "schema.sql".to_owned(),
            artifact_kind: DataArtifactKind::DeclarativeSqlSchema,
            database_name: None,
            schema_name: None,
            tables: vec![DatabaseTable {
                database: None,
                schema: None,
                name: "users".to_owned(),
                columns: Vec::new(),
                indexes: Vec::new(),
                foreign_keys: Vec::new(),
                evidence: line,
            }],
            migration: None,
            accesses: Vec::new(),
            frameworks: Vec::new(),
            references: Vec::new(),
            owners: Vec::new(),
            warnings: Vec::new(),
            incomplete: false,
        };
        let reader = DataDocument {
            source_path: "src/users.rs".to_owned(),
            artifact_kind: DataArtifactKind::LiteralQuerySource,
            database_name: None,
            schema_name: None,
            tables: Vec::new(),
            migration: None,
            accesses: vec![DataAccessObservation {
                role: DataAccessRole::Reader,
                table: "users".to_owned(),
                model: None,
                owner: Some("load_users".to_owned()),
                operation: Some(DataOperation::Select),
                evidence: line,
            }],
            frameworks: Vec::new(),
            references: Vec::new(),
            owners: vec!["load_users".to_owned()],
            warnings: Vec::new(),
            incomplete: false,
        };

        let facts = documents_to_graph(
            &[
                (&repo, "schema.sql", "schema-hash", &declaration),
                (&repo, "src/users.rs", "source-hash", &reader),
            ],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        assert!(facts.edges.iter().any(|edge| {
            edge.kind == EdgeKind::ReadsTable
                && facts.nodes.iter().any(|node| {
                    node.id == edge.source
                        && node.kind == NodeKind::SymbolRef
                        && node.label == "load_users"
                })
                && facts.nodes.iter().any(|node| {
                    node.id == edge.target
                        && node.kind == NodeKind::DatabaseTable
                        && node.label == "users"
                })
        }));
    }

    #[test]
    fn sqlalchemy_access_links_through_explicit_model_binding() {
        let repo = RepoId::new("repo:orm");
        let models = extract_data_artifact(
            "app/models.py",
            r#"
class Services(Base):
    __tablename__ = "services"
    id = Column(Integer, primary_key=True)
"#,
        )
        .expect("model should parse");
        let controller = parse_literal_sql_source_at_root(
            SourceLanguage::Python,
            "app/controllers.py",
            "",
            r"
from sqlalchemy.orm import Session
from app import models

def list_services(db: Session):
    return db.query(models.Services).all()

def create_service(db: Session):
    service = models.Services()
    db.add(service)
",
        );

        let facts = documents_to_graph(
            &[
                (&repo, "app/models.py", "models-hash", &models),
                (&repo, "app/controllers.py", "controllers-hash", &controller),
            ],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        assert!(facts.edges.iter().any(|edge| {
            edge.kind == EdgeKind::ReadsTable
                && facts
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.source && node.label == "list_services")
                && facts
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.target && node.label == "services")
        }));
        assert!(facts.edges.iter().any(|edge| {
            edge.kind == EdgeKind::WritesTable
                && facts
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.source && node.label == "create_service")
                && facts
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.target && node.label == "services")
        }));
    }

    #[test]
    fn sqlx_query_file_links_calling_symbol_to_file_and_table() {
        let repo = RepoId::new("repo:data");
        let schema =
            extract_data_artifact("crates/api/schema.sql", "CREATE TABLE users (id INTEGER);")
                .expect("schema should parse");
        let query = extract_data_artifact(
            "crates/api/queries/users.sql",
            "SELECT id FROM users WHERE id = $1;",
        )
        .expect("query should parse");
        let source = parse_literal_sql_source_at_root(
            SourceLanguage::Rust,
            "crates/api/src/users.rs",
            "crates/api",
            r#"
async fn load_user(pool: &sqlx::PgPool) {
    sqlx::query_file!("queries/users.sql", 7_i64).fetch_one(pool).await;
}
"#,
        );

        let facts = documents_to_graph(
            &[
                (&repo, "crates/api/schema.sql", "schema-hash", &schema),
                (&repo, "crates/api/queries/users.sql", "query-hash", &query),
                (&repo, "crates/api/src/users.rs", "source-hash", &source),
            ],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let symbol = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::SymbolRef && node.label == "load_user")
            .expect("calling symbol");
        let query_artifact = facts
            .nodes
            .iter()
            .find(|node| {
                node.kind == NodeKind::Artifact && node.label == "crates/api/queries/users.sql"
            })
            .expect("query artifact");
        let users = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::DatabaseTable && node.label == "users")
            .expect("users table");

        assert!(facts.edges.iter().any(|edge| {
            edge.source == symbol.id
                && edge.target == query_artifact.id
                && edge.kind == EdgeKind::Consumes
        }));
        assert!(facts.edges.iter().any(|edge| {
            edge.source == symbol.id && edge.target == users.id && edge.kind == EdgeKind::ReadsTable
        }));
    }

    #[test]
    fn sqlx_default_migrate_macro_uses_incremental_configuration_artifact() {
        let repo = RepoId::new("repo:data");
        let source = parse_literal_sql_source_at_root(
            SourceLanguage::Rust,
            "crates/api/src/lib.rs",
            "crates/api",
            "static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();",
        );
        let configuration = extract_data_artifact(
            "crates/api/sqlx.toml",
            "[migrate]\nmigrations-dir = \"db/migrations\"\n",
        )
        .expect("SQLx configuration should parse");
        let migration = extract_data_artifact(
            "crates/api/db/migrations/0001_users.sql",
            "CREATE TABLE users (id INTEGER);",
        )
        .expect("migration should parse");

        let facts = documents_to_graph(
            &[
                (&repo, "crates/api/src/lib.rs", "source-hash", &source),
                (
                    &repo,
                    "crates/api/sqlx.toml",
                    "configuration-hash",
                    &configuration,
                ),
                (
                    &repo,
                    "crates/api/db/migrations/0001_users.sql",
                    "migration-hash",
                    &migration,
                ),
            ],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let source_artifact = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Artifact && node.label == "crates/api/src/lib.rs")
            .expect("source artifact");
        let migration_artifact = facts
            .nodes
            .iter()
            .find(|node| {
                node.kind == NodeKind::Artifact
                    && node.label == "crates/api/db/migrations/0001_users.sql"
            })
            .expect("migration artifact");

        assert!(facts.edges.iter().any(|edge| {
            edge.source == source_artifact.id
                && edge.target == migration_artifact.id
                && edge.kind == EdgeKind::Consumes
        }));
    }

    #[test]
    fn reversible_migrations_keep_empty_artifacts_and_explicit_lineage() {
        let repo = RepoId::new("repo:migrations");
        let first_up = extract_data_artifact(
            "migrations/0001_users.up.sql",
            "CREATE TABLE users (id INTEGER);",
        )
        .expect("up migration should parse");
        let first_down =
            extract_data_artifact("migrations/0001_users.down.sql", "DROP TABLE users;")
                .expect("down migration should parse");
        let second_up = extract_data_artifact(
            "migrations/0002_accounts.up.sql",
            "CREATE TABLE accounts (id INTEGER);",
        )
        .expect("second migration should parse");

        let facts = documents_to_graph(
            &[
                (&repo, "migrations/0001_users.up.sql", "first-up", &first_up),
                (
                    &repo,
                    "migrations/0001_users.down.sql",
                    "first-down",
                    &first_down,
                ),
                (
                    &repo,
                    "migrations/0002_accounts.up.sql",
                    "second-up",
                    &second_up,
                ),
            ],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let artifact = |label: &str| {
            facts
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Artifact && node.label == label)
                .unwrap_or_else(|| panic!("missing migration artifact {label}"))
        };
        let first_up = artifact("migrations/0001_users.up.sql");
        let first_down = artifact("migrations/0001_users.down.sql");
        let second_up = artifact("migrations/0002_accounts.up.sql");

        assert!(facts.edges.iter().any(|edge| {
            edge.source == first_down.id
                && edge.target == first_up.id
                && edge.kind == EdgeKind::Reverts
        }));
        assert!(facts.edges.iter().any(|edge| {
            edge.source == first_up.id
                && edge.target == second_up.id
                && edge.kind == EdgeKind::Precedes
        }));
    }

    #[test]
    fn repository_deploys_unit_that_provides_exact_service() {
        let repo = RepoId::new("repo:api");
        let document = InfrastructureDocument {
            artifact_kind: InfrastructureArtifactKind::DockerCompose,
            source_path: "compose.yaml".to_owned(),
            deployment_units: vec![DeploymentUnit {
                kind: DeploymentKind::DockerComposeService,
                name: "api".to_owned(),
                namespace: None,
                images: Vec::new(),
                ports: Vec::new(),
                environment_keys: Vec::new(),
                dependencies: Vec::new(),
                service_names: vec!["api".to_owned()],
                host_aliases: Vec::new(),
                selectors: Vec::new(),
                evidence: vec![InfrastructureEvidence {
                    kind: InfrastructureEvidenceKind::Declaration,
                    line: Some(2),
                }],
            }],
            resources: Vec::new(),
            environment_keys: Vec::new(),
            warnings: Vec::new(),
            incomplete: false,
        };

        let facts = documents_to_graph(
            &[],
            &[(&repo, "compose.yaml", "compose-hash", &document)],
            &[],
            &[],
            &[],
            &[],
        );

        assert!(
            [EdgeKind::Deploys, EdgeKind::Provides]
                .into_iter()
                .all(|kind| facts.edges.iter().any(|edge| edge.kind == kind))
        );
    }

    #[test]
    fn explicit_catalog_references_document_service_and_owner() {
        let repo = RepoId::new("repo:billing");
        let document = extract_service_catalog(
            "catalog.yaml",
            "services:\n  - name: billing\n    owner: '@payments'\n",
        )
        .expect("valid service catalog");

        let facts = documents_to_graph(
            &[],
            &[],
            &[(&repo, "catalog.yaml", "catalog-hash", &document)],
            &[],
            &[],
            &[],
        );

        assert!(
            facts
                .edges
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Documents)
                .any(|edge| {
                    facts.nodes.iter().any(|node| {
                        node.id == edge.target
                            && node.kind == NodeKind::Service
                            && node.label == "billing"
                    })
                })
                && facts
                    .edges
                    .iter()
                    .filter(|edge| edge.kind == EdgeKind::Documents)
                    .any(|edge| {
                        facts.nodes.iter().any(|node| {
                            node.id == edge.target
                                && node.kind == NodeKind::Owner
                                && node.label == "@payments"
                        })
                    })
        );
    }

    #[test]
    fn explicit_generated_by_link_creates_cross_repository_dependency() {
        let platform = RepoId::new("repo:platform");
        let transpiler = RepoId::new("repo:transpiler");
        let document = extract_markdown(
            "README.md",
            "Generated by [hugint-transpiler](https://github.com/huginthub/hugint-transpiler).\n",
        )
        .expect("valid Markdown");

        let facts = documents_to_graph(
            &[],
            &[],
            &[(&platform, "README.md", "readme-hash", &document)],
            &[],
            &[],
            &[
                ("hugint-platform", &platform),
                ("hugint-transpiler", &transpiler),
            ],
        );

        let relation = facts
            .edges
            .iter()
            .find(|edge| {
                edge.kind == EdgeKind::DependsOnRepository
                    && facts.nodes.iter().any(|node| {
                        node.id == edge.source
                            && node.kind == NodeKind::Repository
                            && node.repo_id.as_ref() == Some(&platform)
                    })
                    && facts.nodes.iter().any(|node| {
                        node.id == edge.target
                            && node.kind == NodeKind::Repository
                            && node.repo_id.as_ref() == Some(&transpiler)
                    })
            })
            .expect("cross-repository dependency");
        assert_ne!(relation.source, relation.target);
    }

    #[test]
    fn serialized_graph_never_contains_fixture_secret() {
        const FIXTURE_SECRET: &str = "extraction-fixture-secret-5f16";
        let repo = RepoId::new("repo:config");
        let source = format!("API_TOKEN={FIXTURE_SECRET}\n");
        let document = extract_safe_config(".env", &source).expect("valid dotenv");
        let facts = documents_to_graph(
            &[],
            &[],
            &[],
            &[(&repo, ".env", "config-hash", &document)],
            &[],
            &[],
        );

        let serialized = serde_json::to_string(&(&facts.nodes, &facts.edges, &facts.evidence))
            .expect("graph facts should serialize");

        assert!(!serialized.contains(FIXTURE_SECRET));
    }
}
