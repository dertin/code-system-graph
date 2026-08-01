//! Shared repository discovery exclusions.

use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::ser::{Serialize, SerializeStruct, Serializer};
use thiserror::Error;

use crate::ConfigSource;

/// Version included in repository fingerprints when built-in exclusion behavior changes.
pub const IGNORE_POLICY_VERSION: u8 = 1;

/// Repository metadata and generated state that can never be re-enabled for discovery.
pub const PROTECTED_EXCLUDES: &[&str] = &[
    "**/.git/**",
    "**/.hg/**",
    "**/.svn/**",
    "**/.codegraph/**",
    "**/.code-system-graph/**",
];

/// Dependency, cache, environment, and build trees excluded unless explicitly re-enabled.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "**/.next/**",
    "**/.venv/**",
    "**/.mypy_cache/**",
    "**/.nox/**",
    "**/.pytest_cache/**",
    "**/.ruff_cache/**",
    "**/.tox/**",
    "**/venv/**",
    "**/env/**",
    "**/site-packages/**",
    "**/node_modules/**",
    "**/vendor/**",
    "**/target/**",
    "**/dist/**",
    "**/build/**",
    "**/__pycache__/**",
];

const PROTECTED_DIRECTORY_NAMES: &[&str] =
    &[".git", ".hg", ".svn", ".codegraph", ".code-system-graph"];

/// Invalid user-supplied discovery glob.
#[derive(Debug, Error)]
pub enum IgnorePatternError {
    /// An empty pattern has no deterministic discovery meaning.
    #[error("ignore pattern must not be empty")]
    Empty,
    /// Dot-only and separator-only patterns do not identify a repository path.
    #[error("ignore pattern `{0}` must contain a repository-relative path component")]
    MissingPathComponent(String),
    /// Patterns are always resolved relative to one checkout.
    #[error("ignore pattern `{0}` must be repository-relative")]
    Absolute(String),
    /// Parent traversal could escape the registered checkout.
    #[error("ignore pattern `{0}` must not contain a `..` component")]
    ParentTraversal(String),
    /// Portable patterns use `/` on every operating system.
    #[error("ignore pattern `{0}` must use `/` separators")]
    Backslash(String),
    /// Terminal control and bidirectional characters are unsafe in reported rules.
    #[error("ignore pattern contains unsafe control or bidirectional characters")]
    UnsafeCharacters,
    /// The public glob contract is intentionally limited to portable wildcard syntax.
    #[error(
        "ignore pattern `{0}` uses unsupported glob syntax; only `*`, `?`, and whole-component `**` are supported"
    )]
    UnsupportedSyntax(String),
    /// The glob expression is malformed.
    #[error("invalid ignore pattern `{pattern}`: {detail}")]
    InvalidGlob {
        /// Rejected pattern.
        pattern: String,
        /// Parser diagnostic.
        detail: String,
    },
    /// Generated state and version-control metadata cannot be re-enabled.
    #[error("includeDefaults pattern `{pattern}` targets protected directory `{directory}`")]
    ProtectedInclude {
        /// Rejected include pattern.
        pattern: String,
        /// Protected literal path component.
        directory: String,
    },
}

/// Effective exclusion policy for one registered repository.
#[derive(Debug, Clone)]
pub struct IgnorePolicy {
    configured_excludes: Vec<String>,
    configured_excludes_source: ConfigSource,
    include_defaults: Vec<String>,
    include_defaults_source: ConfigSource,
    protected_matcher: GlobSet,
    default_matcher: GlobSet,
    configured_matcher: GlobSet,
    include_matcher: GlobSet,
    include_prefixes: Vec<PathBuf>,
    include_can_match_anywhere: bool,
}

impl PartialEq for IgnorePolicy {
    fn eq(&self, other: &Self) -> bool {
        self.configured_excludes == other.configured_excludes
            && self.configured_excludes_source == other.configured_excludes_source
            && self.include_defaults == other.include_defaults
            && self.include_defaults_source == other.include_defaults_source
    }
}

impl Eq for IgnorePolicy {}

impl Serialize for IgnorePolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("IgnorePolicy", 4)?;
        state.serialize_field("configured_excludes", &self.configured_excludes)?;
        state.serialize_field(
            "configured_excludes_source",
            &self.configured_excludes_source,
        )?;
        state.serialize_field("include_defaults", &self.include_defaults)?;
        state.serialize_field("include_defaults_source", &self.include_defaults_source)?;
        state.end()
    }
}

