//! Conservative extraction of package manifests and declared dependencies.
//!
//! The parsers in this module intentionally support static declarations only. They preserve
//! repository-relative paths and exact source lines, reject malformed structured input, and skip
//! declarations that require executing build logic or resolving variables.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ExtractionBudgets, ExtractionLimitExceeded, ExtractionTracker};

const EXACT_CONFIDENCE: f32 = 1.0;
const STATIC_TEXT_CONFIDENCE: f32 = 0.95;
const PRESENCE_CONFIDENCE: f32 = 0.9;

/// Package manager or language ecosystem associated with an extracted declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PackageEcosystem {
    /// JavaScript packages distributed through npm-compatible registries.
    Npm,
    /// Python distributions.
    Python,
    /// Rust crates.
    Cargo,
    /// Go modules.
    Go,
    /// Java Virtual Machine artifacts addressed by Maven coordinates.
    Maven,
    /// Dependencies declared by Gradle build scripts.
    Gradle,
    /// .NET packages distributed through `NuGet`.
    NuGet,
}

/// Semantic role of a dependency in its declaring package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DependencyScope {
    /// Required by the deployed or executed package.
    Runtime,
    /// Used during local development.
    Dev,
    /// Used only while running tests.
    Test,
    /// Used to compile, generate, or package the project.
    Build,
    /// Supplied by a consuming package or host.
    Peer,
    /// Not required for the package's default operation.
    Optional,
}

/// An exact line retained as evidence for one extracted fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageEvidenceLine {
    /// One-based line number in [`Self::text`]'s source file.
    pub line: u32,
    /// Complete source line without its line terminator.
    #[serde(skip)]
    pub text: String,
}

/// A package identity observed in a manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageCoordinate {
    /// Ecosystem in which the package name is interpreted.
    pub ecosystem: PackageEcosystem,
    /// Package name, module path, or group-and-artifact coordinate.
    pub name: String,
    /// Statically declared package version, when present.
    pub version: Option<String>,
    /// Repository-relative manifest path.
    pub source_path: String,
    /// Exact declaration line.
    pub evidence: PackageEvidenceLine,
    /// Normalized confidence in the inclusive range from zero to one.
    pub confidence: f32,
}

/// A statically declared dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageDependency {
    /// Ecosystem in which the dependency name is interpreted.
    pub ecosystem: PackageEcosystem,
    /// Package name, module path, or group-and-artifact coordinate.
    pub name: String,
    /// Exact version or version range as written, when statically available.
    pub version_or_range: Option<String>,
    /// Semantic role of the dependency.
    pub scope: DependencyScope,
    /// Whether the declaration explicitly makes the dependency optional.
    pub optional: bool,
    /// Feature, target, marker, profile, framework, or configuration condition.
    pub condition: Option<String>,
    /// Repository-relative manifest path.
    pub source_path: String,
    /// Exact declaration line.
    pub evidence: PackageEvidenceLine,
    /// Normalized confidence in the inclusive range from zero to one.
    pub confidence: f32,
}

/// A workspace member, package export, or named feature declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageManifestValue {
    /// Static value exactly represented by the manifest.
    pub value: String,
    /// Repository-relative manifest path.
    pub source_path: String,
    /// Exact declaration line.
    pub evidence: PackageEvidenceLine,
    /// Normalized confidence in the inclusive range from zero to one.
    pub confidence: f32,
}

/// Conservative metadata proving that a package-manager lockfile was observed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockfileMetadata {
    /// Ecosystem controlled by the lockfile.
    pub ecosystem: PackageEcosystem,
    /// Package manager that owns the lockfile.
    pub package_manager: String,
    /// Statically declared lockfile format version, when present.
    pub format_version: Option<String>,
    /// Repository-relative lockfile path.
    pub source_path: String,
    /// Exact line proving the lockfile kind or format.
    pub evidence: PackageEvidenceLine,
    /// Normalized confidence in the inclusive range from zero to one.
    pub confidence: f32,
}

/// Deterministic facts extracted from one package manifest or lockfile.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PackageManifest {
    /// Package identities declared by the source.
    pub packages: Vec<PackageCoordinate>,
    /// Direct dependency declarations. Lockfiles do not contribute transitive dependencies.
    pub dependencies: Vec<PackageDependency>,
    /// Workspace members or included module directories.
    pub workspace_members: Vec<PackageManifestValue>,
    /// Public package export keys.
    pub exports: Vec<PackageManifestValue>,
    /// Named Cargo features.
    pub features: Vec<PackageManifestValue>,
    /// Lockfile-presence observations.
    pub lockfiles: Vec<LockfileMetadata>,
}

/// Failure to recognize or safely parse a package manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PackageManifestError {
    /// The supplied path is not a supported repository-relative package manifest path.
    UnsupportedPath(String),
    /// A supported structured file is malformed or contains an invalid static declaration.
    Malformed {
        /// Repository-relative source path.
        path: String,
        /// Bounded parser explanation.
        message: String,
    },
    /// Extraction exceeded one configured invocation resource.
    LimitExceeded(ExtractionLimitExceeded),
}

impl fmt::Display for PackageManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPath(path) => {
                write!(formatter, "unsupported package manifest path `{path}`")
            }
            Self::Malformed { path, message } => {
                write!(formatter, "malformed package manifest `{path}`: {message}")
            }
            Self::LimitExceeded(error) => error.fmt(formatter),
        }
    }
}

impl Error for PackageManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::LimitExceeded(error) => Some(error),
            Self::UnsupportedPath(_) | Self::Malformed { .. } => None,
        }
    }
}

impl From<ExtractionLimitExceeded> for PackageManifestError {
    fn from(error: ExtractionLimitExceeded) -> Self {
        Self::LimitExceeded(error)
    }
}

/// Extracts static package facts by dispatching on a repository-relative file name.
///
/// Supported inputs are `package.json`, npm/pnpm/Yarn lockfiles, `pyproject.toml`,
/// `requirements*.txt`, `Cargo.toml`, `Cargo.lock`, `go.mod`, `go.work`, `pom.xml`, Gradle build files,
/// `packages.config`, and SDK-style `.csproj` files.
///
/// Results are sorted independently of declaration order. Dynamic Gradle expressions, unresolved
/// Maven properties, VCS requirements, and other declarations requiring evaluation are skipped.
///
/// # Errors
///
/// Returns [`PackageManifestError::UnsupportedPath`] for absolute, parent-traversing, or
/// unsupported paths. Returns [`PackageManifestError::Malformed`] when a recognized structured
/// input cannot be parsed without guessing.
pub fn extract_package_manifest(
    relative_path: &str,
    content: &str,
) -> Result<PackageManifest, PackageManifestError> {
    let mut tracker = ExtractionTracker::new(
        relative_path,
        "code-system-graph.packages",
        &ExtractionBudgets::default(),
    );
    extract_package_manifest_with_tracker(relative_path, content, &mut tracker)
}

/// Extracts package facts using an existing per-invocation tracker.
///
/// # Errors
///
/// Returns an error for malformed input or an exhausted extraction budget.
pub fn extract_package_manifest_with_tracker(
    relative_path: &str,
    content: &str,
    tracker: &mut ExtractionTracker,
) -> Result<PackageManifest, PackageManifestError> {
    tracker.check_input_bytes(u64::try_from(content.len()).unwrap_or(u64::MAX))?;
    validate_relative_path(relative_path)?;
    let file_name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    let lower_name = file_name.to_ascii_lowercase();

    let result = match lower_name.as_str() {
        "package.json" => parse_package_json(relative_path, content),
        "package-lock.json" | "npm-shrinkwrap.json" => parse_npm_lockfile(relative_path, content),
        "pnpm-lock.yaml" => parse_pnpm_lockfile(relative_path, content),
        "yarn.lock" => parse_yarn_lockfile(relative_path, content),
        "pyproject.toml" => parse_pyproject(relative_path, content),
        "cargo.toml" => parse_cargo_manifest(relative_path, content),
        "cargo.lock" => parse_cargo_lockfile(relative_path, content),
        "go.mod" => parse_go_mod(relative_path, content),
        "go.work" => parse_go_work(relative_path, content),
        "pom.xml" => parse_maven(relative_path, content, tracker),
        "build.gradle" | "build.gradle.kts" => parse_gradle(relative_path, content),
        "packages.config" => parse_packages_config(relative_path, content, tracker),
        _ if lower_name.starts_with("requirements")
            && std::path::Path::new(&lower_name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("txt")) =>
        {
            parse_requirements(relative_path, content)
        }
        _ if lower_name.ends_with(".csproj") => parse_csproj(relative_path, content, tracker),
        _ => Err(PackageManifestError::UnsupportedPath(
            relative_path.to_owned(),
        )),
    };
    if matches!(result, Err(PackageManifestError::LimitExceeded(_))) {
        return result;
    }
    tracker.check_structured_time()?;
    let result = result?;

    let result = finalize(result);
    tracker.check_structured_time()?;
    Ok(result)
}

fn validate_relative_path(path: &str) -> Result<(), PackageManifestError> {
    let normalized = path.replace('\\', "/");
    let has_parent = normalized.split('/').any(|part| part == "..");
    let windows_absolute = normalized
        .as_bytes()
        .get(1)
        .is_some_and(|byte| *byte == b':');
    if path.is_empty() || normalized.starts_with('/') || windows_absolute || has_parent {
        return Err(PackageManifestError::UnsupportedPath(path.to_owned()));
    }
    Ok(())
}

fn malformed(path: &str, message: impl Into<String>) -> PackageManifestError {
    PackageManifestError::Malformed {
        path: path.to_owned(),
        message: message.into(),
    }
}

