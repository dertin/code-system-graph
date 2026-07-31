use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BoundaryRole, DataDocument, DatabaseColumn, DatabaseForeignKey, DatabaseIndex, DatabaseTable, DependencyScope, EventDocument, EventObservation, GraphqlDocument, GraphqlFieldDefinition, GraphqlTypeDefinition, GraphqlTypeRef, HttpBoundary, PackageCoordinate, PackageDependency, PackageManifest, PackageManifestValue, ProtoEnum, ProtoFieldCardinality, ProtoFile, ProtoMessage
};

const MAX_FINDING_VALUES: usize = 8;
const MAX_FINDING_VALUE_CHARS: usize = 512;

/// Conservative compatibility classification for one contract comparison.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityStatus {
    /// The modeled change is known to break at least one supported contract rule.
    Breaking,
    /// The modeled change can break consumers but requires runtime or policy confirmation.
    PotentiallyBreaking,
    /// Every modeled rule is compatible and both inputs are complete.
    Compatible,
    /// Coverage is insufficient to claim compatibility.
    Unknown,
    /// The supplied contracts cannot be compared under the modeled rules.
    Incomparable,
}

/// One evidence-backed compatibility rule result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompatibilityFinding {
    /// Stable machine-readable rule code.
    pub code: String,
    /// Contract coordinate affected by the rule.
    pub path: String,
    /// Rule classification.
    pub status: CompatibilityStatus,
    /// Factors that caused the classification.
    pub factors: Vec<String>,
    /// Bounded evidence locations or declarations.
    pub evidence: Vec<String>,
    /// Concrete validations recommended to the caller.
    pub recommended_validations: Vec<String>,
}

/// Complete compatibility result with exact before/after fingerprints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompatibilityReport {
    /// Most conservative aggregate status.
    pub status: CompatibilityStatus,
    /// BLAKE3 fingerprint of the structured previous contract.
    pub before_fingerprint: String,
    /// BLAKE3 fingerprint of the structured candidate contract.
    pub after_fingerprint: String,
    /// Deterministically ordered findings.
    pub findings: Vec<CompatibilityFinding>,
}

/// Compares GraphQL schemas and persisted operations using conservative breaking-change rules.
///
/// Missing types/fields/enum values, new required arguments, narrowed types, and invalidated
/// persisted operations are breaking. Incomplete extraction returns [`CompatibilityStatus::Unknown`].
#[must_use]
pub fn compare_graphql_contracts(
    before: &GraphqlDocument,
    after: &GraphqlDocument,
) -> CompatibilityReport {
    let mut findings = Vec::new();
    let (before_fingerprint, after_fingerprint) =
        fingerprints(before, after, "graphql", &mut findings);
    if !before.complete || !after.complete {
        findings.push(finding(
            "graphql.extraction_incomplete",
            "graphql",
            CompatibilityStatus::Unknown,
            vec!["one or both GraphQL documents are incomplete".to_owned()],
            vec![before.source_path.clone(), after.source_path.clone()],
            vec!["resolve extraction warnings before compatibility analysis".to_owned()],
        ));
    }

    let after_types = after
        .types
        .iter()
        .map(|definition| (definition.name.as_str(), definition))
        .collect::<BTreeMap<_, _>>();
    for previous_type in &before.types {
        let Some(current_type) = after_types.get(previous_type.name.as_str()) else {
            findings.push(graphql_breaking(
                "graphql.type_removed",
                &previous_type.name,
                previous_type.lines.start,
                "type is absent from the candidate schema",
            ));
            continue;
        };
        compare_graphql_type(previous_type, current_type, &mut findings);
    }
    compare_persisted_operations(before, after, &mut findings);
    finish_report(before_fingerprint, after_fingerprint, findings)
}

/// Compares event channels and payload schemas using conservative breaking-change rules.
#[must_use]
pub fn compare_event_contracts(
    before: &EventDocument,
    after: &EventDocument,
) -> CompatibilityReport {
    let mut findings = Vec::new();
    let (before_fingerprint, after_fingerprint) =
        fingerprints(before, after, "event", &mut findings);
    if before.incomplete || after.incomplete {
        findings.push(finding(
            "event.extraction_incomplete",
            "event",
            CompatibilityStatus::Unknown,
            vec!["one or both event documents are incomplete".to_owned()],
            vec![
                before.source_path.clone().unwrap_or_default(),
                after.source_path.clone().unwrap_or_default(),
            ],
            vec!["resolve event extraction warnings before compatibility analysis".to_owned()],
        ));
    }
    let current = after
        .observations
        .iter()
        .filter_map(|observation| event_key(observation).map(|key| (key, observation)))
        .collect::<BTreeMap<_, _>>();
    for previous in &before.observations {
        let Some(key) = event_key(previous) else {
            continue;
        };
        let Some(candidate) = current.get(&key) else {
            findings.push(event_breaking(
                "event.channel_removed",
                &key,
                previous,
                "channel role is absent from the candidate contract",
            ));
            continue;
        };
        compare_event_observation(previous, candidate, &key, &mut findings);
    }
    finish_report(before_fingerprint, after_fingerprint, findings)
}

/// Compares protobuf/gRPC contracts using field-number and wire-compatibility rules.
#[must_use]
pub fn compare_protobuf_contracts(before: &ProtoFile, after: &ProtoFile) -> CompatibilityReport {
    let mut findings = Vec::new();
    let (before_fingerprint, after_fingerprint) =
        fingerprints(before, after, "protobuf", &mut findings);
    if before.package != after.package {
        findings.push(proto_breaking(
            "protobuf.package_renamed",
            before.package.as_deref().unwrap_or("<root>"),
            before.package_line.unwrap_or(1),
            "protobuf package changed",
        ));
    }
    compare_proto_messages(before, after, &mut findings);
    compare_proto_enums(before, after, &mut findings);
    compare_proto_services(before, after, &mut findings);
    finish_report(before_fingerprint, after_fingerprint, findings)
}

/// Compares the currently modeled HTTP endpoint, method, and boundary-role inventory.
///
/// This comparator never treats boundary-only extraction as proof of request or response schema
/// compatibility. Duplicate operations are incomparable, incomplete operations are unknown,
/// exact removals are breaking, and uniquely attributable path or method changes are potentially
/// breaking.
#[must_use]
pub fn compare_http_contracts(
    before: &[HttpBoundary],
    after: &[HttpBoundary],
) -> CompatibilityReport {
    let mut findings = Vec::new();
    let before_contract = http_fingerprint_contract(before);
    let after_contract = http_fingerprint_contract(after);
    let (before_fingerprint, after_fingerprint) =
        fingerprints(&before_contract, &after_contract, "http", &mut findings);
    let before_inventory = http_inventory(before, "before", &mut findings);
    let after_inventory = http_inventory(after, "after", &mut findings);
    let mut consumed_additions = BTreeSet::new();

    for (key, previous) in &before_inventory {
        if after_inventory.contains_key(key) {
            continue;
        }
        let candidates = after_inventory
            .iter()
            .filter(|(candidate_key, _)| {
                !consumed_additions.contains(*candidate_key)
                    && !before_inventory.contains_key(*candidate_key)
                    && key.0 == candidate_key.0
                    && ((key.1 == candidate_key.1) ^ (key.2 == candidate_key.2))
            })
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let (candidate_key, candidate) = candidates[0];
            consumed_additions.insert(candidate_key.clone());
            findings.push(finding(
                "http.operation_changed",
                &http_key_path(key),
                CompatibilityStatus::PotentiallyBreaking,
                vec![format!(
                    "operation changed from {} {} to {} {}",
                    key.1, key.2, candidate_key.1, candidate_key.2
                )],
                combined_http_evidence(previous, candidate),
                vec![
                    "confirm the path or method migration with every HTTP consumer".to_owned(),
                    "run request and response contract tests against the candidate operation"
                        .to_owned(),
                ],
            ));
        } else {
            findings.push(finding(
                "http.operation_removed",
                &http_key_path(key),
                CompatibilityStatus::Breaking,
                vec!["endpoint and method pair is absent from the candidate inventory".to_owned()],
                http_evidence(previous),
                vec!["run affected HTTP consumers against the candidate provider".to_owned()],
            ));
        }
    }

    for (key, candidate) in &after_inventory {
        if !before_inventory.contains_key(key) && !consumed_additions.contains(key) {
            findings.push(finding(
                "http.operation_added",
                &http_key_path(key),
                CompatibilityStatus::Compatible,
                vec!["an exact endpoint and method pair was added".to_owned()],
                http_evidence(candidate),
                vec!["validate routing and authorization before exposing the operation".to_owned()],
            ));
        }
    }
    findings.push(finding(
        "http.schema_semantics_unmodeled",
        "http",
        CompatibilityStatus::Incomparable,
        vec![
            "HTTP boundaries model endpoint, method, and role but not parameters or schemas"
                .to_owned(),
        ],
        http_inventory_evidence(before, after),
        vec![
            "compare complete OpenAPI request, response, media-type, status, and security schemas"
                .to_owned(),
        ],
    ));
    finish_report(before_fingerprint, after_fingerprint, findings)
}