impl IgnorePolicy {
    /// Builds one validated, compiled repository exclusion policy.
    ///
    /// # Errors
    ///
    /// Returns [`IgnorePatternError`] when a pattern is unsafe, malformed, or attempts to include
    /// protected generated state.
    pub fn new(
        configured_excludes: Vec<String>,
        configured_excludes_source: ConfigSource,
        include_defaults: Vec<String>,
        include_defaults_source: ConfigSource,
    ) -> Result<Self, IgnorePatternError> {
        let mut configured_excludes = normalize_patterns(configured_excludes)?;
        let mut include_defaults = normalize_patterns(include_defaults)?;
        validate_protected_includes(&include_defaults)?;
        configured_excludes.sort();
        configured_excludes.dedup();
        include_defaults.sort();
        include_defaults.dedup();
        let (include_prefixes, include_can_match_anywhere) = include_prefixes(&include_defaults);
        Ok(Self {
            protected_matcher: compile(PROTECTED_EXCLUDES.iter().copied())?,
            default_matcher: compile(DEFAULT_EXCLUDES.iter().copied())?,
            configured_matcher: compile(configured_excludes.iter().map(String::as_str))?,
            include_matcher: compile(include_defaults.iter().map(String::as_str))?,
            configured_excludes,
            configured_excludes_source,
            include_defaults,
            include_defaults_source,
            include_prefixes,
            include_can_match_anywhere,
        })
    }

    /// User-supplied exclusions selected by repository configuration precedence.
    #[must_use]
    pub fn configured_excludes(&self) -> &[String] {
        &self.configured_excludes
    }

    /// Configuration layer that supplied [`Self::configured_excludes`].
    #[must_use]
    pub const fn configured_excludes_source(&self) -> ConfigSource {
        self.configured_excludes_source
    }

    /// User-supplied exceptions to built-in default exclusions.
    #[must_use]
    pub fn include_defaults(&self) -> &[String] {
        &self.include_defaults
    }

    /// Configuration layer that supplied [`Self::include_defaults`].
    #[must_use]
    pub const fn include_defaults_source(&self) -> ConfigSource {
        self.include_defaults_source
    }

    /// Whether one repository-relative path must be omitted from automatic discovery.
    #[must_use]
    pub fn excludes(&self, relative: &Path, directory: bool) -> bool {
        let candidate = candidate(relative);
        let directory_candidate = directory.then(|| format!("{candidate}/"));
        let matches = |matcher: &GlobSet| {
            matcher.is_match(&candidate)
                || directory_candidate
                    .as_deref()
                    .is_some_and(|candidate| matcher.is_match(candidate))
        };
        if matches(&self.protected_matcher) || matches(&self.configured_matcher) {
            return true;
        }
        matches(&self.default_matcher)
            && !matches(&self.include_matcher)
            && !(directory && self.may_contain_included_default(relative))
    }

    /// Stable material included in incremental repository fingerprints.
    #[must_use]
    pub fn fingerprint_material(&self) -> String {
        format!(
            "version={IGNORE_POLICY_VERSION};protected={PROTECTED_EXCLUDES:?};defaults={DEFAULT_EXCLUDES:?};\
             excludes={:?};excludes_source={:?};include_defaults={:?};include_defaults_source={:?}",
            self.configured_excludes,
            self.configured_excludes_source,
            self.include_defaults,
            self.include_defaults_source
        )
    }

    fn may_contain_included_default(&self, directory: &Path) -> bool {
        if self.include_can_match_anywhere {
            return true;
        }
        self.include_prefixes
            .iter()
            .any(|prefix| prefix.starts_with(directory) || directory.starts_with(prefix))
    }
}

/// Validates configured exclusion globs.
///
/// # Errors
///
/// Returns [`IgnorePatternError`] for unsafe or malformed patterns.
pub fn validate_excludes(patterns: &[String]) -> Result<(), IgnorePatternError> {
    let normalized = normalize_patterns(patterns)?;
    compile(normalized.iter().map(String::as_str)).map(|_| ())
}

/// Validates exceptions to default exclusions.
///
/// # Errors
///
/// Returns [`IgnorePatternError`] for unsafe or malformed patterns, including attempts to include
/// protected repository metadata.
pub fn validate_include_defaults(patterns: &[String]) -> Result<(), IgnorePatternError> {
    let normalized = normalize_patterns(patterns)?;
    validate_protected_includes(&normalized)?;
    compile(normalized.iter().map(String::as_str)).map(|_| ())
}