fn finalize(mut manifest: PackageManifest) -> PackageManifest {
    manifest.packages.sort_by(|left, right| {
        (
            left.ecosystem,
            left.name.as_str(),
            left.version.as_deref(),
            left.evidence.line,
        )
            .cmp(&(
                right.ecosystem,
                right.name.as_str(),
                right.version.as_deref(),
                right.evidence.line,
            ))
    });
    manifest.dependencies.sort_by(|left, right| {
        (
            left.ecosystem,
            left.name.as_str(),
            left.scope,
            left.condition.as_deref(),
            left.version_or_range.as_deref(),
            left.evidence.line,
        )
            .cmp(&(
                right.ecosystem,
                right.name.as_str(),
                right.scope,
                right.condition.as_deref(),
                right.version_or_range.as_deref(),
                right.evidence.line,
            ))
    });
    sort_values(&mut manifest.workspace_members);
    sort_values(&mut manifest.exports);
    sort_values(&mut manifest.features);
    manifest.lockfiles.sort_by(|left, right| {
        (
            left.ecosystem,
            left.package_manager.as_str(),
            left.source_path.as_str(),
        )
            .cmp(&(
                right.ecosystem,
                right.package_manager.as_str(),
                right.source_path.as_str(),
            ))
    });
    manifest
}

fn sort_values(values: &mut [PackageManifestValue]) {
    values.sort_by(|left, right| {
        (left.value.as_str(), left.evidence.line).cmp(&(right.value.as_str(), right.evidence.line))
    });
}

fn evidence_at(content: &str, line: usize) -> PackageEvidenceLine {
    let lines = content.lines().collect::<Vec<_>>();
    let index = line.saturating_sub(1).min(lines.len().saturating_sub(1));
    PackageEvidenceLine {
        line: u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX),
        text: lines.get(index).copied().unwrap_or_default().to_owned(),
    }
}

fn evidence_for(content: &str, start_line: usize, token: &str) -> PackageEvidenceLine {
    let start = start_line.saturating_sub(1);
    let line = content
        .lines()
        .enumerate()
        .skip(start)
        .find_map(|(index, source)| source.contains(token).then_some(index + 1))
        .unwrap_or(start_line);
    evidence_at(content, line)
}

fn value_fact(
    path: &str,
    content: &str,
    value: String,
    start_line: usize,
    token: &str,
) -> PackageManifestValue {
    PackageManifestValue {
        value,
        source_path: path.to_owned(),
        evidence: evidence_for(content, start_line, token),
        confidence: EXACT_CONFIDENCE,
    }
}

fn package(
    ecosystem: PackageEcosystem,
    name: String,
    version: Option<String>,
    path: &str,
    evidence: PackageEvidenceLine,
) -> PackageCoordinate {
    PackageCoordinate {
        ecosystem,
        name,
        version,
        source_path: path.to_owned(),
        evidence,
        confidence: EXACT_CONFIDENCE,
    }
}

struct DependencyInput<'a> {
    ecosystem: PackageEcosystem,
    name: String,
    version: Option<String>,
    scope: DependencyScope,
    optional: bool,
    condition: Option<String>,
    path: &'a str,
    evidence: PackageEvidenceLine,
    confidence: f32,
}

fn dependency(input: DependencyInput<'_>) -> PackageDependency {
    PackageDependency {
        ecosystem: input.ecosystem,
        name: input.name,
        version_or_range: input.version,
        scope: input.scope,
        optional: input.optional,
        condition: input.condition,
        source_path: input.path.to_owned(),
        evidence: input.evidence,
        confidence: input.confidence,
    }
}

fn parse_package_json(path: &str, content: &str) -> Result<PackageManifest, PackageManifestError> {
    let root: Value =
        serde_json::from_str(content).map_err(|error| malformed(path, error.to_string()))?;
    let object = root
        .as_object()
        .ok_or_else(|| malformed(path, "top-level JSON value must be an object"))?;
    let mut result = PackageManifest::default();

    if let Some(name_value) = object.get("name") {
        let name = required_json_string(path, "name", name_value)?;
        let version = object
            .get("version")
            .map(|value| required_json_string(path, "version", value))
            .transpose()?;
        result.packages.push(package(
            PackageEcosystem::Npm,
            name.to_owned(),
            version.map(str::to_owned),
            path,
            evidence_for(content, 1, "\"name\""),
        ));
    } else if object.contains_key("version") {
        return Err(malformed(path, "`version` requires a static `name`"));
    }

    let optional_peers = optional_npm_peers(path, object)?;
    for (section, scope, section_optional) in [
        ("dependencies", DependencyScope::Runtime, false),
        ("devDependencies", DependencyScope::Dev, false),
        ("peerDependencies", DependencyScope::Peer, false),
        ("optionalDependencies", DependencyScope::Optional, true),
    ] {
        let Some(value) = object.get(section) else {
            continue;
        };
        let entries = value
            .as_object()
            .ok_or_else(|| malformed(path, format!("`{section}` must be an object")))?;
        let section_line = evidence_for(content, 1, &format!("\"{section}\"")).line as usize;
        for (name, version) in entries {
            let version = required_json_string(path, section, version)?;
            let optional = section_optional
                || (section == "peerDependencies" && optional_peers.contains(name));
            result.dependencies.push(dependency(DependencyInput {
                ecosystem: PackageEcosystem::Npm,
                name: name.clone(),
                version: Some(version.to_owned()),
                scope,
                optional,
                condition: None,
                path,
                evidence: evidence_for(content, section_line, &format!("\"{name}\"")),
                confidence: EXACT_CONFIDENCE,
            }));
        }
    }

    if let Some(workspaces) = object.get("workspaces") {
        let values = if let Some(array) = workspaces.as_array() {
            array
        } else {
            workspaces
                .as_object()
                .and_then(|map| map.get("packages"))
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    malformed(
                        path,
                        "`workspaces` must be an array or an object with a `packages` array",
                    )
                })?
        };
        for value in values {
            let member = required_json_string(path, "workspaces", value)?;
            result
                .workspace_members
                .push(value_fact(path, content, member.to_owned(), 1, member));
        }
    }

    if let Some(exports) = object.get("exports") {
        collect_json_export_keys(path, content, exports, &mut result.exports)?;
    }
    Ok(result)
}

fn optional_npm_peers(
    path: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<BTreeSet<String>, PackageManifestError> {
    let Some(value) = object.get("peerDependenciesMeta") else {
        return Ok(BTreeSet::new());
    };
    let entries = value
        .as_object()
        .ok_or_else(|| malformed(path, "`peerDependenciesMeta` must be an object"))?;
    let mut optional = BTreeSet::new();
    for (name, metadata) in entries {
        let metadata = metadata.as_object().ok_or_else(|| {
            malformed(
                path,
                format!("peer metadata for `{name}` must be an object"),
            )
        })?;
        if let Some(flag) = metadata.get("optional") {
            let flag = flag.as_bool().ok_or_else(|| {
                malformed(
                    path,
                    format!("peer metadata `optional` for `{name}` must be a boolean"),
                )
            })?;
            if flag {
                optional.insert(name.clone());
            }
        }
    }
    Ok(optional)
}

fn required_json_string<'a>(
    path: &str,
    field: &str,
    value: &'a Value,
) -> Result<&'a str, PackageManifestError> {
    value
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| malformed(path, format!("`{field}` must contain non-empty strings")))
}

fn collect_json_export_keys(
    path: &str,
    content: &str,
    exports: &Value,
    output: &mut Vec<PackageManifestValue>,
) -> Result<(), PackageManifestError> {
    match exports {
        Value::String(target) if !target.is_empty() => {
            output.push(value_fact(path, content, ".".to_owned(), 1, "\"exports\""));
        }
        Value::Object(map) => {
            for (key, value) in map {
                if key.starts_with('.') {
                    output.push(value_fact(
                        path,
                        content,
                        key.clone(),
                        1,
                        &format!("\"{key}\""),
                    ));
                }
                validate_json_export_target(path, value)?;
            }
        }
        _ => return Err(malformed(path, "`exports` must be a string or object")),
    }
    Ok(())
}

fn validate_json_export_target(path: &str, value: &Value) -> Result<(), PackageManifestError> {
    match value {
        Value::String(_) | Value::Null => Ok(()),
        Value::Array(values) => {
            for item in values {
                validate_json_export_target(path, item)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for item in map.values() {
                validate_json_export_target(path, item)?;
            }
            Ok(())
        }
        _ => Err(malformed(
            path,
            "`exports` targets must be strings, null, arrays, or condition objects",
        )),
    }
}

fn parse_npm_lockfile(path: &str, content: &str) -> Result<PackageManifest, PackageManifestError> {
    let root: Value =
        serde_json::from_str(content).map_err(|error| malformed(path, error.to_string()))?;
    let object = root
        .as_object()
        .ok_or_else(|| malformed(path, "top-level JSON value must be an object"))?;
    let version = object
        .get("lockfileVersion")
        .map(|value| match value {
            Value::Number(number) => Ok(number.to_string()),
            Value::String(text) if !text.is_empty() => Ok(text.clone()),
            _ => Err(malformed(
                path,
                "`lockfileVersion` must be a string or number",
            )),
        })
        .transpose()?;
    if object.is_empty() {
        return Err(malformed(path, "lockfile object must not be empty"));
    }
    let token = if version.is_some() {
        "\"lockfileVersion\""
    } else {
        "{"
    };
    Ok(lockfile_result(path, content, "npm", version, token))
}

fn parse_pnpm_lockfile(path: &str, content: &str) -> Result<PackageManifest, PackageManifestError> {
    reject_nul(path, content)?;
    let declaration = content
        .lines()
        .enumerate()
        .find_map(|(index, line)| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("lockfileVersion:")
                .map(|value| (index + 1, unquote_scalar(value.trim())))
        })
        .ok_or_else(|| malformed(path, "missing static `lockfileVersion`"))?;
    if declaration.1.is_empty() {
        return Err(malformed(path, "`lockfileVersion` must not be empty"));
    }
    Ok(lockfile_result_at(
        path,
        content,
        "pnpm",
        Some(declaration.1),
        declaration.0,
    ))
}