/// Compares package coordinates, exports, dependencies, features, and workspace declarations.
///
/// Changes that require package-manager resolution or type-declaration analysis remain unknown.
#[must_use]
pub fn compare_package_contracts(
    before: &PackageManifest,
    after: &PackageManifest,
) -> CompatibilityReport {
    let mut findings = Vec::new();
    let (before_fingerprint, after_fingerprint) =
        fingerprints(before, after, "package", &mut findings);
    compare_package_coordinates(before, after, &mut findings);
    compare_manifest_values(
        "package.export",
        &before.exports,
        &after.exports,
        CompatibilityStatus::Breaking,
        CompatibilityStatus::Compatible,
        &mut findings,
    );
    compare_package_dependencies(&before.dependencies, &after.dependencies, &mut findings);
    compare_manifest_values(
        "package.feature",
        &before.features,
        &after.features,
        CompatibilityStatus::PotentiallyBreaking,
        CompatibilityStatus::Compatible,
        &mut findings,
    );
    compare_manifest_values(
        "package.workspace",
        &before.workspace_members,
        &after.workspace_members,
        CompatibilityStatus::Unknown,
        CompatibilityStatus::Unknown,
        &mut findings,
    );
    if before.workspace_members != after.workspace_members {
        findings.push(finding(
            "package.workspace_cycle_unmodeled",
            "package.workspace",
            CompatibilityStatus::Unknown,
            vec!["workspace membership changed but dependency cycles are not modeled".to_owned()],
            manifest_value_evidence(&before.workspace_members, &after.workspace_members),
            vec![
                "resolve the candidate workspace graph and reject newly introduced cycles"
                    .to_owned(),
            ],
        ));
    }
    findings.push(finding(
        "package.type_declarations_unmodeled",
        "package.types",
        CompatibilityStatus::Unknown,
        vec!["package manifests do not model public type declaration compatibility".to_owned()],
        package_manifest_evidence(before, after),
        vec![
            "run the ecosystem type checker against representative downstream consumers".to_owned(),
        ],
    ));
    finish_report(before_fingerprint, after_fingerprint, findings)
}

/// Compares directly modeled database tables, columns, constraints, and migration ancestry.
///
/// Cross-repository consumer impact is not represented by one [`DataDocument`] and is therefore
/// always reported as unknown rather than inferred.
#[must_use]
pub fn compare_database_contracts(
    before: &DataDocument,
    after: &DataDocument,
) -> CompatibilityReport {
    let mut findings = Vec::new();
    let (before_fingerprint, after_fingerprint) =
        fingerprints(before, after, "database", &mut findings);
    if before.incomplete
        || after.incomplete
        || !before.warnings.is_empty()
        || !after.warnings.is_empty()
    {
        findings.push(finding(
            "database.extraction_incomplete",
            "database",
            CompatibilityStatus::Unknown,
            vec!["one or both database documents contain extraction limitations".to_owned()],
            vec![before.source_path.clone(), after.source_path.clone()],
            vec!["resolve database extraction warnings before compatibility analysis".to_owned()],
        ));
    }
    let before_tables = database_inventory(before, "before", &mut findings);
    let after_tables = database_inventory(after, "after", &mut findings);
    for (key, previous) in &before_tables {
        let Some(candidate) = after_tables.get(key) else {
            findings.push(database_finding(
                "database.table_removed",
                key,
                CompatibilityStatus::Breaking,
                previous.evidence.get(),
                "table is absent from the candidate schema",
            ));
            continue;
        };
        compare_database_table(key, previous, candidate, &mut findings);
    }
    compare_migrations(before, after, &mut findings);
    findings.push(finding(
        "database.shared_consumer_impact_unmodeled",
        "database.consumers",
        CompatibilityStatus::Unknown,
        vec!["one artifact cannot establish the impact on every shared-table consumer".to_owned()],
        vec![before.source_path.clone(), after.source_path.clone()],
        vec!["query the repository graph for all readers and writers, then run their integration tests".to_owned()],
    ));
    finish_report(before_fingerprint, after_fingerprint, findings)
}

const _: fn(&[HttpBoundary], &[HttpBoundary]) -> CompatibilityReport = compare_http_contracts;
const _: fn(&PackageManifest, &PackageManifest) -> CompatibilityReport = compare_package_contracts;
const _: fn(&DataDocument, &DataDocument) -> CompatibilityReport = compare_database_contracts;

fn http_fingerprint_contract(
    boundaries: &[HttpBoundary],
) -> Vec<(String, String, String, String, u32, u32, u32)> {
    boundaries
        .iter()
        .map(|boundary| {
            (
                http_role(boundary.role).to_owned(),
                boundary.method.clone(),
                boundary.path.clone(),
                boundary.evidence.file_path.clone().unwrap_or_default(),
                boundary.evidence.start_line.unwrap_or_default(),
                boundary.evidence.end_line.unwrap_or_default(),
                boundary.evidence.confidence.to_bits(),
            )
        })
        .collect()
}

fn http_inventory<'a>(
    boundaries: &'a [HttpBoundary],
    side: &str,
    findings: &mut Vec<CompatibilityFinding>,
) -> BTreeMap<(String, String, String), &'a HttpBoundary> {
    let mut inventory = BTreeMap::new();
    for boundary in boundaries {
        let key = (
            http_role(boundary.role).to_owned(),
            boundary.method.trim().to_ascii_uppercase(),
            boundary.path.trim().to_owned(),
        );
        if key.1.is_empty()
            || key.2.is_empty()
            || !key.2.starts_with('/')
            || !boundary.evidence.confidence.is_finite()
            || boundary.evidence.confidence < 1.0
        {
            findings.push(finding(
                "http.operation_incomplete",
                &http_key_path(&key),
                CompatibilityStatus::Unknown,
                vec![format!("{side} operation lacks an exact method, canonical path, or full-confidence evidence")],
                http_evidence(boundary),
                vec!["rerun bounded HTTP extraction and resolve dynamic operation evidence".to_owned()],
            ));
        }
        if let Some(previous) = inventory.insert(key.clone(), boundary) {
            findings.push(finding(
                "http.operation_duplicate",
                &http_key_path(&key),
                CompatibilityStatus::Incomparable,
                vec![format!(
                    "{side} inventory contains duplicate operation coordinates"
                )],
                combined_http_evidence(previous, boundary),
                vec!["deduplicate providers or disambiguate the operation declarations".to_owned()],
            ));
        }
    }
    inventory
}

fn http_role(role: BoundaryRole) -> &'static str {
    match role {
        BoundaryRole::Provider => "provider",
        BoundaryRole::Consumer => "consumer",
    }
}

fn http_key_path(key: &(String, String, String)) -> String {
    format!("{}:{} {}", key.0, key.1, key.2)
}

fn http_evidence(boundary: &HttpBoundary) -> Vec<String> {
    let source = boundary
        .evidence
        .file_path
        .as_deref()
        .unwrap_or("<unknown>");
    let locator = match (boundary.evidence.start_line, boundary.evidence.end_line) {
        (Some(start), Some(end)) => format!("{source}:{start}-{end}"),
        (Some(start), None) => format!("{source}:{start}"),
        _ => source.to_owned(),
    };
    vec![locator, boundary.node.stable_key.clone()]
}

fn combined_http_evidence(before: &HttpBoundary, after: &HttpBoundary) -> Vec<String> {
    http_evidence(before)
        .into_iter()
        .chain(http_evidence(after))
        .collect()
}

fn http_inventory_evidence(before: &[HttpBoundary], after: &[HttpBoundary]) -> Vec<String> {
    before.iter().chain(after).flat_map(http_evidence).collect()
}

