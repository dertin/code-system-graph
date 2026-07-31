use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};

use crate::{PackageDependency, PackageEcosystem, PackageManifest};

/// Package identity used for deterministic cross-repository ownership resolution.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageIdentity {
    /// Package ecosystem.
    pub ecosystem: PackageEcosystem,
    /// Ecosystem-specific package name.
    pub name: String,
}

/// Pending dependency ownership resolution retained with extracted package graph facts.
#[derive(Debug, Clone)]
pub struct PackageDependencyFact {
    /// Repository declaring the dependency.
    pub source_repo_id: RepoId,
    /// Node that declares the dependency.
    pub source_node_id: NodeId,
    /// Target package node.
    pub target_node_id: NodeId,
    /// Canonical target package identity.
    pub target: PackageIdentity,
    /// Direct declaration evidence.
    pub evidence_id: EvidenceId,
    /// Exact declaration confidence.
    pub confidence: f32,
}

/// Graph-ready package facts from one source-owned manifest.
#[derive(Debug, Clone, Default)]
pub struct PackageGraphFacts {
    /// Repository, artifact, and package nodes.
    pub nodes: Vec<Node>,
    /// Manifest containment and package dependency edges.
    pub edges: Vec<Edge>,
    /// Direct package declaration evidence.
    pub evidence: Vec<Evidence>,
    /// Package coordinates owned by this source repository.
    pub owned_packages: Vec<(PackageIdentity, RepoId)>,
    /// Dependency facts used to distinguish internal repositories from external packages.
    pub dependencies: Vec<PackageDependencyFact>,
}

/// Converts one package-manifest payload into deterministic graph facts.
#[must_use]
pub fn package_manifest_to_graph(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    manifest: &PackageManifest,
) -> PackageGraphFacts {
    let repository = repository_node(repo_id);
    let artifact = artifact_node(repo_id, source_path);
    let mut result = PackageGraphFacts {
        nodes: vec![repository.clone(), artifact.clone()],
        ..PackageGraphFacts::default()
    };
    let artifact_evidence = package_evidence(
        repo_id,
        source_path,
        content_hash,
        1,
        "manifest artifact",
        1.0,
    );
    result.evidence.push(artifact_evidence.clone());
    result.edges.push(edge(
        &repository.id,
        &artifact.id,
        EdgeKind::Contains,
        artifact_evidence.id,
        1.0,
    ));

    let mut owner_nodes = Vec::new();
    for package in &manifest.packages {
        let identity = PackageIdentity {
            ecosystem: package.ecosystem,
            name: package.name.clone(),
        };
        let node = package_node(&identity);
        let evidence = package_evidence(
            repo_id,
            source_path,
            content_hash,
            package.evidence.line,
            "package coordinate",
            package.confidence,
        );
        result.edges.push(edge(
            &artifact.id,
            &node.id,
            EdgeKind::Contains,
            evidence.id.clone(),
            package.confidence,
        ));
        result.owned_packages.push((identity, repo_id.clone()));
        result.evidence.push(evidence);
        owner_nodes.push(node.id.clone());
        result.nodes.push(node);
    }
    for member in &manifest.workspace_members {
        if member.value.contains(['*', '?', '[', ']']) {
            continue;
        }
        let Some(member_manifest) = workspace_member_manifest(source_path, &member.value) else {
            continue;
        };
        let node = artifact_node(repo_id, &member_manifest);
        let evidence = package_evidence(
            repo_id,
            source_path,
            content_hash,
            member.evidence.line,
            "workspace member",
            member.confidence,
        );
        result.edges.push(edge(
            &artifact.id,
            &node.id,
            EdgeKind::Contains,
            evidence.id.clone(),
            member.confidence,
        ));
        result.nodes.push(node);
        result.evidence.push(evidence);
    }
    let dependency_source = owner_nodes.first().unwrap_or(&artifact.id).clone();
    for dependency in &manifest.dependencies {
        append_dependency(
            &mut result,
            repo_id,
            source_path,
            content_hash,
            &dependency_source,
            dependency,
        );
    }
    deduplicate_package_facts(&mut result);
    result
}