fn parse_yarn_lockfile(path: &str, content: &str) -> Result<PackageManifest, PackageManifestError> {
    reject_nul(path, content)?;
    let first = content
        .lines()
        .enumerate()
        .find(|(_, line)| !line.trim().is_empty())
        .ok_or_else(|| malformed(path, "lockfile must not be empty"))?;
    let is_v1 = first.1.contains("yarn lockfile v1");
    let metadata_line = content
        .lines()
        .enumerate()
        .find(|(_, line)| line.trim() == "__metadata:")
        .map(|(index, _)| index + 1);
    if !is_v1 && metadata_line.is_none() {
        return Err(malformed(
            path,
            "missing Yarn v1 header or Berry `__metadata` section",
        ));
    }
    let version = if is_v1 {
        Some("1".to_owned())
    } else {
        metadata_line.and_then(|start| {
            content
                .lines()
                .skip(start)
                .find_map(|line| line.trim().strip_prefix("version:").map(str::trim))
                .map(unquote_scalar)
        })
    };
    Ok(lockfile_result_at(
        path,
        content,
        "yarn",
        version,
        metadata_line.unwrap_or(first.0 + 1),
    ))
}

fn lockfile_result(
    path: &str,
    content: &str,
    manager: &str,
    version: Option<String>,
    token: &str,
) -> PackageManifest {
    let evidence = evidence_for(content, 1, token);
    lockfile_result_with_evidence(path, manager, version, evidence)
}

fn lockfile_result_at(
    path: &str,
    content: &str,
    manager: &str,
    version: Option<String>,
    line: usize,
) -> PackageManifest {
    lockfile_result_with_evidence(path, manager, version, evidence_at(content, line))
}

fn lockfile_result_with_evidence(
    path: &str,
    manager: &str,
    version: Option<String>,
    evidence: PackageEvidenceLine,
) -> PackageManifest {
    ecosystem_lockfile_result(path, PackageEcosystem::Npm, manager, version, evidence)
}

fn ecosystem_lockfile_result(
    path: &str,
    ecosystem: PackageEcosystem,
    manager: &str,
    version: Option<String>,
    evidence: PackageEvidenceLine,
) -> PackageManifest {
    PackageManifest {
        lockfiles: vec![LockfileMetadata {
            ecosystem,
            package_manager: manager.to_owned(),
            format_version: version,
            source_path: path.to_owned(),
            evidence,
            confidence: PRESENCE_CONFIDENCE,
        }],
        ..PackageManifest::default()
    }
}

fn parse_cargo_lockfile(
    path: &str,
    content: &str,
) -> Result<PackageManifest, PackageManifestError> {
    reject_nul(path, content)?;
    let first_content_line = content
        .lines()
        .enumerate()
        .find(|(_, line)| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        })
        .ok_or_else(|| malformed(path, "lockfile must not be empty"))?;
    let version = content
        .lines()
        .enumerate()
        .take_while(|(_, line)| line.trim() != "[[package]]")
        .find_map(|(index, line)| {
            line.trim()
                .strip_prefix("version =")
                .map(|value| (index + 1, unquote_scalar(value.trim())))
        });
    if version
        .as_ref()
        .is_some_and(|(_, version)| version.is_empty())
    {
        return Err(malformed(path, "`version` must not be empty"));
    }
    let evidence = version.as_ref().map_or_else(
        || evidence_at(content, first_content_line.0 + 1),
        |(line, _)| evidence_at(content, *line),
    );
    Ok(ecosystem_lockfile_result(
        path,
        PackageEcosystem::Cargo,
        "cargo",
        version.map(|(_, value)| value),
        evidence,
    ))
}

#[derive(Debug)]
struct TomlEntry {
    section: String,
    key: String,
    value: String,
    line: usize,
}

fn parse_toml(path: &str, content: &str) -> Result<Vec<TomlEntry>, PackageManifestError> {
    reject_nul(path, content)?;
    let lines = content.lines().collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut section = String::new();
    let mut index = 0;
    while index < lines.len() {
        let first_line = index + 1;
        let stripped = strip_line_comment(lines[index], '#')?;
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }
        if trimmed.starts_with('[') {
            let (open, close) = if trimmed.starts_with("[[") {
                ("[[", "]]")
            } else {
                ("[", "]")
            };
            if !trimmed.ends_with(close) {
                return Err(malformed(
                    path,
                    format!("unterminated table at line {first_line}"),
                ));
            }
            let name = &trimmed[open.len()..trimmed.len() - close.len()];
            if name.trim().is_empty() {
                return Err(malformed(path, format!("empty table at line {first_line}")));
            }
            name.trim().clone_into(&mut section);
            index += 1;
            continue;
        }
        let Some((key, initial_value)) = split_top_level_once(trimmed, '=') else {
            return Err(malformed(
                path,
                format!("expected a key/value declaration at line {first_line}"),
            ));
        };
        let key = unquote_scalar(key.trim());
        if key.is_empty() {
            return Err(malformed(path, format!("empty key at line {first_line}")));
        }
        let mut value = initial_value.trim().to_owned();
        while !structured_value_complete(&value)? {
            index += 1;
            let Some(next) = lines.get(index) else {
                return Err(malformed(
                    path,
                    format!("unterminated value beginning at line {first_line}"),
                ));
            };
            let next = strip_line_comment(next, '#')?;
            value.push('\n');
            value.push_str(next.trim());
        }
        if value.is_empty() {
            return Err(malformed(path, format!("empty value at line {first_line}")));
        }
        entries.push(TomlEntry {
            section: section.clone(),
            key,
            value,
            line: first_line,
        });
        index += 1;
    }
    Ok(entries)
}

fn strip_line_comment(line: &str, marker: char) -> Result<&str, PackageManifestError> {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        if character == '\'' || character == '"' {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
        } else if character == marker && quote.is_none() {
            return Ok(&line[..index]);
        }
    }
    if quote.is_some() {
        return Err(PackageManifestError::Malformed {
            path: String::new(),
            message: "unterminated quoted string".to_owned(),
        });
    }
    Ok(line)
}

fn structured_value_complete(value: &str) -> Result<bool, PackageManifestError> {
    let mut square = 0_i32;
    let mut curly = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        if character == '\'' || character == '"' {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if quote.is_some() {
            continue;
        }
        match character {
            '[' => square += 1,
            ']' => square -= 1,
            '{' => curly += 1,
            '}' => curly -= 1,
            _ => {}
        }
        if square < 0 || curly < 0 {
            return Err(PackageManifestError::Malformed {
                path: String::new(),
                message: "unbalanced structured value".to_owned(),
            });
        }
    }
    Ok(square == 0 && curly == 0 && quote.is_none())
}

fn split_top_level_once(input: &str, separator: char) -> Option<(&str, &str)> {
    let mut square = 0_i32;
    let mut curly = 0_i32;
    let mut round = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        if character == '\'' || character == '"' {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if quote.is_some() {
            continue;
        }
        match character {
            '[' => square += 1,
            ']' => square -= 1,
            '{' => curly += 1,
            '}' => curly -= 1,
            '(' => round += 1,
            ')' => round -= 1,
            _ => {}
        }
        if character == separator && square == 0 && curly == 0 && round == 0 {
            return Some((&input[..index], &input[index + character.len_utf8()..]));
        }
    }
    None
}

fn split_top_level(input: &str, separator: char) -> Vec<&str> {
    let mut result = Vec::new();
    let mut rest = input;
    while let Some((left, right)) = split_top_level_once(rest, separator) {
        result.push(left);
        rest = right;
    }
    result.push(rest);
    result
}

fn unquote_scalar(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn toml_string(path: &str, entry: &TomlEntry) -> Result<String, PackageManifestError> {
    let trimmed = entry.value.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') {
        return serde_json::from_str::<String>(trimmed)
            .map_err(|error| malformed(path, format!("line {}: {error}", entry.line)));
    }
    if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        return Ok(trimmed[1..trimmed.len() - 1].to_owned());
    }
    Err(malformed(
        path,
        format!("`{}` at line {} must be a string", entry.key, entry.line),
    ))
}

fn toml_array(path: &str, entry: &TomlEntry) -> Result<Vec<String>, PackageManifestError> {
    let trimmed = entry.value.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(malformed(
            path,
            format!("`{}` at line {} must be an array", entry.key, entry.line),
        ));
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let mut values = Vec::new();
    for item in split_top_level(inner, ',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let fake = TomlEntry {
            section: entry.section.clone(),
            key: entry.key.clone(),
            value: item.to_owned(),
            line: entry.line,
        };
        values.push(toml_string(path, &fake)?);
    }
    Ok(values)
}

fn inline_table(
    path: &str,
    entry: &TomlEntry,
) -> Result<BTreeMap<String, String>, PackageManifestError> {
    let trimmed = entry.value.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return Err(malformed(
            path,
            format!(
                "`{}` at line {} must be an inline table",
                entry.key, entry.line
            ),
        ));
    }
    let mut values = BTreeMap::new();
    for item in split_top_level(&trimmed[1..trimmed.len() - 1], ',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (key, value) = split_top_level_once(item, '=').ok_or_else(|| {
            malformed(path, format!("invalid inline table at line {}", entry.line))
        })?;
        values.insert(unquote_scalar(key), value.trim().to_owned());
    }
    Ok(values)
}

fn entry<'a>(entries: &'a [TomlEntry], section: &str, key: &str) -> Option<&'a TomlEntry> {
    entries
        .iter()
        .find(|item| item.section == section && item.key == key)
}

fn parse_pyproject(path: &str, content: &str) -> Result<PackageManifest, PackageManifestError> {
    let entries = parse_toml(path, content).map_err(|error| rehome_error(path, error))?;
    let mut result = PackageManifest::default();
    extract_python_package(path, content, &entries, &mut result)?;
    extract_pep621_dependencies(path, content, &entries, &mut result)?;
    extract_poetry_dependencies(path, content, &entries, &mut result)?;
    Ok(result)
}