fn compare_package_coordinates(
    before: &PackageManifest,
    after: &PackageManifest,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let before_packages = package_inventory(&before.packages, "before", findings);
    let after_packages = package_inventory(&after.packages, "after", findings);
    let mut consumed = BTreeSet::new();
    for (key, previous) in &before_packages {
        if let Some(candidate) = after_packages.get(key) {
            compare_package_version(previous, candidate, findings);
            continue;
        }
        let rename_candidates = after_packages
            .iter()
            .filter(|(candidate_key, candidate)| {
                !consumed.contains(*candidate_key)
                    && key.0 == candidate_key.0
                    && previous.source_path == candidate.source_path
            })
            .collect::<Vec<_>>();
        if let [(candidate_key, candidate)] = rename_candidates.as_slice() {
            consumed.insert((*candidate_key).clone());
            findings.push(finding(
                "package.coordinate_renamed",
                &format!("{}:{}", key.0, key.1),
                CompatibilityStatus::Breaking,
                vec![format!(
                    "package coordinate changed from {} to {}",
                    previous.name, candidate.name
                )],
                package_coordinate_evidence(previous, candidate),
                vec![
                    "publish a compatibility package or migrate every downstream dependency"
                        .to_owned(),
                ],
            ));
        } else {
            findings.push(finding(
                "package.coordinate_removed",
                &format!("{}:{}", key.0, key.1),
                CompatibilityStatus::Breaking,
                vec!["package coordinate is absent from the candidate manifest".to_owned()],
                vec![package_locator(previous)],
                vec![
                    "verify all registry and workspace consumers before removing the package"
                        .to_owned(),
                ],
            ));
        }
    }
    for (key, candidate) in &after_packages {
        if !before_packages.contains_key(key) && !consumed.contains(key) {
            findings.push(finding(
                "package.coordinate_added",
                &format!("{}:{}", key.0, key.1),
                CompatibilityStatus::Compatible,
                vec!["a distinct package coordinate was added".to_owned()],
                vec![package_locator(candidate)],
                vec!["validate publication metadata before release".to_owned()],
            ));
        }
    }
}

fn package_inventory<'a>(
    packages: &'a [PackageCoordinate],
    side: &str,
    findings: &mut Vec<CompatibilityFinding>,
) -> BTreeMap<(String, String), &'a PackageCoordinate> {
    let mut inventory = BTreeMap::new();
    for package in packages {
        let key = (format!("{:?}", package.ecosystem), package.name.clone());
        if let Some(previous) = inventory.insert(key.clone(), package) {
            findings.push(finding(
                "package.coordinate_duplicate",
                &format!("{}:{}", key.0, key.1),
                CompatibilityStatus::Incomparable,
                vec![format!(
                    "{side} manifest contains duplicate package coordinates"
                )],
                package_coordinate_evidence(previous, package),
                vec!["deduplicate package declarations before comparison".to_owned()],
            ));
        }
    }
    inventory
}

fn compare_package_version(
    before: &PackageCoordinate,
    after: &PackageCoordinate,
    findings: &mut Vec<CompatibilityFinding>,
) {
    if before.version == after.version {
        return;
    }
    let status = match (&before.version, &after.version) {
        (Some(previous), Some(candidate))
            if leading_version_major(previous) != leading_version_major(candidate) =>
        {
            CompatibilityStatus::Breaking
        }
        (Some(_), Some(_)) => CompatibilityStatus::PotentiallyBreaking,
        _ => CompatibilityStatus::Unknown,
    };
    findings.push(finding(
        "package.declared_version_changed",
        &format!("{:?}:{}", before.ecosystem, before.name),
        status,
        vec![format!(
            "declared package version changed from {} to {}",
            before.version.as_deref().unwrap_or("<unspecified>"),
            after.version.as_deref().unwrap_or("<unspecified>")
        )],
        package_coordinate_evidence(before, after),
        vec![
            "resolve the published versions and run downstream package compatibility tests"
                .to_owned(),
        ],
    ));
}

fn leading_version_major(value: &str) -> Option<u64> {
    let start = value.find(|character: char| character.is_ascii_digit())?;
    value[start..]
        .split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn compare_manifest_values(
    prefix: &str,
    before: &[PackageManifestValue],
    after: &[PackageManifestValue],
    removed_status: CompatibilityStatus,
    added_status: CompatibilityStatus,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let before_values = manifest_value_inventory(prefix, "before", before, findings);
    let after_values = manifest_value_inventory(prefix, "after", after, findings);
    for (value, previous) in &before_values {
        if !after_values.contains_key(value) {
            findings.push(finding(
                &format!("{prefix}_removed"),
                value,
                removed_status,
                vec![format!(
                    "{prefix} declaration is absent from the candidate manifest"
                )],
                vec![manifest_value_locator(previous)],
                vec![format!(
                    "validate downstream consumers of `{value}` before release"
                )],
            ));
        }
    }
    for (value, candidate) in &after_values {
        if !before_values.contains_key(value) {
            findings.push(finding(
                &format!("{prefix}_added"),
                value,
                added_status,
                vec![format!("{prefix} declaration was added")],
                vec![manifest_value_locator(candidate)],
                vec![format!(
                    "validate the new `{value}` declaration with the package manager"
                )],
            ));
        }
    }
}

fn manifest_value_inventory<'a>(
    prefix: &str,
    side: &str,
    values: &'a [PackageManifestValue],
    findings: &mut Vec<CompatibilityFinding>,
) -> BTreeMap<String, &'a PackageManifestValue> {
    let mut inventory = BTreeMap::new();
    for value in values {
        if let Some(previous) = inventory.insert(value.value.clone(), value) {
            findings.push(finding(
                &format!("{prefix}_duplicate"),
                &value.value,
                CompatibilityStatus::Incomparable,
                vec![format!("{side} manifest contains duplicate declarations")],
                vec![
                    manifest_value_locator(previous),
                    manifest_value_locator(value),
                ],
                vec!["deduplicate declarations before comparison".to_owned()],
            ));
        }
    }
    inventory
}

fn compare_package_dependencies(
    before: &[PackageDependency],
    after: &[PackageDependency],
    findings: &mut Vec<CompatibilityFinding>,
) {
    let before_dependencies = dependency_inventory(before, "before", findings);
    let after_dependencies = dependency_inventory(after, "after", findings);
    for (key, previous) in &before_dependencies {
        let Some(candidate) = after_dependencies.get(key) else {
            findings.push(package_dependency_finding(
                "package.dependency_removed",
                key,
                dependency_drift_status(previous.scope),
                previous,
                None,
                "dependency declaration is absent from the candidate manifest",
            ));
            continue;
        };
        if previous.version_or_range != candidate.version_or_range
            || previous.scope != candidate.scope
            || previous.optional != candidate.optional
            || previous.condition != candidate.condition
        {
            let status =
                if previous.version_or_range.is_none() || candidate.version_or_range.is_none() {
                    CompatibilityStatus::Unknown
                } else {
                    dependency_drift_status(previous.scope)
                };
            findings.push(package_dependency_finding(
                "package.dependency_drift",
                key,
                status,
                previous,
                Some(candidate),
                "version range, scope, optionality, or condition changed",
            ));
        }
    }
    for (key, candidate) in &after_dependencies {
        if !before_dependencies.contains_key(key) {
            let status = if candidate.scope == DependencyScope::Peer {
                CompatibilityStatus::PotentiallyBreaking
            } else {
                CompatibilityStatus::Unknown
            };
            findings.push(package_dependency_finding(
                "package.dependency_added",
                key,
                status,
                candidate,
                None,
                "dependency declaration was added",
            ));
        }
    }
}

fn dependency_inventory<'a>(
    dependencies: &'a [PackageDependency],
    side: &str,
    findings: &mut Vec<CompatibilityFinding>,
) -> BTreeMap<(String, String), &'a PackageDependency> {
    let mut inventory = BTreeMap::new();
    for dependency in dependencies {
        let key = (
            format!("{:?}", dependency.ecosystem),
            dependency.name.clone(),
        );
        if let Some(previous) = inventory.insert(key.clone(), dependency) {
            findings.push(finding(
                "package.dependency_duplicate",
                &format!("{}:{}", key.0, key.1),
                CompatibilityStatus::Incomparable,
                vec![format!(
                    "{side} manifest contains duplicate dependency coordinates"
                )],
                vec![
                    package_dependency_locator(previous),
                    package_dependency_locator(dependency),
                ],
                vec!["deduplicate dependency declarations before comparison".to_owned()],
            ));
        }
    }
    inventory
}