fn validate_protected_includes(patterns: &[String]) -> Result<(), IgnorePatternError> {
    for pattern in patterns {
        for component in pattern.split('/') {
            if PROTECTED_DIRECTORY_NAMES.contains(&component) {
                return Err(IgnorePatternError::ProtectedInclude {
                    pattern: pattern.clone(),
                    directory: component.to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn normalize_patterns<I, S>(patterns: I) -> Result<Vec<String>, IgnorePatternError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    patterns
        .into_iter()
        .map(|pattern| normalize_pattern(pattern.as_ref()))
        .collect()
}

fn normalize_pattern(pattern: &str) -> Result<String, IgnorePatternError> {
    if pattern.trim().is_empty() {
        return Err(IgnorePatternError::Empty);
    }
    if absolute_pattern(pattern) {
        return Err(IgnorePatternError::Absolute(pattern.to_owned()));
    }
    if pattern.contains('\\') {
        return Err(IgnorePatternError::Backslash(pattern.to_owned()));
    }
    if pattern.split('/').any(|component| component == "..") {
        return Err(IgnorePatternError::ParentTraversal(pattern.to_owned()));
    }
    if pattern.chars().any(unsafe_character) {
        return Err(IgnorePatternError::UnsafeCharacters);
    }

    let directory_only = pattern.ends_with('/')
        || pattern
            .split('/')
            .rfind(|component| !component.is_empty())
            .is_some_and(|component| component == ".");
    let components = pattern
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>();
    if components.is_empty() {
        return Err(IgnorePatternError::MissingPathComponent(pattern.to_owned()));
    }
    validate_supported_syntax(pattern, &components)?;

    let mut normalized = components.join("/");
    if absolute_pattern(&normalized) {
        return Err(IgnorePatternError::Absolute(pattern.to_owned()));
    }
    if directory_only {
        normalized.push('/');
    }
    Ok(normalized)
}

fn validate_supported_syntax(pattern: &str, components: &[&str]) -> Result<(), IgnorePatternError> {
    let unsupported_delimiter = pattern.contains(['[', ']', '{', '}']);
    let unsupported_recursive = components
        .iter()
        .any(|component| component.contains("**") && *component != "**");
    if unsupported_delimiter || unsupported_recursive {
        return Err(IgnorePatternError::UnsupportedSyntax(pattern.to_owned()));
    }
    Ok(())
}

fn absolute_pattern(pattern: &str) -> bool {
    pattern.starts_with('/')
        || pattern
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
}

fn compile<'a>(patterns: impl Iterator<Item = &'a str>) -> Result<GlobSet, IgnorePatternError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
            .map_err(|error| IgnorePatternError::InvalidGlob {
                pattern: pattern.to_owned(),
                detail: error.to_string(),
            })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|error| IgnorePatternError::InvalidGlob {
            pattern: "<set>".to_owned(),
            detail: error.to_string(),
        })
}

fn include_prefixes(patterns: &[String]) -> (Vec<PathBuf>, bool) {
    let mut prefixes = Vec::new();
    let mut can_match_anywhere = false;
    for pattern in patterns {
        let mut prefix = PathBuf::new();
        for component in pattern.split('/') {
            if component.contains(['*', '?']) {
                break;
            }
            if !component.is_empty() {
                prefix.push(component);
            }
        }
        if prefix.as_os_str().is_empty() {
            can_match_anywhere = true;
        } else {
            prefixes.push(prefix);
        }
    }
    prefixes.sort();
    prefixes.dedup();
    (prefixes, can_match_anywhere)
}

fn candidate(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn unsafe_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(excludes: &[&str], includes: &[&str]) -> IgnorePolicy {
        IgnorePolicy::new(
            excludes.iter().map(ToString::to_string).collect(),
            ConfigSource::WorkspaceManifest,
            includes.iter().map(ToString::to_string).collect(),
            ConfigSource::WorkspaceManifest,
        )
        .expect("valid fixture policy")
    }

    #[test]
    fn defaults_should_exclude_nested_dependency_tree() {
        assert!(policy(&[], &[]).excludes(Path::new("web/node_modules/react/index.js"), false));
    }

    #[test]
    fn configured_excludes_should_match_repository_relative_globs() {
        assert!(policy(&["coverage/**"], &[]).excludes(Path::new("coverage/lcov.info"), false));
    }

    #[test]
    fn configured_excludes_should_support_single_component_wildcards() {
        assert!(policy(&["src/*/?.rs"], &[]).excludes(Path::new("src/api/x.rs"), false));
    }

    #[test]
    fn configured_excludes_should_normalize_and_deduplicate_patterns() {
        let policy = policy(&["./coverage//**", "coverage/./**", "coverage/**"], &[]);

        assert_eq!(policy.configured_excludes(), &["coverage/**"]);
    }

    #[test]
    fn configured_excludes_should_match_normalized_patterns() {
        assert!(policy(&["./coverage/./**"], &[]).excludes(Path::new("coverage/lcov.info"), false));
    }

    #[test]
    fn directory_patterns_should_preserve_their_terminal_separator() {
        let policy = policy(&["./coverage//"], &[]);

        assert!(policy.excludes(Path::new("coverage"), true));
        assert!(!policy.excludes(Path::new("coverage"), false));
    }

    #[test]
    fn ordinary_globs_should_match_directories_without_a_terminal_separator() {
        let policy = policy(&["generated/*"], &[]);

        assert!(policy.excludes(Path::new("generated/output"), true));
    }

    #[test]
    fn include_defaults_should_reopen_only_selected_subtree() {
        let policy = policy(&[], &["./vendor//internal-sdk/./**"]);
        assert_eq!(
            (
                policy.excludes(Path::new("vendor/internal-sdk/src/lib.rs"), false),
                policy.excludes(Path::new("vendor/external/src/lib.rs"), false),
            ),
            (false, true)
        );
    }

    #[test]
    fn configured_excludes_should_override_default_includes() {
        assert!(
            policy(
                &["vendor/internal-sdk/private/**"],
                &["vendor/internal-sdk/**"]
            )
            .excludes(Path::new("vendor/internal-sdk/private/key.rs"), false)
        );
    }

    #[test]
    fn include_defaults_should_keep_ancestor_traversable() {
        assert!(!policy(&[], &["./vendor//internal-sdk/./**"]).excludes(Path::new("vendor"), true));
    }

    #[test]
    fn canonical_equivalent_policies_should_have_the_same_fingerprint() {
        let canonical = policy(&["coverage/**"], &["vendor/internal-sdk/**"]);
        let redundant = policy(&["./coverage//./**"], &["./vendor//internal-sdk/./**"]);

        assert_eq!(
            canonical.fingerprint_material(),
            redundant.fingerprint_material()
        );
    }

    #[test]
    fn include_defaults_should_reject_protected_directories() {
        assert!(matches!(
            validate_include_defaults(&["./.git//config".to_owned()]),
            Err(IgnorePatternError::ProtectedInclude { .. })
        ));
    }

    #[test]
    fn patterns_should_reject_parent_traversal() {
        assert!(matches!(
            validate_excludes(&["./safe/../outside/**".to_owned()]),
            Err(IgnorePatternError::ParentTraversal(_))
        ));
    }

    #[test]
    fn patterns_should_reject_windows_absolute_paths_after_normalization() {
        assert!(matches!(
            validate_excludes(&["./C:/outside/**".to_owned()]),
            Err(IgnorePatternError::Absolute(_))
        ));
    }

    #[test]
    fn patterns_should_reject_whitespace_only_values() {
        assert!(matches!(
            validate_excludes(&["   ".to_owned()]),
            Err(IgnorePatternError::Empty)
        ));
    }

    #[test]
    fn patterns_should_reject_values_without_path_components() {
        assert!(matches!(
            validate_excludes(&["././".to_owned()]),
            Err(IgnorePatternError::MissingPathComponent(_))
        ));
    }

    #[test]
    fn patterns_should_reject_character_classes() {
        assert!(matches!(
            validate_excludes(&["src/[ab]/**".to_owned()]),
            Err(IgnorePatternError::UnsupportedSyntax(_))
        ));
    }

    #[test]
    fn patterns_should_reject_alternations() {
        assert!(matches!(
            validate_excludes(&["{src,test}/**".to_owned()]),
            Err(IgnorePatternError::UnsupportedSyntax(_))
        ));
    }

    #[test]
    fn patterns_should_reject_non_component_recursive_wildcards() {
        assert!(matches!(
            validate_excludes(&["src/**generated/**".to_owned()]),
            Err(IgnorePatternError::UnsupportedSyntax(_))
        ));
    }

    #[test]
    fn patterns_should_reject_three_star_wildcards() {
        assert!(matches!(
            validate_excludes(&["src/***/generated".to_owned()]),
            Err(IgnorePatternError::UnsupportedSyntax(_))
        ));
    }
}