fn extract_python_package(
    path: &str,
    content: &str,
    entries: &[TomlEntry],
    result: &mut PackageManifest,
) -> Result<(), PackageManifestError> {
    let project_name = entry(entries, "project", "name")
        .map(|item| toml_string(path, item))
        .transpose()?;
    let poetry_name = entry(entries, "tool.poetry", "name")
        .map(|item| toml_string(path, item))
        .transpose()?;
    if let Some(name) = project_name.as_ref().or(poetry_name.as_ref()) {
        let section = if project_name.is_some() {
            "project"
        } else {
            "tool.poetry"
        };
        let name_entry = entry(entries, section, "name")
            .ok_or_else(|| malformed(path, "package name declaration disappeared"))?;
        let version = entry(entries, section, "version")
            .map(|item| toml_string(path, item))
            .transpose()?;
        result.packages.push(package(
            PackageEcosystem::Python,
            name.clone(),
            version,
            path,
            evidence_at(content, name_entry.line),
        ));
    }
    Ok(())
}

fn extract_pep621_dependencies(
    path: &str,
    content: &str,
    entries: &[TomlEntry],
    result: &mut PackageManifest,
) -> Result<(), PackageManifestError> {
    if let Some(dependencies) = entry(entries, "project", "dependencies") {
        for requirement in toml_array(path, dependencies)? {
            if let Some(parsed) = parse_python_requirement(&requirement) {
                result.dependencies.push(python_dependency(
                    path,
                    content,
                    parsed,
                    DependencyScope::Runtime,
                    false,
                    dependencies.line,
                    &requirement,
                ));
            }
        }
    }
    for item in entries {
        if let Some(extra) = item.section.strip_prefix("project.optional-dependencies") {
            let extra = extra.trim_start_matches('.');
            if extra.is_empty() {
                for requirement in toml_array(path, item)? {
                    if let Some(parsed) = parse_python_requirement(&requirement) {
                        result.dependencies.push(python_dependency(
                            path,
                            content,
                            parsed.with_condition(format!("extra = \"{}\"", item.key)),
                            DependencyScope::Optional,
                            true,
                            item.line,
                            &requirement,
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn extract_poetry_dependencies(
    path: &str,
    content: &str,
    entries: &[TomlEntry],
    result: &mut PackageManifest,
) -> Result<(), PackageManifestError> {
    for item in entries {
        let (scope, group_condition) = if item.section == "tool.poetry.dependencies" {
            (DependencyScope::Runtime, None)
        } else if item.section == "tool.poetry.dev-dependencies" {
            (DependencyScope::Dev, None)
        } else if let Some(group) = item
            .section
            .strip_prefix("tool.poetry.group.")
            .and_then(|tail| tail.strip_suffix(".dependencies"))
        {
            (
                if group.eq_ignore_ascii_case("test") {
                    DependencyScope::Test
                } else {
                    DependencyScope::Dev
                },
                Some(format!("poetry group = \"{group}\"")),
            )
        } else {
            continue;
        };
        if item.key == "python" || contains_dynamic(&item.key) {
            continue;
        }
        let (version, optional, table_condition) = parse_poetry_value(path, item)?;
        let condition = join_conditions(group_condition, table_condition);
        result.dependencies.push(dependency(DependencyInput {
            ecosystem: PackageEcosystem::Python,
            name: item.key.clone(),
            version,
            scope: if optional {
                DependencyScope::Optional
            } else {
                scope
            },
            optional,
            condition,
            path,
            evidence: evidence_at(content, item.line),
            confidence: EXACT_CONFIDENCE,
        }));
    }
    Ok(())
}

fn rehome_error(path: &str, error: PackageManifestError) -> PackageManifestError {
    match error {
        PackageManifestError::Malformed { message, .. } => malformed(path, message),
        other => other,
    }
}

fn parse_poetry_value(
    path: &str,
    item: &TomlEntry,
) -> Result<(Option<String>, bool, Option<String>), PackageManifestError> {
    if item.value.trim().starts_with(['"', '\'']) {
        return Ok((Some(toml_string(path, item)?), false, None));
    }
    let values = inline_table(path, item)?;
    let version = values.get("version").map(|value| unquote_scalar(value));
    let optional = values
        .get("optional")
        .is_some_and(|value| value.trim() == "true");
    let mut conditions = Vec::new();
    for key in ["markers", "python", "platform"] {
        if let Some(value) = values.get(key) {
            conditions.push(format!("{key} = {}", value.trim()));
        }
    }
    Ok((
        version,
        optional,
        (!conditions.is_empty()).then(|| conditions.join(" and ")),
    ))
}

#[derive(Debug)]
struct PythonRequirement {
    name: String,
    version: Option<String>,
    condition: Option<String>,
}

impl PythonRequirement {
    fn with_condition(mut self, condition: String) -> Self {
        self.condition = join_conditions(Some(condition), self.condition);
        self
    }
}

fn parse_python_requirement(input: &str) -> Option<PythonRequirement> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('-')
        || trimmed.contains("${")
        || trimmed.contains(" @ ")
        || trimmed.starts_with("git+")
    {
        return None;
    }
    let (declaration, marker) = trimmed
        .split_once(';')
        .map_or((trimmed, None), |(left, right)| {
            (left.trim(), Some(right.trim().to_owned()))
        });
    let name_end = declaration
        .char_indices()
        .find_map(|(index, character)| {
            (character.is_whitespace() || matches!(character, '<' | '>' | '=' | '!' | '~' | '@'))
                .then_some(index)
        })
        .unwrap_or(declaration.len());
    let name = declaration[..name_end].trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-[]".contains(character))
    {
        return None;
    }
    let version = declaration[name_end..].trim();
    Some(PythonRequirement {
        name: name.to_owned(),
        version: (!version.is_empty()).then(|| version.to_owned()),
        condition: marker.filter(|value| !value.is_empty()),
    })
}

fn python_dependency(
    path: &str,
    content: &str,
    parsed: PythonRequirement,
    scope: DependencyScope,
    optional: bool,
    start_line: usize,
    token: &str,
) -> PackageDependency {
    dependency(DependencyInput {
        ecosystem: PackageEcosystem::Python,
        name: parsed.name,
        version: parsed.version,
        scope,
        optional,
        condition: parsed.condition,
        path,
        evidence: evidence_for(content, start_line, token),
        confidence: EXACT_CONFIDENCE,
    })
}

fn parse_requirements(path: &str, content: &str) -> Result<PackageManifest, PackageManifestError> {
    reject_nul(path, content)?;
    let mut result = PackageManifest::default();
    for (index, line) in content.lines().enumerate() {
        let declaration = strip_requirement_comment(line).trim();
        if declaration.is_empty() || declaration.starts_with('-') {
            continue;
        }
        if declaration.ends_with('\\') {
            return Err(malformed(
                path,
                format!("line continuations are not supported at line {}", index + 1),
            ));
        }
        if let Some(parsed) = parse_python_requirement(declaration) {
            result.dependencies.push(python_dependency(
                path,
                content,
                parsed,
                DependencyScope::Runtime,
                false,
                index + 1,
                declaration,
            ));
        }
    }
    Ok(result)
}

fn strip_requirement_comment(line: &str) -> &str {
    line.find(" #")
        .map_or(line, |index| &line[..index])
        .trim_end()
}

fn parse_cargo_manifest(
    path: &str,
    content: &str,
) -> Result<PackageManifest, PackageManifestError> {
    let entries = parse_toml(path, content).map_err(|error| rehome_error(path, error))?;
    let mut result = PackageManifest::default();
    if let Some(name_entry) = entry(&entries, "package", "name") {
        let name = toml_string(path, name_entry)?;
        let version = entry(&entries, "package", "version")
            .map(|item| toml_string(path, item))
            .transpose()?;
        result.packages.push(package(
            PackageEcosystem::Cargo,
            name,
            version,
            path,
            evidence_at(content, name_entry.line),
        ));
    }
    if let Some(members) = entry(&entries, "workspace", "members") {
        for member in toml_array(path, members)? {
            result.workspace_members.push(value_fact(
                path,
                content,
                member.clone(),
                members.line,
                &member,
            ));
        }
    }

    let mut feature_dependencies: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for feature in entries.iter().filter(|item| item.section == "features") {
        result.features.push(value_fact(
            path,
            content,
            feature.key.clone(),
            feature.line,
            &feature.key,
        ));
        for member in toml_array(path, feature)? {
            let dependency_name = member
                .strip_prefix("dep:")
                .or_else(|| (!member.contains('/')).then_some(member.as_str()));
            if let Some(name) = dependency_name {
                feature_dependencies
                    .entry(name.to_owned())
                    .or_default()
                    .insert(feature.key.clone());
            }
        }
    }

    for item in &entries {
        let Some((scope, target)) = cargo_dependency_section(&item.section) else {
            continue;
        };
        let (name, version, optional, declared_features) = parse_cargo_dependency(path, item)?;
        let features = feature_dependencies.get(&item.key);
        let feature_condition = features.map(|names| {
            let joined = names.iter().cloned().collect::<Vec<_>>().join("|");
            format!("feature = \"{joined}\"")
        });
        let implicit_feature =
            (optional && features.is_none()).then(|| format!("feature = \"{}\"", item.key));
        let dependency_features = (!declared_features.is_empty())
            .then(|| format!("dependency features = \"{}\"", declared_features.join("|")));
        result.dependencies.push(dependency(DependencyInput {
            ecosystem: PackageEcosystem::Cargo,
            name,
            version,
            scope,
            optional,
            condition: join_conditions(
                join_conditions(target, feature_condition.or(implicit_feature)),
                dependency_features,
            ),
            path,
            evidence: evidence_at(content, item.line),
            confidence: EXACT_CONFIDENCE,
        }));
    }
    Ok(result)
}

fn cargo_dependency_section(section: &str) -> Option<(DependencyScope, Option<String>)> {
    let (prefix, scope) = if let Some(prefix) = section.strip_suffix(".dev-dependencies") {
        (prefix, DependencyScope::Dev)
    } else if let Some(prefix) = section.strip_suffix(".build-dependencies") {
        (prefix, DependencyScope::Build)
    } else if let Some(prefix) = section.strip_suffix(".dependencies") {
        (prefix, DependencyScope::Runtime)
    } else {
        return match section {
            "dependencies" => Some((DependencyScope::Runtime, None)),
            "dev-dependencies" => Some((DependencyScope::Dev, None)),
            "build-dependencies" => Some((DependencyScope::Build, None)),
            _ => None,
        };
    };
    let target = prefix
        .strip_prefix("target.")
        .map(|value| format!("target = {}", value.trim_matches(['\'', '"'])));
    target.map(|condition| (scope, Some(condition)))
}

fn parse_cargo_dependency(
    path: &str,
    item: &TomlEntry,
) -> Result<(String, Option<String>, bool, Vec<String>), PackageManifestError> {
    if item.value.trim().starts_with(['"', '\'']) {
        return Ok((
            item.key.clone(),
            Some(toml_string(path, item)?),
            false,
            Vec::new(),
        ));
    }
    let values = inline_table(path, item)?;
    let name = values
        .get("package")
        .map_or_else(|| item.key.clone(), |value| unquote_scalar(value));
    let version = values.get("version").map(|value| unquote_scalar(value));
    let optional = values
        .get("optional")
        .is_some_and(|value| value.trim() == "true");
    let mut features = values
        .get("features")
        .map(|value| {
            toml_array(
                path,
                &TomlEntry {
                    section: item.section.clone(),
                    key: "features".to_owned(),
                    value: value.clone(),
                    line: item.line,
                },
            )
        })
        .transpose()?
        .unwrap_or_default();
    features.sort();
    features.dedup();
    Ok((name, version, optional, features))
}

fn parse_go_mod(path: &str, content: &str) -> Result<PackageManifest, PackageManifestError> {
    reject_nul(path, content)?;
    let mut result = PackageManifest::default();
    let mut require_block = false;
    let mut saw_module = false;
    for (index, line) in content.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if let Some(module) = trimmed.strip_prefix("module ") {
            let module = module.trim();
            if module.is_empty() || saw_module {
                return Err(malformed(
                    path,
                    format!("invalid module at line {line_number}"),
                ));
            }
            saw_module = true;
            result.packages.push(package(
                PackageEcosystem::Go,
                module.to_owned(),
                None,
                path,
                evidence_at(content, line_number),
            ));
            continue;
        }
        if trimmed == "require (" {
            if require_block {
                return Err(malformed(
                    path,
                    format!("nested require block at line {line_number}"),
                ));
            }
            require_block = true;
            continue;
        }
        if trimmed == ")" && require_block {
            require_block = false;
            continue;
        }
        let declaration = if require_block {
            trimmed
        } else {
            trimmed.strip_prefix("require ").unwrap_or_default()
        };
        if declaration.is_empty() || declaration.starts_with("//") {
            continue;
        }
        let indirect = declaration.contains("// indirect");
        let declaration = declaration.split("//").next().unwrap_or_default().trim();
        let mut fields = declaration.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(version) = fields.next() else {
            return Err(malformed(
                path,
                format!("requirement lacks a version at line {line_number}"),
            ));
        };
        if fields.next().is_some() {
            return Err(malformed(
                path,
                format!("invalid requirement at line {line_number}"),
            ));
        }
        result.dependencies.push(dependency(DependencyInput {
            ecosystem: PackageEcosystem::Go,
            name: name.to_owned(),
            version: Some(version.to_owned()),
            scope: DependencyScope::Runtime,
            optional: indirect,
            condition: indirect.then(|| "indirect".to_owned()),
            path,
            evidence: evidence_at(content, line_number),
            confidence: EXACT_CONFIDENCE,
        }));
    }
    if require_block {
        return Err(malformed(path, "unterminated require block"));
    }
    if !saw_module {
        return Err(malformed(path, "missing `module` declaration"));
    }
    Ok(result)
}

fn parse_go_work(path: &str, content: &str) -> Result<PackageManifest, PackageManifestError> {
    reject_nul(path, content)?;
    let mut result = PackageManifest::default();
    let mut use_block = false;
    for (index, line) in content.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.split("//").next().unwrap_or_default().trim();
        if trimmed == "use (" {
            if use_block {
                return Err(malformed(
                    path,
                    format!("nested use block at line {line_number}"),
                ));
            }
            use_block = true;
            continue;
        }
        if trimmed == ")" && use_block {
            use_block = false;
            continue;
        }
        let member = if use_block {
            trimmed
        } else {
            trimmed.strip_prefix("use ").unwrap_or_default()
        };
        if member.is_empty() || member.starts_with("go ") || member.starts_with("toolchain ") {
            continue;
        }
        if member.split_whitespace().count() != 1 {
            return Err(malformed(
                path,
                format!("invalid use declaration at line {line_number}"),
            ));
        }
        result.workspace_members.push(value_fact(
            path,
            content,
            member.to_owned(),
            line_number,
            member,
        ));
    }
    if use_block {
        return Err(malformed(path, "unterminated use block"));
    }
    Ok(result)
}

#[derive(Debug, Default)]
struct XmlNode {
    name: String,
    attributes: BTreeMap<String, String>,
    text: String,
    line: usize,
    children: Vec<usize>,
}

#[derive(Debug)]
struct XmlDocument {
    nodes: Vec<XmlNode>,
}

impl XmlDocument {
    fn node(&self, index: usize) -> &XmlNode {
        &self.nodes[index]
    }

    fn children<'a>(&'a self, node: &'a XmlNode) -> impl Iterator<Item = &'a XmlNode> + 'a {
        node.children.iter().map(|index| self.node(*index))
    }
}

fn parse_xml(
    path: &str,
    content: &str,
    tracker: &mut ExtractionTracker,
) -> Result<XmlDocument, PackageManifestError> {
    reject_nul(path, content)?;
    let mut nodes = vec![XmlNode {
        name: "#document".to_owned(),
        line: 1,
        ..XmlNode::default()
    }];
    let mut stack = vec![0_usize];
    let mut cursor = 0;
    while cursor < content.len() {
        tracker.charge_work(1)?;
        let Some(relative_open) = content[cursor..].find('<') else {
            append_xml_text(&mut nodes, &stack, &content[cursor..], tracker)?;
            break;
        };
        let open = cursor + relative_open;
        append_xml_text(&mut nodes, &stack, &content[cursor..open], tracker)?;
        if content[open..].starts_with("<!--") {
            let end = content[open + 4..]
                .find("-->")
                .map(|index| open + 4 + index + 3)
                .ok_or_else(|| malformed(path, "unterminated XML comment"))?;
            cursor = end;
            continue;
        }
        if content[open..].starts_with("<![CDATA[") {
            let start = open + 9;
            let end = content[start..]
                .find("]]>")
                .map(|index| start + index)
                .ok_or_else(|| malformed(path, "unterminated CDATA section"))?;
            append_xml_text(&mut nodes, &stack, &content[start..end], tracker)?;
            cursor = end + 3;
            continue;
        }
        let close = find_xml_tag_end(content, open + 1)
            .ok_or_else(|| malformed(path, "unterminated XML tag"))?;
        let raw = content[open + 1..close].trim();
        cursor = close + 1;
        if raw.starts_with('?') || raw.starts_with('!') {
            continue;
        }
        if let Some(name) = raw.strip_prefix('/') {
            let name = local_xml_name(name.trim());
            if stack.len() <= 1 {
                return Err(malformed(path, format!("unexpected closing tag `{name}`")));
            }
            let node_index = stack
                .pop()
                .ok_or_else(|| malformed(path, "XML parser stack underflow"))?;
            let node = &nodes[node_index];
            if node.name != name {
                return Err(malformed(
                    path,
                    format!("closing tag `{name}` does not match `{}`", node.name),
                ));
            }
            continue;
        }
        let self_closing = raw.ends_with('/');
        let declaration = raw.trim_end_matches('/').trim();
        let (name, attributes) = parse_xml_opening(path, declaration)?;
        let depth = u64::try_from(stack.len()).unwrap_or(u64::MAX);
        tracker.check_structural_depth(depth)?;
        tracker.charge_identifier(&name)?;
        tracker.charge_observation(1)?;
        let node_index = nodes.len();
        nodes.push(XmlNode {
            name,
            attributes,
            text: String::new(),
            line: content[..open]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1,
            children: Vec::new(),
        });
        let parent = *stack
            .last()
            .ok_or_else(|| malformed(path, "XML parser stack underflow"))?;
        nodes[parent].children.push(node_index);
        if !self_closing {
            stack.push(node_index);
        }
    }
    if stack.len() != 1 {
        let name = stack.last().map_or("", |index| nodes[*index].name.as_str());
        return Err(malformed(path, format!("unclosed XML tag `{name}`")));
    }
    Ok(XmlDocument { nodes })
}

fn append_xml_text(
    nodes: &mut [XmlNode],
    stack: &[usize],
    text: &str,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    if !text.is_empty() {
        tracker.charge_string(text)?;
    }
    if let Some(index) = stack.last()
        && let Some(node) = nodes.get_mut(*index)
    {
        node.text.push_str(text);
    }
    Ok(())
}

fn find_xml_tag_end(content: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (relative, character) in content[start..].char_indices() {
        if character == '\'' || character == '"' {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
        } else if character == '>' && quote.is_none() {
            return Some(start + relative);
        }
    }
    None
}

fn parse_xml_opening(
    path: &str,
    declaration: &str,
) -> Result<(String, BTreeMap<String, String>), PackageManifestError> {
    let name_end = declaration
        .find(char::is_whitespace)
        .unwrap_or(declaration.len());
    let name = local_xml_name(&declaration[..name_end]);
    if name.is_empty() {
        return Err(malformed(path, "empty XML element name"));
    }
    let mut attributes = BTreeMap::new();
    let mut rest = declaration[name_end..].trim();
    while !rest.is_empty() {
        let Some(equal) = rest.find('=') else {
            return Err(malformed(path, "XML attribute lacks `=`"));
        };
        let key = rest[..equal].trim();
        rest = rest[equal + 1..].trim_start();
        let quote = rest
            .chars()
            .next()
            .filter(|character| *character == '\'' || *character == '"')
            .ok_or_else(|| malformed(path, "XML attribute value must be quoted"))?;
        let tail = &rest[quote.len_utf8()..];
        let end = tail
            .find(quote)
            .ok_or_else(|| malformed(path, "unterminated XML attribute"))?;
        attributes.insert(local_xml_name(key), decode_xml_entities(&tail[..end]));
        rest = tail[end + quote.len_utf8()..].trim_start();
    }
    Ok((name, attributes))
}

fn local_xml_name(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_owned()
}

fn decode_xml_entities(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn child<'a>(document: &'a XmlDocument, node: &'a XmlNode, name: &str) -> Option<&'a XmlNode> {
    document.children(node).find(|item| item.name == name)
}

fn child_text(document: &XmlDocument, node: &XmlNode, name: &str) -> Option<String> {
    child(document, node, name)
        .map(|item| decode_xml_entities(item.text.trim()))
        .filter(|value| !value.is_empty())
}

fn parse_maven(
    path: &str,
    content: &str,
    tracker: &mut ExtractionTracker,
) -> Result<PackageManifest, PackageManifestError> {
    let document = parse_xml(path, content, tracker)?;
    let project = document
        .children(document.node(0))
        .find(|node| node.name == "project")
        .ok_or_else(|| malformed(path, "missing `project` root element"))?;
    let mut result = PackageManifest::default();
    let group = child_text(&document, project, "groupId").or_else(|| {
        child(&document, project, "parent").and_then(|node| child_text(&document, node, "groupId"))
    });
    let artifact = child_text(&document, project, "artifactId");
    let version = child_text(&document, project, "version").or_else(|| {
        child(&document, project, "parent").and_then(|node| child_text(&document, node, "version"))
    });
    if let (Some(group), Some(artifact)) = (group, artifact)
        && !contains_dynamic(&group)
        && !contains_dynamic(&artifact)
    {
        let evidence = child(&document, project, "artifactId").map_or_else(
            || evidence_at(content, project.line),
            |node| evidence_at(content, node.line),
        );
        result.packages.push(package(
            PackageEcosystem::Maven,
            format!("{group}:{artifact}"),
            version.filter(|value| !contains_dynamic(value)),
            path,
            evidence,
        ));
    }
    if let Some(dependencies) = child(&document, project, "dependencies") {
        collect_maven_dependencies(path, content, &document, dependencies, None, &mut result);
    }
    if let Some(profiles) = child(&document, project, "profiles") {
        for profile in document
            .children(profiles)
            .filter(|node| node.name == "profile")
        {
            let profile_id = child_text(&document, profile, "id");
            if let Some(dependencies) = child(&document, profile, "dependencies") {
                collect_maven_dependencies(
                    path,
                    content,
                    &document,
                    dependencies,
                    profile_id
                        .as_deref()
                        .map(|id| format!("profile = \"{id}\""))
                        .as_deref(),
                    &mut result,
                );
            }
        }
    }
    Ok(result)
}

fn collect_maven_dependencies(
    path: &str,
    content: &str,
    document: &XmlDocument,
    dependencies: &XmlNode,
    profile: Option<&str>,
    output: &mut PackageManifest,
) {
    for item in document
        .children(dependencies)
        .filter(|node| node.name == "dependency")
    {
        let (Some(group), Some(artifact)) = (
            child_text(document, item, "groupId"),
            child_text(document, item, "artifactId"),
        ) else {
            continue;
        };
        if contains_dynamic(&group) || contains_dynamic(&artifact) {
            continue;
        }
        let version =
            child_text(document, item, "version").filter(|value| !contains_dynamic(value));
        let declared_scope =
            child_text(document, item, "scope").unwrap_or_else(|| "compile".to_owned());
        let scope = match declared_scope.as_str() {
            "test" => DependencyScope::Test,
            "provided" | "system" => DependencyScope::Build,
            _ => DependencyScope::Runtime,
        };
        let optional = child_text(document, item, "optional").is_some_and(|value| value == "true");
        let type_condition = child_text(document, item, "type")
            .filter(|value| value != "jar")
            .map(|value| format!("type = \"{value}\""));
        output.dependencies.push(dependency(DependencyInput {
            ecosystem: PackageEcosystem::Maven,
            name: format!("{group}:{artifact}"),
            version,
            scope: if optional {
                DependencyScope::Optional
            } else {
                scope
            },
            optional,
            condition: join_conditions(profile.map(str::to_owned), type_condition),
            path,
            evidence: evidence_at(content, item.line),
            confidence: EXACT_CONFIDENCE,
        }));
    }
}

fn parse_gradle(path: &str, content: &str) -> Result<PackageManifest, PackageManifestError> {
    reject_nul(path, content)?;
    validate_gradle_balance(path, content)?;
    let mut result = PackageManifest::default();
    for (index, line) in content.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }
        let Some((configuration, argument)) = parse_gradle_declaration(trimmed) else {
            continue;
        };
        let Some((group, artifact, version)) = parse_gradle_coordinate(argument) else {
            continue;
        };
        let (scope, optional) = gradle_scope(configuration);
        result.dependencies.push(dependency(DependencyInput {
            ecosystem: PackageEcosystem::Gradle,
            name: format!("{group}:{artifact}"),
            version,
            scope,
            optional,
            condition: Some(format!("configuration = \"{configuration}\"")),
            path,
            evidence: evidence_at(content, line_number),
            confidence: STATIC_TEXT_CONFIDENCE,
        }));
    }
    Ok(result)
}

fn validate_gradle_balance(path: &str, content: &str) -> Result<(), PackageManifestError> {
    let mut curly = 0_i32;
    let mut round = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    for character in content.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if character == '\'' || character == '"' {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if quote.is_some() {
            continue;
        }
        match character {
            '{' => curly += 1,
            '}' => curly -= 1,
            '(' => round += 1,
            ')' => round -= 1,
            _ => {}
        }
        if curly < 0 || round < 0 {
            return Err(malformed(path, "unbalanced Gradle delimiters"));
        }
    }
    if curly != 0 || round != 0 || quote.is_some() {
        return Err(malformed(path, "unbalanced Gradle delimiters or quotes"));
    }
    Ok(())
}

fn parse_gradle_declaration(line: &str) -> Option<(&str, &str)> {
    let configuration_end = line
        .char_indices()
        .find_map(|(index, character)| {
            (character.is_whitespace() || character == '(').then_some(index)
        })
        .unwrap_or(line.len());
    let configuration = &line[..configuration_end];
    if !matches!(
        configuration,
        "api"
            | "implementation"
            | "runtimeOnly"
            | "compileOnly"
            | "testImplementation"
            | "testRuntimeOnly"
            | "testCompileOnly"
            | "developmentOnly"
            | "annotationProcessor"
            | "kapt"
            | "classpath"
    ) {
        return None;
    }
    let rest = line[configuration_end..].trim();
    let argument = if rest.starts_with('(') && rest.ends_with(')') {
        rest[1..rest.len() - 1].trim()
    } else {
        rest
    };
    Some((configuration, argument))
}

fn parse_gradle_coordinate(argument: &str) -> Option<(&str, &str, Option<String>)> {
    let literal = argument
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            argument
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })?;
    if contains_dynamic(literal) {
        return None;
    }
    let mut parts = literal.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next().map(str::to_owned);
    if group.is_empty() || artifact.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((group, artifact, version))
}

fn gradle_scope(configuration: &str) -> (DependencyScope, bool) {
    if configuration.starts_with("test") {
        (DependencyScope::Test, false)
    } else if matches!(configuration, "annotationProcessor" | "kapt" | "classpath") {
        (DependencyScope::Build, false)
    } else if matches!(configuration, "compileOnly" | "developmentOnly") {
        (DependencyScope::Dev, false)
    } else {
        (DependencyScope::Runtime, false)
    }
}

fn parse_packages_config(
    path: &str,
    content: &str,
    tracker: &mut ExtractionTracker,
) -> Result<PackageManifest, PackageManifestError> {
    let document = parse_xml(path, content, tracker)?;
    let packages = document
        .children(document.node(0))
        .find(|node| node.name == "packages")
        .ok_or_else(|| malformed(path, "missing `packages` root element"))?;
    let mut result = PackageManifest::default();
    for item in document
        .children(packages)
        .filter(|node| node.name == "package")
    {
        let Some(name) = item.attributes.get("id") else {
            return Err(malformed(path, "`package` requires an `id` attribute"));
        };
        let version = item.attributes.get("version").cloned();
        let development = item
            .attributes
            .get("developmentDependency")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"));
        result.dependencies.push(dependency(DependencyInput {
            ecosystem: PackageEcosystem::NuGet,
            name: name.clone(),
            version,
            scope: if development {
                DependencyScope::Dev
            } else {
                DependencyScope::Runtime
            },
            optional: false,
            condition: item
                .attributes
                .get("targetFramework")
                .map(|value| format!("targetFramework = \"{value}\"")),
            path,
            evidence: evidence_at(content, item.line),
            confidence: EXACT_CONFIDENCE,
        }));
    }
    Ok(result)
}

fn parse_csproj(
    path: &str,
    content: &str,
    tracker: &mut ExtractionTracker,
) -> Result<PackageManifest, PackageManifestError> {
    let document = parse_xml(path, content, tracker)?;
    let project = document
        .children(document.node(0))
        .find(|node| node.name == "Project")
        .ok_or_else(|| malformed(path, "missing `Project` root element"))?;
    let mut result = PackageManifest::default();
    if let Some(identity) = csproj_package_identity(path, &document, project)? {
        result.packages.push(package(
            PackageEcosystem::NuGet,
            identity.name,
            identity.version,
            path,
            evidence_at(content, identity.name_node.line),
        ));
    }
    for group in document
        .children(project)
        .filter(|node| node.name == "ItemGroup")
    {
        let group_condition = group.attributes.get("Condition").cloned();
        for reference in document
            .children(group)
            .filter(|node| node.name == "PackageReference")
        {
            let Some(name) = reference
                .attributes
                .get("Include")
                .or_else(|| reference.attributes.get("Update"))
            else {
                return Err(malformed(
                    path,
                    "`PackageReference` requires `Include` or `Update`",
                ));
            };
            if contains_dynamic(name) {
                continue;
            }
            let version = reference
                .attributes
                .get("Version")
                .cloned()
                .or_else(|| child_text(&document, reference, "Version"))
                .filter(|value| !contains_dynamic(value));
            let private_assets = reference
                .attributes
                .get("PrivateAssets")
                .cloned()
                .or_else(|| child_text(&document, reference, "PrivateAssets"));
            let reference_condition = reference.attributes.get("Condition").cloned();
            result.dependencies.push(dependency(DependencyInput {
                ecosystem: PackageEcosystem::NuGet,
                name: name.clone(),
                version,
                scope: DependencyScope::Runtime,
                optional: false,
                condition: join_conditions(group_condition.clone(), reference_condition),
                path,
                evidence: evidence_at(content, reference.line),
                confidence: if private_assets.as_deref() == Some("all") {
                    STATIC_TEXT_CONFIDENCE
                } else {
                    EXACT_CONFIDENCE
                },
            }));
        }
    }
    Ok(result)
}

struct CsprojPackageIdentity<'a> {
    name_node: &'a XmlNode,
    name: String,
    version: Option<String>,
}