fn dependency_drift_status(scope: DependencyScope) -> CompatibilityStatus {
    match scope {
        DependencyScope::Dev | DependencyScope::Test => CompatibilityStatus::Unknown,
        DependencyScope::Runtime
        | DependencyScope::Build
        | DependencyScope::Peer
        | DependencyScope::Optional => CompatibilityStatus::PotentiallyBreaking,
    }
}

fn package_dependency_finding(
    code: &str,
    key: &(String, String),
    status: CompatibilityStatus,
    before: &PackageDependency,
    after: Option<&PackageDependency>,
    factor: &str,
) -> CompatibilityFinding {
    let evidence = std::iter::once(package_dependency_locator(before))
        .chain(after.map(package_dependency_locator))
        .collect();
    finding(
        code,
        &format!("{}:{}", key.0, key.1),
        status,
        vec![factor.to_owned()],
        evidence,
        vec!["resolve dependency ranges and run package-manager peer validation".to_owned()],
    )
}

fn package_locator(package: &PackageCoordinate) -> String {
    format!("{}:{}", package.source_path, package.evidence.line)
}

fn package_coordinate_evidence(
    before: &PackageCoordinate,
    after: &PackageCoordinate,
) -> Vec<String> {
    vec![package_locator(before), package_locator(after)]
}

fn package_dependency_locator(dependency: &PackageDependency) -> String {
    format!("{}:{}", dependency.source_path, dependency.evidence.line)
}

fn manifest_value_locator(value: &PackageManifestValue) -> String {
    format!("{}:{}", value.source_path, value.evidence.line)
}

fn manifest_value_evidence(
    before: &[PackageManifestValue],
    after: &[PackageManifestValue],
) -> Vec<String> {
    before
        .iter()
        .chain(after)
        .map(manifest_value_locator)
        .collect()
}

fn package_manifest_evidence(before: &PackageManifest, after: &PackageManifest) -> Vec<String> {
    before
        .packages
        .iter()
        .chain(&after.packages)
        .map(package_locator)
        .chain(
            before
                .exports
                .iter()
                .chain(&after.exports)
                .map(manifest_value_locator),
        )
        .collect()
}

fn database_inventory<'a>(
    document: &'a DataDocument,
    side: &str,
    findings: &mut Vec<CompatibilityFinding>,
) -> BTreeMap<String, &'a DatabaseTable> {
    let mut inventory = BTreeMap::new();
    for table in &document.tables {
        let key = database_table_key(table);
        if let Some(previous) = inventory.insert(key.clone(), table) {
            findings.push(finding(
                "database.table_duplicate",
                &key,
                CompatibilityStatus::Incomparable,
                vec![format!(
                    "{side} document contains duplicate table coordinates"
                )],
                vec![
                    data_locator(document, previous.evidence.get()),
                    data_locator(document, table.evidence.get()),
                ],
                vec!["deduplicate or qualify table declarations before comparison".to_owned()],
            ));
        }
    }
    inventory
}

fn database_table_key(table: &DatabaseTable) -> String {
    [
        table.database.as_deref(),
        table.schema.as_deref(),
        Some(&table.name),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(".")
}

fn compare_database_table(
    path: &str,
    before: &DatabaseTable,
    after: &DatabaseTable,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let before_columns = database_column_inventory(path, "before", before, findings);
    let after_columns = database_column_inventory(path, "after", after, findings);
    for (name, previous) in before_columns {
        let column_path = format!("{path}.{name}");
        let Some(candidate) = after_columns.get(name) else {
            findings.push(database_finding(
                "database.column_removed",
                &column_path,
                CompatibilityStatus::Breaking,
                previous.evidence.get(),
                "column is absent from the candidate table",
            ));
            continue;
        };
        compare_database_column(&column_path, previous, candidate, findings);
    }
    compare_database_indexes(path, &before.indexes, &after.indexes, findings);
    compare_database_foreign_keys(path, &before.foreign_keys, &after.foreign_keys, findings);
}

fn database_column_inventory<'a>(
    path: &str,
    side: &str,
    table: &'a DatabaseTable,
    findings: &mut Vec<CompatibilityFinding>,
) -> BTreeMap<&'a str, &'a DatabaseColumn> {
    let mut inventory = BTreeMap::new();
    for column in &table.columns {
        if let Some(previous) = inventory.insert(column.name.as_str(), column) {
            findings.push(finding(
                "database.column_duplicate",
                &format!("{path}.{}", column.name),
                CompatibilityStatus::Incomparable,
                vec![format!(
                    "{side} table contains duplicate column declarations"
                )],
                vec![
                    format!("line:{}", previous.evidence.get()),
                    format!("line:{}", column.evidence.get()),
                ],
                vec!["deduplicate column declarations before comparison".to_owned()],
            ));
        }
    }
    inventory
}

fn compare_database_column(
    path: &str,
    before: &DatabaseColumn,
    after: &DatabaseColumn,
    findings: &mut Vec<CompatibilityFinding>,
) {
    if before.data_type != after.data_type {
        let status = match (&before.data_type, &after.data_type) {
            (Some(previous), Some(candidate)) if database_type_narrowed(previous, candidate) => {
                CompatibilityStatus::Breaking
            }
            (Some(_), Some(_)) => CompatibilityStatus::PotentiallyBreaking,
            _ => CompatibilityStatus::Unknown,
        };
        findings.push(database_finding(
            "database.column_type_changed",
            path,
            status,
            after.evidence.get(),
            "declared column type changed",
        ));
    }
    if before.nullable != after.nullable {
        let status = match (before.nullable, after.nullable) {
            (Some(true), Some(false)) if after.default_present => {
                CompatibilityStatus::PotentiallyBreaking
            }
            (Some(true), Some(false)) => CompatibilityStatus::Breaking,
            (Some(_), Some(_)) => CompatibilityStatus::Compatible,
            _ => CompatibilityStatus::Unknown,
        };
        findings.push(database_finding(
            "database.column_nullability_changed",
            path,
            status,
            after.evidence.get(),
            "declared column nullability changed",
        ));
    }
    if before.primary_key && !after.primary_key {
        findings.push(database_finding(
            "database.primary_key_removed",
            path,
            CompatibilityStatus::Breaking,
            after.evidence.get(),
            "primary-key constraint was removed",
        ));
    }
    if before.unique && !after.unique {
        findings.push(database_finding(
            "database.unique_constraint_removed",
            path,
            CompatibilityStatus::PotentiallyBreaking,
            after.evidence.get(),
            "unique constraint was removed",
        ));
    }
}

fn database_type_narrowed(before: &str, after: &str) -> bool {
    let before = before.to_ascii_lowercase().replace(' ', "");
    let after = after.to_ascii_lowercase().replace(' ', "");
    let integer_rank = |value: &str| match value {
        "tinyint" => Some(0_u8),
        "smallint" => Some(1),
        "int" | "integer" => Some(2),
        "bigint" => Some(3),
        _ => None,
    };
    if let (Some(previous), Some(candidate)) = (integer_rank(&before), integer_rank(&after)) {
        return candidate < previous;
    }
    for prefix in ["char", "varchar", "binary", "varbinary"] {
        if let (Some(previous), Some(candidate)) =
            (type_width(&before, prefix), type_width(&after, prefix))
        {
            return candidate < previous;
        }
    }
    false
}

fn type_width(value: &str, prefix: &str) -> Option<u64> {
    value
        .strip_prefix(prefix)?
        .strip_prefix('(')?
        .strip_suffix(')')?
        .parse()
        .ok()
}

fn compare_database_indexes(
    path: &str,
    before: &[DatabaseIndex],
    after: &[DatabaseIndex],
    findings: &mut Vec<CompatibilityFinding>,
) {
    let current = after
        .iter()
        .map(database_index_key)
        .collect::<BTreeSet<_>>();
    for index in before {
        let key = database_index_key(index);
        if !current.contains(&key) {
            findings.push(database_finding(
                "database.index_removed",
                &format!("{path}.index:{key}"),
                CompatibilityStatus::PotentiallyBreaking,
                index.evidence.get(),
                "index or unique-index constraint was removed",
            ));
        }
    }
}

fn database_index_key(index: &DatabaseIndex) -> String {
    format!("{}:{}", index.unique, index.columns.join(","))
}

fn compare_database_foreign_keys(
    path: &str,
    before: &[DatabaseForeignKey],
    after: &[DatabaseForeignKey],
    findings: &mut Vec<CompatibilityFinding>,
) {
    let current = after
        .iter()
        .map(database_foreign_key)
        .collect::<BTreeSet<_>>();
    for foreign_key in before {
        let key = database_foreign_key(foreign_key);
        if !current.contains(&key) {
            findings.push(database_finding(
                "database.foreign_key_removed",
                &format!("{path}.foreign_key:{key}"),
                CompatibilityStatus::PotentiallyBreaking,
                foreign_key.evidence.get(),
                "foreign-key constraint was removed",
            ));
        }
    }
}