/// Adds exact repository dependency edges for packages with one unambiguous registered owner.
///
/// Package dependencies remain present even when ownership is absent or ambiguous.
pub fn link_registered_package_owners(facts: &mut [PackageGraphFacts]) {
    let mut owners: BTreeMap<PackageIdentity, BTreeSet<RepoId>> = BTreeMap::new();
    for fact in facts.iter() {
        for (package, repo_id) in &fact.owned_packages {
            owners
                .entry(package.clone())
                .or_default()
                .insert(repo_id.clone());
        }
    }
    for fact in facts {
        for dependency in &fact.dependencies {
            let Some(owner_set) = owners.get(&dependency.target) else {
                continue;
            };
            if owner_set.len() != 1 || owner_set.contains(&dependency.source_repo_id) {
                continue;
            }
            let Some(owner) = owner_set.first() else {
                continue;
            };
            let owner_repository = repository_node(owner);
            fact.edges.push(edge(
                &dependency.source_node_id,
                &owner_repository.id,
                EdgeKind::DependsOnRepository,
                dependency.evidence_id.clone(),
                dependency.confidence,
            ));
            fact.nodes.push(owner_repository);
        }
        deduplicate_package_facts(fact);
    }
}

fn append_dependency(
    result: &mut PackageGraphFacts,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    source_node: &NodeId,
    dependency: &PackageDependency,
) {
    let identity = PackageIdentity {
        ecosystem: dependency.ecosystem,
        name: dependency.name.clone(),
    };
    let node = package_node(&identity);
    let evidence = package_evidence(
        repo_id,
        source_path,
        content_hash,
        dependency.evidence.line,
        "package dependency",
        dependency.confidence,
    );
    result.edges.push(edge(
        source_node,
        &node.id,
        EdgeKind::DependsOnPackage,
        evidence.id.clone(),
        dependency.confidence,
    ));
    result.dependencies.push(PackageDependencyFact {
        source_repo_id: repo_id.clone(),
        source_node_id: source_node.clone(),
        target_node_id: node.id.clone(),
        target: identity,
        evidence_id: evidence.id.clone(),
        confidence: dependency.confidence,
    });
    result.nodes.push(node);
    result.evidence.push(evidence);
}

fn repository_node(repo_id: &RepoId) -> Node {
    let stable_key = format!("repository:{}", repo_id.as_str());
    Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind: NodeKind::Repository,
        repo_id: Some(repo_id.clone()),
        stable_key,
        label: repo_id.as_str().to_owned(),
    }
}

fn artifact_node(repo_id: &RepoId, source_path: &str) -> Node {
    let stable_key = format!("artifact:{}:{source_path}", repo_id.as_str());
    Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind: NodeKind::Artifact,
        repo_id: Some(repo_id.clone()),
        stable_key,
        label: source_path.to_owned(),
    }
}

fn workspace_member_manifest(manifest_path: &str, member: &str) -> Option<String> {
    let parent = manifest_path
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    let member = member.replace('\\', "/");
    if member.starts_with('/')
        || member
            .split('/')
            .any(|component| component == ".." || component.is_empty())
    {
        return None;
    }
    let member = member
        .split('/')
        .filter(|component| *component != ".")
        .collect::<Vec<_>>()
        .join("/");
    if member.is_empty() {
        return Some(manifest_path.to_owned());
    }
    if parent.is_empty() {
        Some(format!("{member}/Cargo.toml"))
    } else {
        Some(format!("{parent}/{member}/Cargo.toml"))
    }
}

fn package_node(identity: &PackageIdentity) -> Node {
    let ecosystem = ecosystem_name(identity.ecosystem);
    let stable_key = format!("package:{ecosystem}:{}", identity.name);
    Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind: NodeKind::Package,
        repo_id: None,
        stable_key,
        label: format!("{ecosystem}:{}", identity.name),
    }
}