fn csproj_package_identity<'a>(
    path: &str,
    document: &'a XmlDocument,
    project: &'a XmlNode,
) -> Result<Option<CsprojPackageIdentity<'a>>, PackageManifestError> {
    let mut identities = Vec::new();
    for group in document
        .children(project)
        .filter(|node| node.name == "PropertyGroup")
    {
        let Some(name_node) = child(document, group, "PackageId") else {
            continue;
        };
        let name = decode_xml_entities(name_node.text.trim());
        if name.is_empty() || contains_dynamic(&name) {
            continue;
        }
        let version = ["PackageVersion", "Version", "VersionPrefix"]
            .into_iter()
            .find_map(|field| child_text(document, group, field))
            .filter(|value| !contains_dynamic(value));
        identities.push(CsprojPackageIdentity {
            name_node,
            name,
            version,
        });
    }
    identities.sort_by(|left, right| left.name.cmp(&right.name));
    identities.dedup_by(|left, right| left.name == right.name && left.version == right.version);
    if identities.len() > 1 {
        return Err(malformed(
            path,
            "multiple distinct static `PackageId` declarations are ambiguous",
        ));
    }
    Ok(identities.pop())
}

fn contains_dynamic(value: &str) -> bool {
    value.contains("${")
        || value.contains("$(")
        || value.contains('$')
        || value.contains("#{")
        || value.contains("{{")
}