fn database_foreign_key(foreign_key: &DatabaseForeignKey) -> String {
    format!(
        "{}->{}:{}",
        foreign_key.columns.join(","),
        foreign_key.referenced_table,
        foreign_key.referenced_columns.join(",")
    )
}

fn compare_migrations(
    before: &DataDocument,
    after: &DataDocument,
    findings: &mut Vec<CompatibilityFinding>,
) {
    match (&before.migration, &after.migration) {
        (Some(previous), Some(candidate)) => {
            if previous.revision == candidate.revision
                && previous.down_revision != candidate.down_revision
            {
                findings.push(finding(
                    "database.migration_predecessor_changed",
                    "database.migration",
                    CompatibilityStatus::Breaking,
                    vec!["the predecessor of an existing migration revision changed".to_owned()],
                    vec![
                        data_locator(before, previous.evidence.get()),
                        data_locator(after, candidate.evidence.get()),
                    ],
                    vec![
                        "validate the full migration DAG on a production-like snapshot".to_owned(),
                    ],
                ));
            } else if previous.revision != candidate.revision
                && candidate.down_revision != previous.revision
            {
                findings.push(finding(
                    "database.migration_predecessor_conflict",
                    "database.migration",
                    CompatibilityStatus::PotentiallyBreaking,
                    vec![
                        "candidate migration does not directly follow the previous revision"
                            .to_owned(),
                    ],
                    vec![
                        data_locator(before, previous.evidence.get()),
                        data_locator(after, candidate.evidence.get()),
                    ],
                    vec![
                        "resolve migration branches and validate the complete predecessor graph"
                            .to_owned(),
                    ],
                ));
            }
            if previous
                .order_hint
                .zip(candidate.order_hint)
                .is_some_and(|(previous_order, candidate_order)| candidate_order < previous_order)
            {
                findings.push(finding(
                    "database.migration_order_regressed",
                    "database.migration",
                    CompatibilityStatus::Breaking,
                    vec!["candidate migration order precedes the previous order hint".to_owned()],
                    vec![
                        data_locator(before, previous.evidence.get()),
                        data_locator(after, candidate.evidence.get()),
                    ],
                    vec![
                        "apply the complete migration sequence to an empty and upgraded database"
                            .to_owned(),
                    ],
                ));
            }
        }
        (Some(previous), None) => findings.push(finding(
            "database.migration_metadata_removed",
            "database.migration",
            CompatibilityStatus::Unknown,
            vec!["candidate document lacks previously modeled migration metadata".to_owned()],
            vec![data_locator(before, previous.evidence.get())],
            vec!["restore migration ancestry metadata before release".to_owned()],
        )),
        (None, Some(candidate)) => findings.push(finding(
            "database.migration_metadata_added",
            "database.migration",
            CompatibilityStatus::Unknown,
            vec![
                "migration ancestry was added without a comparable predecessor document".to_owned(),
            ],
            vec![data_locator(after, candidate.evidence.get())],
            vec!["validate the candidate against the deployed migration head".to_owned()],
        )),
        (None, None) => {}
    }
}

fn database_finding(
    code: &str,
    path: &str,
    status: CompatibilityStatus,
    line: u32,
    factor: &str,
) -> CompatibilityFinding {
    finding(
        code,
        path,
        status,
        vec![factor.to_owned()],
        vec![format!("line:{line}")],
        vec!["run database migration and affected reader/writer integration tests".to_owned()],
    )
}

fn data_locator(document: &DataDocument, line: u32) -> String {
    format!("{}:{line}", document.source_path)
}

fn compare_graphql_type(
    before: &GraphqlTypeDefinition,
    after: &GraphqlTypeDefinition,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let after_fields = after
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field))
        .collect::<BTreeMap<_, _>>();
    for previous_field in &before.fields {
        let Some(current_field) = after_fields.get(previous_field.name.as_str()) else {
            findings.push(graphql_breaking(
                "graphql.field_removed",
                &previous_field.coordinate,
                previous_field.lines.start,
                "field is absent from the candidate schema",
            ));
            continue;
        };
        compare_graphql_field(previous_field, current_field, findings);
    }
    let after_enum_values = after.enum_values.iter().collect::<BTreeSet<_>>();
    for value in &before.enum_values {
        if !after_enum_values.contains(value) {
            findings.push(graphql_breaking(
                "graphql.enum_value_removed",
                &format!("{}.{}", before.name, value),
                before.lines.start,
                "enum value is absent from the candidate schema",
            ));
        }
    }
}

fn compare_graphql_field(
    before: &GraphqlFieldDefinition,
    after: &GraphqlFieldDefinition,
    findings: &mut Vec<CompatibilityFinding>,
) {
    if type_narrowed(&before.type_ref, &after.type_ref) {
        findings.push(graphql_breaking(
            "graphql.return_type_narrowed",
            &before.coordinate,
            after.lines.start,
            &format!(
                "return type changed from {} to {}",
                before.type_ref.as_graphql(),
                after.type_ref.as_graphql()
            ),
        ));
    }
    let before_arguments = before
        .arguments
        .iter()
        .map(|argument| argument.name.as_str())
        .collect::<BTreeSet<_>>();
    for argument in &after.arguments {
        if !before_arguments.contains(argument.name.as_str())
            && type_is_non_null(&argument.type_ref)
            && argument.default_value.is_none()
        {
            findings.push(graphql_breaking(
                "graphql.required_argument_added",
                &format!("{}({})", before.coordinate, argument.name),
                argument.lines.start,
                "new argument is non-null and has no default",
            ));
        }
    }
}

fn compare_persisted_operations(
    before: &GraphqlDocument,
    after: &GraphqlDocument,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let current = after
        .persisted_operations
        .iter()
        .map(|operation| operation.id.as_str())
        .collect::<BTreeSet<_>>();
    for operation in &before.persisted_operations {
        if !current.contains(operation.id.as_str()) {
            findings.push(graphql_breaking(
                "graphql.persisted_operation_invalidated",
                &operation.id,
                operation.lines.start,
                "persisted operation identifier is absent from the candidate manifest",
            ));
        }
    }
}

fn event_key(observation: &EventObservation) -> Option<String> {
    observation.channel.as_ref().map(|channel| {
        format!(
            "{:?}:{}:{channel}:{:?}",
            observation.broker,
            observation.namespace.as_deref().unwrap_or(""),
            observation.role
        )
    })
}

fn compare_event_observation(
    before: &EventObservation,
    after: &EventObservation,
    path: &str,
    findings: &mut Vec<CompatibilityFinding>,
) {
    if before.routing_key != after.routing_key || before.partition_key != after.partition_key {
        findings.push(event_breaking(
            "event.routing_key_changed",
            path,
            after,
            "routing or partition key declaration changed",
        ));
    }
    let (Some(before_schema), Some(after_schema)) = (&before.schema, &after.schema) else {
        if before.schema.is_some() != after.schema.is_some() {
            findings.push(finding(
                "event.schema_coverage_changed",
                path,
                CompatibilityStatus::Unknown,
                vec!["payload schema is absent from one side of the comparison".to_owned()],
                event_evidence(after),
                vec!["validate producer and consumer payloads at runtime".to_owned()],
            ));
        }
        return;
    };
    if before_schema.version != after_schema.version
        && before_schema.version.is_some()
        && after_schema.version.is_some()
    {
        findings.push(event_breaking(
            "event.schema_version_changed",
            path,
            after,
            "explicit schema version changed",
        ));
    }
    let before_fields = before_schema
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field))
        .collect::<BTreeMap<_, _>>();
    let after_fields = after_schema
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field))
        .collect::<BTreeMap<_, _>>();
    for (name, previous) in &before_fields {
        let Some(candidate) = after_fields.get(name) else {
            findings.push(event_breaking(
                "event.field_removed",
                &format!("{path}.{name}"),
                after,
                "payload field is absent from the candidate schema",
            ));
            continue;
        };
        if previous.field_type != candidate.field_type {
            findings.push(event_breaking(
                "event.field_type_changed",
                &format!("{path}.{name}"),
                after,
                "payload field type changed",
            ));
        }
    }
    for (name, candidate) in after_fields {
        if candidate.required && !before_fields.contains_key(name) {
            findings.push(event_breaking(
                "event.required_field_added",
                &format!("{path}.{name}"),
                after,
                "new payload field is required",
            ));
        }
    }
}