fn package_evidence(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    line: u32,
    note: &str,
    confidence: f32,
) -> Evidence {
    let key = format!(
        "{}:{source_path}:{line}:{note}:{content_hash}",
        repo_id.as_str()
    );
    Evidence {
        id: EvidenceId::new(stable_id("evidence", &key)),
        repo_id: Some(repo_id.clone()),
        file_path: Some(source_path.to_owned()),
        start_line: Some(line),
        end_line: Some(line),
        extractor: "code-system-graph.packages".to_owned(),
        extractor_version: "1.0.0".to_owned(),
        provenance: Provenance::Extracted,
        confidence,
        observed_at_commit: None,
        content_hash: Some(content_hash.to_owned()),
        note: Some(note.to_owned()),
    }
}

fn edge(
    source: &NodeId,
    target: &NodeId,
    kind: EdgeKind,
    evidence_id: EvidenceId,
    confidence: f32,
) -> Edge {
    let key = format!("{}:{kind:?}:{}", source.as_str(), target.as_str());
    Edge {
        id: EdgeId::new(stable_id("edge", &key)),
        source: source.clone(),
        target: target.clone(),
        kind,
        confidence,
        status: EpistemicStatus::Confirmed,
        evidence: vec![evidence_id],
    }
}

fn ecosystem_name(ecosystem: PackageEcosystem) -> &'static str {
    match ecosystem {
        PackageEcosystem::Npm => "npm",
        PackageEcosystem::Python => "python",
        PackageEcosystem::Cargo => "cargo",
        PackageEcosystem::Go => "go",
        PackageEcosystem::Maven => "maven",
        PackageEcosystem::Gradle => "gradle",
        PackageEcosystem::NuGet => "nuget",
    }
}

fn deduplicate_package_facts(facts: &mut PackageGraphFacts) {
    facts.nodes.sort_by(|left, right| left.id.cmp(&right.id));
    facts.nodes.dedup_by(|left, right| left.id == right.id);
    facts.edges.sort_by(|left, right| left.id.cmp(&right.id));
    facts.edges.dedup_by(|left, right| left.id == right.id);
    facts.evidence.sort_by(|left, right| left.id.cmp(&right.id));
    facts.evidence.dedup_by(|left, right| left.id == right.id);
    facts
        .owned_packages
        .sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    facts.owned_packages.dedup();
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use code_system_graph_model::{EdgeKind, RepoId};

    use super::{link_registered_package_owners, package_manifest_to_graph};
    use crate::extract_package_manifest;

    #[test]
    fn package_linking_should_distinguish_registered_repo_from_external_dependency() {
        let api_manifest = extract_package_manifest(
            "Cargo.toml",
            "[package]\nname = \"api-contract\"\nversion = \"1\"\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap_or_else(|error| panic!("fixture must parse: {error}"));
        let web_manifest = extract_package_manifest(
            "Cargo.toml",
            "[package]\nname = \"web\"\nversion = \"1\"\n[dependencies]\napi-contract = \"1\"\n",
        )
        .unwrap_or_else(|error| panic!("fixture must parse: {error}"));
        let mut facts = vec![
            package_manifest_to_graph(&RepoId::new("repo:api"), "Cargo.toml", "api", &api_manifest),
            package_manifest_to_graph(&RepoId::new("repo:web"), "Cargo.toml", "web", &web_manifest),
        ];

        link_registered_package_owners(&mut facts);

        assert!(
            facts[1]
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::DependsOnRepository)
        );
        assert!(
            facts[0]
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::DependsOnPackage)
        );
    }

    #[test]
    fn cargo_workspace_should_contain_declared_member_manifests() {
        let manifest = extract_package_manifest(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/api\", \"crates/core\"]\n",
        )
        .unwrap_or_else(|error| panic!("fixture must parse: {error}"));

        let facts = package_manifest_to_graph(
            &RepoId::new("repo:workspace"),
            "Cargo.toml",
            "root",
            &manifest,
        );

        let member_ids = facts
            .nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.label.as_str(),
                    "crates/api/Cargo.toml" | "crates/core/Cargo.toml"
                )
            })
            .map(|node| &node.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(member_ids.len(), 2);
        assert_eq!(
            facts
                .edges
                .iter()
                .filter(|edge| {
                    edge.kind == EdgeKind::Contains && member_ids.contains(&edge.target)
                })
                .count(),
            2
        );
    }
}