fn join_conditions(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => Some(format!("{left} and {right}")),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn reject_nul(path: &str, content: &str) -> Result<(), PackageManifestError> {
    if content.contains('\0') {
        return Err(malformed(path, "input contains a NUL byte"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{ExtractionClock, ExtractionResource};

    #[derive(Debug)]
    struct FixedClock(Duration);

    impl ExtractionClock for FixedClock {
        fn elapsed(&self) -> Duration {
            self.0
        }
    }

    fn extract(path: &str, source: &str) -> PackageManifest {
        extract_package_manifest(path, source).expect("fixture should parse")
    }

    fn nested_maven(depth: usize) -> String {
        let mut source = String::from("<project>");
        source.push_str(&"<level>".repeat(depth.saturating_sub(1)));
        source.push_str(&"</level>".repeat(depth.saturating_sub(1)));
        source.push_str("</project>");
        source
    }

    #[test]
    fn xml_depth_should_accept_64_and_reject_65_with_an_iterative_stack() {
        let budgets = ExtractionBudgets {
            max_structural_depth_per_artifact: 64,
            ..ExtractionBudgets::default()
        };
        let mut exact = ExtractionTracker::new("pom.xml", "packages", &budgets);
        let mut above = ExtractionTracker::new("pom.xml", "packages", &budgets);

        assert!(
            extract_package_manifest_with_tracker("pom.xml", &nested_maven(64), &mut exact).is_ok()
        );
        assert!(matches!(
            extract_package_manifest_with_tracker("pom.xml", &nested_maven(65), &mut above),
            Err(PackageManifestError::LimitExceeded(error))
                if error.resource == ExtractionResource::StructuralDepth
                    && error.observed == 65
                    && error.maximum == 64
        ));
    }

    #[test]
    fn short_package_extraction_should_check_its_final_deadline() {
        let budgets = ExtractionBudgets {
            max_structured_wall_time_ms_per_artifact: 1,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::with_clock(
            "Cargo.toml",
            "packages",
            &budgets,
            Box::new(FixedClock(Duration::from_millis(2))),
        );

        assert!(matches!(
            extract_package_manifest_with_tracker("Cargo.toml", "", &mut tracker),
            Err(PackageManifestError::LimitExceeded(error))
                if error.resource == ExtractionResource::StructuredWallTimeMs
        ));
    }

    #[test]
    fn package_json_preserves_scopes_workspaces_exports_and_lines() {
        let source = r#"{
  "name": "@acme/web",
  "version": "1.2.3",
  "dependencies": {"zod": "^3.0.0"},
  "devDependencies": {"vitest": "~2.0"},
  "peerDependencies": {"react": ">=18"},
  "peerDependenciesMeta": {"react": {"optional": true}},
  "optionalDependencies": {"fsevents": "2.3.3"},
  "workspaces": ["apps/*", "packages/*"],
  "exports": {".": "./index.js", "./cli": "./cli.js"}
}"#;
        let result = extract("web/package.json", source);

        assert_eq!(
            (
                result.packages[0].name.as_str(),
                result
                    .dependencies
                    .iter()
                    .map(|item| (item.name.as_str(), item.scope, item.optional))
                    .collect::<Vec<_>>(),
                result
                    .workspace_members
                    .iter()
                    .map(|item| item.value.as_str())
                    .collect::<Vec<_>>(),
                result
                    .exports
                    .iter()
                    .map(|item| item.value.as_str())
                    .collect::<Vec<_>>(),
                result.dependencies[0].evidence.line,
            ),
            (
                "@acme/web",
                vec![
                    ("fsevents", DependencyScope::Optional, true),
                    ("react", DependencyScope::Peer, true),
                    ("vitest", DependencyScope::Dev, false),
                    ("zod", DependencyScope::Runtime, false),
                ],
                vec!["apps/*", "packages/*"],
                vec![".", "./cli"],
                8,
            )
        );
    }

    #[test]
    fn npm_lockfiles_emit_presence_without_transitive_dependencies() {
        let source = r#"{"name":"app","lockfileVersion":3,"packages":{"":{"name":"app"}}}"#;
        let result = extract("package-lock.json", source);

        assert_eq!(
            (
                result.lockfiles[0].package_manager.as_str(),
                result.lockfiles[0].format_version.as_deref(),
                result.dependencies.len(),
            ),
            ("npm", Some("3"), 0)
        );
    }

    #[test]
    fn cargo_lockfile_emits_presence_without_transitive_dependencies() {
        let source = "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"api\"\nversion = \"1.0.0\"\n";
        let result = extract("Cargo.lock", source);

        assert_eq!(
            (
                result.lockfiles[0].ecosystem,
                result.lockfiles[0].package_manager.as_str(),
                result.lockfiles[0].format_version.as_deref(),
                result.dependencies.len(),
            ),
            (PackageEcosystem::Cargo, "cargo", Some("4"), 0)
        );
    }

    #[test]
    fn pnpm_and_yarn_lockfiles_preserve_format_metadata() {
        let pnpm = extract("pnpm-lock.yaml", "lockfileVersion: '9.0'\nimporters: {}\n");
        let yarn = extract("yarn.lock", "__metadata:\n  version: 8\n  cacheKey: 10c0\n");

        assert_eq!(
            (
                pnpm.lockfiles[0].format_version.as_deref(),
                yarn.lockfiles[0].format_version.as_deref(),
            ),
            (Some("9.0"), Some("8"))
        );
    }

    #[test]
    fn pyproject_extracts_pep621_poetry_groups_markers_and_optional_flags() {
        let source = r#"[project]
name = "service"
version = "2.0.0"
dependencies = [
  "httpx>=0.27; python_version >= '3.11'",
]
[project.optional-dependencies]
docs = ["sphinx~=7.0"]
[tool.poetry.dependencies]
orjson = { version = "^3.10", optional = true, markers = "platform_python_implementation == 'CPython'" }
[tool.poetry.group.test.dependencies]
pytest = "^8.0"
"#;
        let result = extract("pyproject.toml", source);

        assert_eq!(
            result
                .dependencies
                .iter()
                .map(|item| (
                    item.name.as_str(),
                    item.scope,
                    item.optional,
                    item.condition.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "httpx",
                    DependencyScope::Runtime,
                    false,
                    Some("python_version >= '3.11'")
                ),
                (
                    "orjson",
                    DependencyScope::Optional,
                    true,
                    Some("markers = \"platform_python_implementation == 'CPython'\"")
                ),
                (
                    "pytest",
                    DependencyScope::Test,
                    false,
                    Some("poetry group = \"test\"")
                ),
                (
                    "sphinx",
                    DependencyScope::Optional,
                    true,
                    Some("extra = \"docs\"")
                ),
            ]
        );
    }

    #[test]
    fn requirements_preserve_markers_and_ignore_includes_and_dynamic_urls() {
        let source = "# pinned\nrequests>=2.32 ; python_version >= \"3.10\"\n-r base.txt\npkg @ git+https://example.invalid/pkg\n";
        let result = extract("config/requirements-dev.txt", source);

        assert_eq!(
            (
                result.dependencies.len(),
                result.dependencies[0].name.as_str(),
                result.dependencies[0].version_or_range.as_deref(),
                result.dependencies[0].condition.as_deref(),
                result.dependencies[0].evidence.line,
            ),
            (
                1,
                "requests",
                Some(">=2.32"),
                Some("python_version >= \"3.10\""),
                2,
            )
        );
    }

    #[test]
    fn cargo_extracts_scopes_targets_features_renames_and_workspace_members() {
        let source = r#"[package]
name = "engine"
version = "0.4.0"
[dependencies]
serde = "1"
wire = { package = "prost", version = "0.13", optional = true }
[dev-dependencies]
insta = "1"
[build-dependencies]
cc = "1"
[target.'cfg(unix)'.dependencies]
nix = "0.29"
[features]
grpc = ["dep:wire"]
[workspace]
members = ["api", "worker"]
"#;
        let result = extract("Cargo.toml", source);

        assert_eq!(
            (
                result
                    .dependencies
                    .iter()
                    .map(|item| (
                        item.name.as_str(),
                        item.scope,
                        item.optional,
                        item.condition.as_deref()
                    ))
                    .collect::<Vec<_>>(),
                result.features[0].value.as_str(),
                result
                    .workspace_members
                    .iter()
                    .map(|item| item.value.as_str())
                    .collect::<Vec<_>>(),
            ),
            (
                vec![
                    ("cc", DependencyScope::Build, false, None),
                    ("insta", DependencyScope::Dev, false, None),
                    (
                        "nix",
                        DependencyScope::Runtime,
                        false,
                        Some("target = cfg(unix)")
                    ),
                    (
                        "prost",
                        DependencyScope::Runtime,
                        true,
                        Some("feature = \"grpc\"")
                    ),
                    ("serde", DependencyScope::Runtime, false, None),
                ],
                "grpc",
                vec!["api", "worker"],
            )
        );
    }

    #[test]
    fn cargo_preserves_sqlx_backend_and_capability_features() {
        let source = r#"
[package]
name = "api"
version = "1.0.0"

[dependencies]
sqlx = { version = "0.8", features = ["postgres", "macros", "migrate", "postgres"] }
"#;
        let result = extract("Cargo.toml", source);
        let sqlx = result
            .dependencies
            .iter()
            .find(|dependency| dependency.name == "sqlx")
            .expect("SQLx dependency");

        assert_eq!(
            (
                sqlx.version_or_range.as_deref(),
                sqlx.scope,
                sqlx.condition.as_deref(),
            ),
            (
                Some("0.8"),
                DependencyScope::Runtime,
                Some("dependency features = \"macros|migrate|postgres\""),
            )
        );
    }

    #[test]
    fn cargo_preserves_mysql_async_transport_and_mapping_features() {
        let source = r#"
[package]
name = "api"
version = "1.0.0"

[dependencies]
mysql_async = { version = "0.37", default-features = false, features = ["minimal-rust", "rustls-tls", "ring", "derive"] }
"#;
        let result = extract("Cargo.toml", source);
        let mysql_async = result
            .dependencies
            .iter()
            .find(|dependency| dependency.name == "mysql_async")
            .expect("mysql_async dependency");

        assert_eq!(
            (
                mysql_async.version_or_range.as_deref(),
                mysql_async.scope,
                mysql_async.condition.as_deref(),
            ),
            (
                Some("0.37"),
                DependencyScope::Runtime,
                Some("dependency features = \"derive|minimal-rust|ring|rustls-tls\""),
            )
        );
    }

    #[test]
    fn go_mod_preserves_indirect_condition_and_go_work_members() {
        let module = extract(
            "go.mod",
            "module example.com/service\n\ngo 1.23\nrequire (\n example.com/a v1.2.0\n example.com/b v2.0.0 // indirect\n)\n",
        );
        let workspace = extract("go.work", "go 1.23\nuse (\n ./api\n ./worker\n)\n");

        assert_eq!(
            (
                module.packages[0].name.as_str(),
                module.dependencies[1].optional,
                module.dependencies[1].condition.as_deref(),
                workspace
                    .workspace_members
                    .iter()
                    .map(|item| item.value.as_str())
                    .collect::<Vec<_>>(),
            ),
            (
                "example.com/service",
                true,
                Some("indirect"),
                vec!["./api", "./worker"],
            )
        );
    }

    #[test]
    fn maven_extracts_static_coordinates_scopes_optional_and_profiles() {
        let source = r"<project>
  <parent><groupId>com.acme</groupId><version>1.0</version></parent>
  <artifactId>service</artifactId>
  <dependencies>
    <dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId><version>2.0.16</version></dependency>
    <dependency><groupId>junit</groupId><artifactId>junit</artifactId><scope>test</scope><optional>true</optional></dependency>
    <dependency><groupId>${dynamic.group}</groupId><artifactId>ignored</artifactId></dependency>
  </dependencies>
  <profiles><profile><id>native</id><dependencies>
    <dependency><groupId>org.graalvm</groupId><artifactId>native-image</artifactId><version>24</version></dependency>
  </dependencies></profile></profiles>
</project>";
        let result = extract("pom.xml", source);

        assert_eq!(
            (
                result.packages[0].name.as_str(),
                result
                    .dependencies
                    .iter()
                    .map(|item| (
                        item.name.as_str(),
                        item.scope,
                        item.optional,
                        item.condition.as_deref()
                    ))
                    .collect::<Vec<_>>(),
            ),
            (
                "com.acme:service",
                vec![
                    ("junit:junit", DependencyScope::Optional, true, None),
                    (
                        "org.graalvm:native-image",
                        DependencyScope::Runtime,
                        false,
                        Some("profile = \"native\"")
                    ),
                    ("org.slf4j:slf4j-api", DependencyScope::Runtime, false, None),
                ],
            )
        );
    }

    #[test]
    fn gradle_extracts_only_literal_coordinates_with_configuration_conditions() {
        let source = r#"dependencies {
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    testImplementation 'org.junit.jupiter:junit-jupiter:5.11.0'
    implementation(libs.jackson)
    annotationProcessor("org.example:processor:1.0")
}"#;
        let result = extract("app/build.gradle.kts", source);

        assert_eq!(
            result
                .dependencies
                .iter()
                .map(|item| (item.name.as_str(), item.scope, item.condition.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "com.squareup.okhttp3:okhttp",
                    DependencyScope::Runtime,
                    Some("configuration = \"implementation\"")
                ),
                (
                    "org.example:processor",
                    DependencyScope::Build,
                    Some("configuration = \"annotationProcessor\"")
                ),
                (
                    "org.junit.jupiter:junit-jupiter",
                    DependencyScope::Test,
                    Some("configuration = \"testImplementation\"")
                ),
            ]
        );
    }

    #[test]
    fn nuget_extracts_packages_config_and_sdk_package_references() {
        let packages_config = extract(
            "packages.config",
            r#"<packages>
  <package id="Newtonsoft.Json" version="13.0.3" targetFramework="net48" />
  <package id="NUnit" version="4.2.2" developmentDependency="true" />
</packages>"#,
        );
        let sdk = extract(
            "src/App.csproj",
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><PackageId>Acme.App</PackageId><Version>1.4.0</Version></PropertyGroup>
  <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <PackageReference Include="Serilog" Version="4.0.0" />
    <PackageReference Include="Dynamic" Version="$(DynamicVersion)" />
  </ItemGroup>
</Project>"#,
        );

        assert_eq!(
            (
                packages_config
                    .dependencies
                    .iter()
                    .map(|item| (item.name.as_str(), item.scope, item.condition.as_deref()))
                    .collect::<Vec<_>>(),
                sdk.dependencies[0].name.as_str(),
                sdk.dependencies[0].condition.as_deref(),
                sdk.dependencies[1].version_or_range.as_deref(),
                sdk.packages
                    .first()
                    .map(|item| (item.name.as_str(), item.version.as_deref())),
            ),
            (
                vec![
                    ("NUnit", DependencyScope::Dev, None),
                    (
                        "Newtonsoft.Json",
                        DependencyScope::Runtime,
                        Some("targetFramework = \"net48\"")
                    ),
                ],
                "Dynamic",
                Some("'$(TargetFramework)' == 'net8.0'"),
                Some("4.0.0"),
                Some(("Acme.App", Some("1.4.0"))),
            )
        );
    }

    #[test]
    fn malformed_structured_inputs_are_rejected() {
        let cases = [
            ("package.json", r#"{"dependencies": []}"#),
            ("pyproject.toml", "[project\nname = \"bad\""),
            ("Cargo.toml", "[dependencies]\nserde = { version = \"1\""),
            ("go.mod", "module example.com/a\nrequire (\na v1\n"),
            ("pom.xml", "<project><artifactId>x</project>"),
            ("build.gradle", "dependencies { implementation(\"a:b:1\")"),
            ("packages.config", "<packages><package /></packages>"),
            ("app.csproj", "<Project><ItemGroup></Project>"),
        ];

        for (path, source) in cases {
            assert!(
                matches!(
                    extract_package_manifest(path, source),
                    Err(PackageManifestError::Malformed { .. })
                ),
                "{path} should reject malformed input"
            );
        }
    }

    #[test]
    fn output_order_is_deterministic_across_declaration_order() {
        let first = extract(
            "package.json",
            r#"{"name":"x","dependencies":{"z":"1","a":"2"},"devDependencies":{"m":"3"}}"#,
        );
        let second = extract(
            "package.json",
            r#"{"devDependencies":{"m":"3"},"dependencies":{"a":"2","z":"1"},"name":"x"}"#,
        );

        assert_eq!(
            first
                .dependencies
                .iter()
                .map(|item| (&item.name, item.scope, &item.version_or_range))
                .collect::<Vec<_>>(),
            second
                .dependencies
                .iter()
                .map(|item| (&item.name, item.scope, &item.version_or_range))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn unsupported_and_non_relative_paths_are_rejected() {
        for path in [
            "/package.json",
            "../package.json",
            "README.md",
            "C:\\package.json",
        ] {
            assert!(
                matches!(
                    extract_package_manifest(path, "{}"),
                    Err(PackageManifestError::UnsupportedPath(_))
                ),
                "{path} should be unsupported"
            );
        }
    }
}