fn compare_proto_messages(
    before: &ProtoFile,
    after: &ProtoFile,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let before_messages = flatten_messages(&before.messages);
    let after_messages = flatten_messages(&after.messages);
    for (name, previous) in before_messages {
        let Some(candidate) = after_messages.get(name) else {
            findings.push(proto_breaking(
                "protobuf.message_removed",
                name,
                previous.line,
                "message is absent from the candidate schema",
            ));
            continue;
        };
        let fields_by_number = candidate
            .fields
            .iter()
            .map(|field| (field.number, field))
            .collect::<BTreeMap<_, _>>();
        let fields_by_name = candidate
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field))
            .collect::<BTreeMap<_, _>>();
        for field in &previous.fields {
            let Some(number_match) = fields_by_number.get(&field.number) else {
                findings.push(proto_breaking(
                    "protobuf.field_removed",
                    &format!("{name}.{}", field.name),
                    field.line,
                    "field number is absent from the candidate message",
                ));
                continue;
            };
            if number_match.name != field.name {
                findings.push(proto_breaking(
                    "protobuf.field_number_reused",
                    &format!("{name}.{}", field.number),
                    number_match.line,
                    "field number is assigned to a different name",
                ));
            }
            if number_match.wire_type != field.wire_type
                || number_match.type_name != field.type_name
                || number_match.cardinality != field.cardinality
            {
                findings.push(proto_breaking(
                    "protobuf.field_wire_incompatible",
                    &format!("{name}.{}", field.number),
                    number_match.line,
                    "field type, wire encoding, or cardinality changed",
                ));
            }
            if let Some(name_match) = fields_by_name.get(field.name.as_str())
                && name_match.number != field.number
            {
                findings.push(proto_breaking(
                    "protobuf.field_number_changed",
                    &format!("{name}.{}", field.name),
                    name_match.line,
                    "field name moved to a different number",
                ));
            }
        }
        for field in &candidate.fields {
            if field.cardinality == ProtoFieldCardinality::Required
                && !previous
                    .fields
                    .iter()
                    .any(|previous| previous.name == field.name)
            {
                findings.push(proto_breaking(
                    "protobuf.required_field_added",
                    &format!("{name}.{}", field.name),
                    field.line,
                    "required proto2 field was added",
                ));
            }
        }
    }
}

fn compare_proto_enums(
    before: &ProtoFile,
    after: &ProtoFile,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let before_enums = flatten_enums(&before.messages, &before.enums);
    let after_enums = flatten_enums(&after.messages, &after.enums);
    for (name, previous) in before_enums {
        let Some(candidate) = after_enums.get(name) else {
            findings.push(proto_breaking(
                "protobuf.enum_removed",
                name,
                previous.line,
                "enum is absent from the candidate schema",
            ));
            continue;
        };
        let current_numbers = candidate
            .values
            .iter()
            .map(|value| (value.number, value.name.as_str()))
            .collect::<BTreeMap<_, _>>();
        for value in &previous.values {
            if current_numbers.get(&value.number) != Some(&value.name.as_str()) {
                findings.push(proto_breaking(
                    "protobuf.enum_numeric_changed",
                    &format!("{name}.{}", value.name),
                    value.line,
                    "enum numeric value was removed or reassigned",
                ));
            }
        }
    }
}

fn compare_proto_services(
    before: &ProtoFile,
    after: &ProtoFile,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let current = after
        .services
        .iter()
        .map(|service| (service.full_name.as_str(), service))
        .collect::<BTreeMap<_, _>>();
    for service in &before.services {
        let Some(candidate) = current.get(service.full_name.as_str()) else {
            findings.push(proto_breaking(
                "protobuf.service_removed",
                &service.full_name,
                service.line,
                "service is absent from the candidate schema",
            ));
            continue;
        };
        let methods = candidate
            .methods
            .iter()
            .map(|method| (method.name.as_str(), method))
            .collect::<BTreeMap<_, _>>();
        for method in &service.methods {
            let path = format!("{}/{}", service.full_name, method.name);
            let Some(current_method) = methods.get(method.name.as_str()) else {
                findings.push(proto_breaking(
                    "protobuf.method_removed",
                    &path,
                    method.line,
                    "RPC method is absent from the candidate service",
                ));
                continue;
            };
            if method.request_type != current_method.request_type
                || method.response_type != current_method.response_type
                || method.client_streaming != current_method.client_streaming
                || method.server_streaming != current_method.server_streaming
            {
                findings.push(proto_breaking(
                    "protobuf.method_signature_changed",
                    &path,
                    current_method.line,
                    "RPC request, response, or streaming mode changed",
                ));
            }
        }
    }
}

fn flatten_messages(messages: &[ProtoMessage]) -> BTreeMap<&str, &ProtoMessage> {
    fn append<'a>(messages: &'a [ProtoMessage], output: &mut BTreeMap<&'a str, &'a ProtoMessage>) {
        for message in messages {
            output.insert(message.full_name.as_str(), message);
            append(&message.messages, output);
        }
    }
    let mut output = BTreeMap::new();
    append(messages, &mut output);
    output
}

fn flatten_enums<'a>(
    messages: &'a [ProtoMessage],
    enums: &'a [ProtoEnum],
) -> BTreeMap<&'a str, &'a ProtoEnum> {
    fn append<'a>(messages: &'a [ProtoMessage], output: &mut BTreeMap<&'a str, &'a ProtoEnum>) {
        for message in messages {
            for enumeration in &message.enums {
                output.insert(enumeration.full_name.as_str(), enumeration);
            }
            append(&message.messages, output);
        }
    }
    let mut output = enums
        .iter()
        .map(|enumeration| (enumeration.full_name.as_str(), enumeration))
        .collect::<BTreeMap<_, _>>();
    append(messages, &mut output);
    output
}

fn type_narrowed(before: &GraphqlTypeRef, after: &GraphqlTypeRef) -> bool {
    if before == after {
        return false;
    }
    match (before, after) {
        (
            GraphqlTypeRef::Named {
                name: before_name,
                non_null: before_non_null,
            },
            GraphqlTypeRef::Named {
                name: after_name,
                non_null: after_non_null,
            },
        ) => before_name != after_name || (!before_non_null && *after_non_null),
        (
            GraphqlTypeRef::List {
                element: before_element,
                non_null: before_non_null,
            },
            GraphqlTypeRef::List {
                element: after_element,
                non_null: after_non_null,
            },
        ) => (!before_non_null && *after_non_null) || type_narrowed(before_element, after_element),
        _ => true,
    }
}

fn type_is_non_null(type_ref: &GraphqlTypeRef) -> bool {
    match type_ref {
        GraphqlTypeRef::Named { non_null, .. } | GraphqlTypeRef::List { non_null, .. } => *non_null,
    }
}

fn graphql_breaking(code: &str, path: &str, line: u32, factor: &str) -> CompatibilityFinding {
    finding(
        code,
        path,
        CompatibilityStatus::Breaking,
        vec![factor.to_owned()],
        vec![format!("line:{line}")],
        vec!["run affected GraphQL consumer operations against the candidate schema".to_owned()],
    )
}

fn event_breaking(
    code: &str,
    path: &str,
    observation: &EventObservation,
    factor: &str,
) -> CompatibilityFinding {
    finding(
        code,
        path,
        CompatibilityStatus::Breaking,
        vec![factor.to_owned()],
        event_evidence(observation),
        vec!["validate all event producers and consumers against the candidate schema".to_owned()],
    )
}

fn event_evidence(observation: &EventObservation) -> Vec<String> {
    observation
        .evidence
        .iter()
        .map(|evidence| format!("line:{}:{}", evidence.line, evidence.text))
        .collect()
}

fn proto_breaking(code: &str, path: &str, line: u32, factor: &str) -> CompatibilityFinding {
    finding(
        code,
        path,
        CompatibilityStatus::Breaking,
        vec![factor.to_owned()],
        vec![format!("line:{line}")],
        vec![
            "regenerate clients and run protobuf wire-compatibility tests before release"
                .to_owned(),
        ],
    )
}

fn finding(
    code: &str,
    path: &str,
    status: CompatibilityStatus,
    factors: Vec<String>,
    evidence: Vec<String>,
    recommended_validations: Vec<String>,
) -> CompatibilityFinding {
    CompatibilityFinding {
        code: code.to_owned(),
        path: path.to_owned(),
        status,
        factors: bounded_values(factors),
        evidence: bounded_values(evidence),
        recommended_validations: bounded_values(recommended_validations),
    }
}

fn bounded_values(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .take(MAX_FINDING_VALUES)
        .map(|value| value.chars().take(MAX_FINDING_VALUE_CHARS).collect())
        .collect()
}

fn fingerprints<T: Serialize>(
    before: &T,
    after: &T,
    family: &str,
    findings: &mut Vec<CompatibilityFinding>,
) -> (String, String) {
    let before = structured_fingerprint(before);
    let after = structured_fingerprint(after);
    if before.is_none() || after.is_none() {
        findings.push(finding(
            &format!("{family}.fingerprint_unavailable"),
            family,
            CompatibilityStatus::Unknown,
            vec!["structured contract serialization failed".to_owned()],
            Vec::new(),
            vec!["report the serialization failure and rerun extraction".to_owned()],
        ));
    }
    (
        before.unwrap_or_else(|| "unavailable".to_owned()),
        after.unwrap_or_else(|| "unavailable".to_owned()),
    )
}

fn structured_fingerprint<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_vec(value)
        .ok()
        .map(|encoded| blake3::hash(&encoded).to_hex().to_string())
}

fn finish_report(
    before_fingerprint: String,
    after_fingerprint: String,
    mut findings: Vec<CompatibilityFinding>,
) -> CompatibilityReport {
    findings.sort_by(|left, right| left.path.cmp(&right.path).then(left.code.cmp(&right.code)));
    findings.dedup_by(|left, right| left.path == right.path && left.code == right.code);
    let status = if findings
        .iter()
        .any(|finding| finding.status == CompatibilityStatus::Breaking)
    {
        CompatibilityStatus::Breaking
    } else if findings
        .iter()
        .any(|finding| finding.status == CompatibilityStatus::PotentiallyBreaking)
    {
        CompatibilityStatus::PotentiallyBreaking
    } else if findings
        .iter()
        .any(|finding| finding.status == CompatibilityStatus::Incomparable)
    {
        CompatibilityStatus::Incomparable
    } else if findings
        .iter()
        .any(|finding| finding.status == CompatibilityStatus::Unknown)
    {
        CompatibilityStatus::Unknown
    } else {
        CompatibilityStatus::Compatible
    };
    CompatibilityReport {
        status,
        before_fingerprint,
        after_fingerprint,
        findings,
    }
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::RepoId;

    use super::{
        CompatibilityStatus, compare_database_contracts, compare_event_contracts, compare_graphql_contracts, compare_http_contracts, compare_package_contracts, compare_protobuf_contracts
    };
    use crate::{
        DataWarning, MigrationMetadata, extract_asyncapi, extract_data_artifact, extract_graphql_document, extract_openapi, extract_package_manifest, extract_protobuf
    };

    const OPENAPI_PREFIX: &str =
        r#"{"openapi":"3.0.0","info":{"title":"test","version":"1"},"paths":{"#;
    const OPENAPI_SUFFIX: &str = "}}";

    fn http_contract(operations: &str) -> Vec<crate::HttpBoundary> {
        let source = format!("{OPENAPI_PREFIX}{operations}{OPENAPI_SUFFIX}");
        extract_openapi(&RepoId::new("repo:test"), "openapi.json", &source)
            .expect("test OpenAPI contract should parse")
    }

    fn package_contract(source: &str) -> crate::PackageManifest {
        extract_package_manifest("package.json", source)
            .expect("test package manifest should parse")
    }

    fn database_contract(path: &str, source: &str) -> crate::DataDocument {
        extract_data_artifact(path, source).expect("test database contract should parse")
    }

    #[test]
    fn graphql_comparison_should_detect_removed_field_and_required_argument() {
        let before = extract_graphql_document(
            "before.graphql",
            include_str!("../../../fixtures/contracts/protocols/graphql/before.graphql"),
        );
        let after = extract_graphql_document(
            "after.graphql",
            include_str!("../../../fixtures/contracts/protocols/graphql/after.graphql"),
        );
        let report = before
            .as_ref()
            .ok()
            .zip(after.as_ref().ok())
            .map(|(before, after)| compare_graphql_contracts(before, after));

        assert!(matches!(
            report,
            Some(report)
                if report.status == CompatibilityStatus::Breaking
                    && report.findings.iter().any(|finding| {
                        finding.code == "graphql.field_removed"
                            && finding.path == "Order.status"
                    })
                    && report.findings.iter().any(|finding| {
                        finding.code == "graphql.required_argument_added"
                            && finding.path == "Query.order(region)"
                    })
        ));
    }

    #[test]
    fn event_comparison_should_detect_required_field_and_type_changes() {
        let before = extract_asyncapi(
            "before.yaml",
            include_str!("../../../fixtures/contracts/protocols/event/before.yaml"),
        );
        let after = extract_asyncapi(
            "after.yaml",
            include_str!("../../../fixtures/contracts/protocols/event/after.yaml"),
        );
        let report = before
            .as_ref()
            .ok()
            .zip(after.as_ref().ok())
            .map(|(before, after)| compare_event_contracts(before, after));

        assert!(matches!(
            report,
            Some(report)
                if report.status == CompatibilityStatus::Breaking
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "event.required_field_added")
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "event.field_type_changed")
        ));
    }

    #[test]
    fn protobuf_comparison_should_detect_field_number_wire_reuse() {
        let before = extract_protobuf(
            "before.proto",
            include_str!("../../../fixtures/contracts/protocols/protobuf/before.proto"),
        );
        let after = extract_protobuf(
            "after.proto",
            include_str!("../../../fixtures/contracts/protocols/protobuf/after.proto"),
        );
        let report = before
            .as_ref()
            .ok()
            .zip(after.as_ref().ok())
            .map(|(before, after)| compare_protobuf_contracts(before, after));

        assert!(matches!(
            report,
            Some(report)
                if report.status == CompatibilityStatus::Breaking
                    && report.findings.iter().any(|finding| {
                        finding.code == "protobuf.field_number_reused"
                    })
                    && report.findings.iter().any(|finding| {
                        finding.code == "protobuf.field_wire_incompatible"
                    })
        ));
    }

    #[test]
    fn http_comparison_should_report_removed_operation_as_breaking() {
        let before =
            http_contract(r#""/orders":{"get":{"responses":{"200":{"description":"ok"}}}}"#);
        let after = Vec::new();

        let report = compare_http_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Breaking
                    && report.before_fingerprint != report.after_fingerprint
                    && report.findings.iter().any(|finding| {
                        finding.code == "http.operation_removed"
                            && !finding.evidence.is_empty()
                            && !finding.recommended_validations.is_empty()
                    })
        ));
    }

    #[test]
    fn http_comparison_should_report_exact_addition_but_not_full_schema_compatibility() {
        let after =
            http_contract(r#""/orders":{"post":{"responses":{"201":{"description":"created"}}}}"#);

        let report = compare_http_contracts(&[], &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Incomparable
                    && report.findings.iter().any(|finding| {
                        finding.code == "http.operation_added"
                            && finding.status == CompatibilityStatus::Compatible
                    })
                    && report.findings.iter().any(|finding| {
                        finding.code == "http.schema_semantics_unmodeled"
                    })
        ));
    }

    #[test]
    fn http_comparison_should_report_unique_method_change_as_potentially_breaking() {
        let before =
            http_contract(r#""/orders":{"get":{"responses":{"200":{"description":"ok"}}}}"#);
        let mut after = before.clone();
        after[0].method = "POST".to_owned();

        let report = compare_http_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::PotentiallyBreaking
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "http.operation_changed")
        ));
    }

    #[test]
    fn http_comparison_should_report_unique_path_change_as_potentially_breaking() {
        let before =
            http_contract(r#""/orders":{"get":{"responses":{"200":{"description":"ok"}}}}"#);
        let mut after = before.clone();
        after[0].path = "/v2/orders".to_owned();

        let report = compare_http_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::PotentiallyBreaking
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "http.operation_changed")
        ));
    }

    #[test]
    fn http_comparison_should_report_duplicate_and_incomplete_inventory() {
        let mut before =
            http_contract(r#""/orders":{"get":{"responses":{"200":{"description":"ok"}}}}"#);
        before[0].evidence.confidence = 0.5;
        before.push(before[0].clone());

        let report = compare_http_contracts(&before, &before);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Incomparable
                    && report.findings.iter().any(|finding| {
                        finding.code == "http.operation_duplicate"
                    })
                    && report.findings.iter().any(|finding| {
                        finding.code == "http.operation_incomplete"
                            && finding.status == CompatibilityStatus::Unknown
                    })
        ));
    }

    #[test]
    fn package_comparison_should_report_removed_export_as_breaking() {
        let before = package_contract(
            r#"{"name":"demo","version":"1.0.0","exports":{".":"./index.js","./admin":"./admin.js"}}"#,
        );
        let after =
            package_contract(r#"{"name":"demo","version":"1.0.0","exports":{".":"./index.js"}}"#);

        let report = compare_package_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Breaking
                    && report.findings.iter().any(|finding| {
                        finding.code == "package.export_removed" && finding.path == "./admin"
                    })
        ));
    }

    #[test]
    fn package_comparison_should_report_coordinate_rename_as_breaking() {
        let before = package_contract(r#"{"name":"old-name","version":"1.0.0"}"#);
        let after = package_contract(r#"{"name":"new-name","version":"1.0.0"}"#);

        let report = compare_package_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Breaking
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "package.coordinate_renamed")
        ));
    }

    #[test]
    fn package_comparison_should_report_major_version_and_peer_range_drift() {
        let before = package_contract(
            r#"{"name":"demo","version":"1.0.0","dependencies":{"router":"^1.0.0"},"peerDependencies":{"react":"^18.0.0"}}"#,
        );
        let after = package_contract(
            r#"{"name":"demo","version":"2.0.0","dependencies":{"router":"^2.0.0"},"peerDependencies":{"react":"^19.0.0"}}"#,
        );

        let report = compare_package_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Breaking
                    && report.findings.iter().any(|finding| {
                        finding.code == "package.declared_version_changed"
                            && finding.status == CompatibilityStatus::Breaking
                    })
                    && report.findings.iter().any(|finding| {
                        finding.code == "package.dependency_drift"
                            && finding.status == CompatibilityStatus::PotentiallyBreaking
                    })
                    && report
                        .findings
                        .iter()
                        .filter(|finding| finding.code == "package.dependency_drift")
                        .count()
                        == 2
        ));
    }

    #[test]
    fn package_comparison_should_report_feature_workspace_and_cycle_limitations() {
        let mut before = package_contract(
            r#"{"name":"demo","version":"1.0.0","exports":{"./feature":"./feature.js"},"workspaces":["a"]}"#,
        );
        let mut after = before.clone();
        before.features = before.exports.clone();
        after.features.clear();
        after.workspace_members[0].value = "b".to_owned();

        let report = compare_package_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::PotentiallyBreaking
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "package.feature_removed")
                    && report.findings.iter().any(|finding| {
                        finding.code == "package.workspace_cycle_unmodeled"
                            && finding.status == CompatibilityStatus::Unknown
                    })
        ));
    }

    #[test]
    fn package_comparison_should_report_unmodeled_types_and_duplicate_exports() {
        let mut manifest =
            package_contract(r#"{"name":"demo","version":"1.0.0","exports":{".":"./index.js"}}"#);
        manifest.exports.push(manifest.exports[0].clone());

        let report = compare_package_contracts(&manifest, &manifest);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Incomparable
                    && report.findings.iter().any(|finding| {
                        finding.code == "package.export_duplicate"
                    })
                    && report.findings.iter().any(|finding| {
                        finding.code == "package.type_declarations_unmodeled"
                            && finding.status == CompatibilityStatus::Unknown
                    })
        ));
    }

    #[test]
    fn database_comparison_should_report_removed_table_and_column_as_breaking() {
        let before = database_contract(
            "schema.sql",
            "CREATE TABLE users (id BIGINT PRIMARY KEY, email TEXT); CREATE TABLE audit (id BIGINT);",
        );
        let after = database_contract("schema.sql", "CREATE TABLE users (id BIGINT PRIMARY KEY);");

        let report = compare_database_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Breaking
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "database.table_removed")
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "database.column_removed")
        ));
    }

    #[test]
    fn database_comparison_should_report_type_narrowing_and_nullability() {
        let before = database_contract(
            "schema.sql",
            "CREATE TABLE users (id BIGINT, email VARCHAR(255) NULL);",
        );
        let after = database_contract(
            "schema.sql",
            "CREATE TABLE users (id INTEGER, email VARCHAR(64) NOT NULL);",
        );

        let report = compare_database_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Breaking
                    && report.findings.iter().filter(|finding| {
                        finding.code == "database.column_type_changed"
                            && finding.status == CompatibilityStatus::Breaking
                    }).count() == 2
                    && report.findings.iter().any(|finding| {
                        finding.code == "database.column_nullability_changed"
                            && finding.status == CompatibilityStatus::Breaking
                    })
        ));
    }

    #[test]
    fn database_comparison_should_report_defaulted_non_null_as_potentially_breaking() {
        let before = database_contract("schema.sql", "CREATE TABLE users (email TEXT NULL);");
        let after = database_contract(
            "schema.sql",
            "CREATE TABLE users (email TEXT DEFAULT 'redacted' NOT NULL);",
        );

        let report = compare_database_contracts(&before, &after);

        assert!(report.findings.iter().any(|finding| {
            finding.code == "database.column_nullability_changed"
                && finding.status == CompatibilityStatus::PotentiallyBreaking
        }));
    }

    #[test]
    fn database_comparison_should_report_unclassified_type_change_as_potentially_breaking() {
        let before = database_contract("schema.sql", "CREATE TABLE users (id UUID);");
        let after = database_contract("schema.sql", "CREATE TABLE users (id TEXT);");

        let report = compare_database_contracts(&before, &after);

        assert!(report.findings.iter().any(|finding| {
            finding.code == "database.column_type_changed"
                && finding.status == CompatibilityStatus::PotentiallyBreaking
        }));
    }

    #[test]
    fn database_comparison_should_report_removed_constraints() {
        let before = database_contract(
            "schema.sql",
            "CREATE TABLE orgs (id BIGINT PRIMARY KEY); \
             CREATE TABLE users (id BIGINT PRIMARY KEY, email TEXT UNIQUE, org_id BIGINT, \
             FOREIGN KEY (org_id) REFERENCES orgs(id)); \
             CREATE INDEX users_email_idx ON users(email);",
        );
        let after = database_contract(
            "schema.sql",
            "CREATE TABLE orgs (id BIGINT PRIMARY KEY); \
             CREATE TABLE users (id BIGINT, email TEXT, org_id BIGINT);",
        );

        let report = compare_database_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Breaking
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "database.primary_key_removed")
                    && report.findings.iter().any(|finding| {
                        finding.code == "database.unique_constraint_removed"
                    })
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "database.index_removed")
                    && report
                        .findings
                        .iter()
                        .any(|finding| finding.code == "database.foreign_key_removed")
        ));
    }

    #[test]
    fn database_comparison_should_report_migration_predecessor_and_order_conflicts() {
        let mut before = database_contract(
            "migrations/002_users.sql",
            "CREATE TABLE users (id BIGINT);",
        );
        let mut after = before.clone();
        let evidence = before.tables[0].evidence;
        before.migration = Some(MigrationMetadata {
            revision: Some("002".to_owned()),
            down_revision: Some("001".to_owned()),
            order_hint: Some(2),
            reversible: false,
            evidence,
        });
        after.migration = Some(MigrationMetadata {
            revision: Some("002".to_owned()),
            down_revision: Some("000".to_owned()),
            order_hint: Some(1),
            reversible: false,
            evidence,
        });

        let report = compare_database_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Breaking
                    && report.findings.iter().any(|finding| {
                        finding.code == "database.migration_predecessor_changed"
                    })
                    && report.findings.iter().any(|finding| {
                        finding.code == "database.migration_order_regressed"
                    })
        ));
    }

    #[test]
    fn database_comparison_should_report_extraction_and_shared_consumer_unknowns() {
        let before = database_contract("schema.sql", "CREATE TABLE users (id BIGINT);");
        let mut after = before.clone();
        after.incomplete = true;
        after.warnings.push(DataWarning::UnsupportedConstruct);

        let report = compare_database_contracts(&before, &after);

        assert!(matches!(
            report,
            report
                if report.status == CompatibilityStatus::Unknown
                    && report.findings.iter().any(|finding| {
                        finding.code == "database.extraction_incomplete"
                    })
                    && report.findings.iter().any(|finding| {
                        finding.code == "database.shared_consumer_impact_unmodeled"
                    })
        ));
    }
}
