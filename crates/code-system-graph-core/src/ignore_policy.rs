//! Shared repository discovery exclusions.

use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::{IncrementalIgnore, WalkBuilder};
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
    use_gitignore: bool,
    use_gitignore_source: ConfigSource,
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
            && self.use_gitignore == other.use_gitignore
            && self.use_gitignore_source == other.use_gitignore_source
    }
}

impl Eq for IgnorePolicy {}

impl Serialize for IgnorePolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("IgnorePolicy", 6)?;
        state.serialize_field("configured_excludes", &self.configured_excludes)?;
        state.serialize_field(
            "configured_excludes_source",
            &self.configured_excludes_source,
        )?;
        state.serialize_field("include_defaults", &self.include_defaults)?;
        state.serialize_field("include_defaults_source", &self.include_defaults_source)?;
        state.serialize_field("use_gitignore", &self.use_gitignore)?;
        state.serialize_field("use_gitignore_source", &self.use_gitignore_source)?;
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
        Self::with_gitignore(
            configured_excludes,
            configured_excludes_source,
            include_defaults,
            include_defaults_source,
            false,
            ConfigSource::Default,
        )
    }

    /// Builds one validated, compiled repository exclusion policy with optional `.gitignore`
    /// discovery.
    ///
    /// # Errors
    ///
    /// Returns [`IgnorePatternError`] when a configured pattern is unsafe or malformed.
    pub fn with_gitignore(
        configured_excludes: Vec<String>,
        configured_excludes_source: ConfigSource,
        include_defaults: Vec<String>,
        include_defaults_source: ConfigSource,
        use_gitignore: bool,
        use_gitignore_source: ConfigSource,
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
            use_gitignore,
            use_gitignore_source,
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

    /// Whether repository-contained `.gitignore` files participate in automatic discovery.
    #[must_use]
    pub const fn use_gitignore(&self) -> bool {
        self.use_gitignore
    }

    /// Configuration layer that selected [`Self::use_gitignore`].
    #[must_use]
    pub const fn use_gitignore_source(&self) -> ConfigSource {
        self.use_gitignore_source
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
             excludes={:?};excludes_source={:?};include_defaults={:?};include_defaults_source={:?};\
             use_gitignore={};use_gitignore_source={:?}",
            self.configured_excludes,
            self.configured_excludes_source,
            self.include_defaults,
            self.include_defaults_source,
            self.use_gitignore,
            self.use_gitignore_source
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

/// Failure while applying repository-contained `.gitignore` files during discovery.
#[derive(Debug, Error)]
pub enum RepositoryDiscoveryError {
    /// Directory traversal or an enabled ignore file failed.
    #[error("repository discovery failed under `{root}`: {source}")]
    Ignore {
        /// Registered repository root.
        root: PathBuf,
        /// Bounded walker or ignore-file failure.
        #[source]
        source: ignore::Error,
    },
    /// A walker result unexpectedly escaped its configured root.
    #[error("repository discovery path `{path}` escaped root `{root}`")]
    OutsideRoot {
        /// Registered repository root.
        root: PathBuf,
        /// Unexpected walker path.
        path: PathBuf,
    },
}

/// Cached matcher for event paths outside a complete repository traversal.
#[derive(Debug, Clone)]
pub struct RepositoryPathMatcher {
    policy: IgnorePolicy,
    gitignore: Option<IncrementalIgnore>,
}

impl RepositoryPathMatcher {
    /// Builds a matcher rooted at one registered checkout.
    #[must_use]
    pub fn new(root: &Path, policy: IgnorePolicy) -> Self {
        let gitignore = policy
            .use_gitignore()
            .then(|| {
                let mut matchers = gitignore_walk_builder(root, true).build_matchers();
                matchers.pop()
            })
            .flatten();
        Self { policy, gitignore }
    }

    /// Returns whether a path is excluded by protected, configured, default, or Git rules.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryDiscoveryError`] when an enabled ignore file cannot be interpreted.
    pub fn excludes(
        &mut self,
        relative: &Path,
        directory: bool,
    ) -> Result<bool, RepositoryDiscoveryError> {
        if self.policy.excludes(relative, directory) {
            return Ok(true);
        }
        let Some(matcher) = self.gitignore.as_mut() else {
            return Ok(false);
        };
        let (match_result, error) = matcher.matched_with_errors(relative, directory);
        if let Some(source) = error {
            return Err(RepositoryDiscoveryError::Ignore {
                root: matcher.root().to_path_buf(),
                source,
            });
        }
        Ok(match_result.is_ignore())
    }
}

/// Discovers regular, non-symlink files under one repository with the complete native policy.
///
/// `max_depth` uses walker depth, where the configured repository root is depth zero.
///
/// # Errors
///
/// Returns [`RepositoryDiscoveryError`] for directory or enabled ignore-file failures.
pub fn discover_repository_files(
    root: &Path,
    policy: &IgnorePolicy,
    max_depth: Option<usize>,
) -> Result<Vec<PathBuf>, RepositoryDiscoveryError> {
    let mut builder = gitignore_walk_builder(root, policy.use_gitignore());
    if let Some(max_depth) = max_depth {
        builder.max_depth(Some(max_depth));
    }
    let filter_policy = policy.clone();
    let filter_root = root.to_path_buf();
    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        let Some(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_symlink() {
            return false;
        }
        entry
            .path()
            .strip_prefix(&filter_root)
            .is_ok_and(|relative| !filter_policy.excludes(relative, file_type.is_dir()))
    });
    builder.sort_by_file_path(std::path::Path::cmp);

    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry.map_err(|source| RepositoryDiscoveryError::Ignore {
            root: root.to_path_buf(),
            source,
        })?;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if entry.depth() == 0 || !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let relative =
            entry
                .path()
                .strip_prefix(root)
                .map_err(|_| RepositoryDiscoveryError::OutsideRoot {
                    root: root.to_path_buf(),
                    path: entry.path().to_path_buf(),
                })?;
        files.push(relative.to_path_buf());
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn gitignore_walk_builder(root: &Path, use_gitignore: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(false)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_ignore(use_gitignore)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false);
    builder
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
    use std::fs;

    use tempfile::tempdir;

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

    fn gitignore_policy(excludes: &[&str], includes: &[&str]) -> IgnorePolicy {
        IgnorePolicy::with_gitignore(
            excludes.iter().map(ToString::to_string).collect(),
            ConfigSource::WorkspaceManifest,
            includes.iter().map(ToString::to_string).collect(),
            ConfigSource::WorkspaceManifest,
            true,
            ConfigSource::WorkspaceManifest,
        )
        .expect("valid fixture policy")
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn gitignore_should_be_opt_in_and_support_nested_rules_and_negation() {
        let checkout = tempdir().expect("checkout");
        let root = checkout.path();
        write(
            root,
            ".gitignore",
            "ignored/*\n!ignored/keep.rs\nspace\\ path.rs\n",
        );
        write(root, "ignored/drop.rs", "drop");
        write(root, "ignored/keep.rs", "keep");
        write(root, "space path.rs", "space");
        write(root, "nested/.gitignore", "*.rs\n!keep.rs\n");
        write(root, "nested/drop.rs", "drop");
        write(root, "nested/keep.rs", "keep");

        let enabled = discover_repository_files(root, &gitignore_policy(&[], &[]), None)
            .expect("enabled discovery");
        assert!(enabled.contains(&PathBuf::from("ignored/keep.rs")));
        assert!(enabled.contains(&PathBuf::from("nested/keep.rs")));
        assert!(!enabled.contains(&PathBuf::from("ignored/drop.rs")));
        assert!(!enabled.contains(&PathBuf::from("nested/drop.rs")));
        assert!(!enabled.contains(&PathBuf::from("space path.rs")));

        let disabled =
            discover_repository_files(root, &policy(&[], &[]), None).expect("disabled discovery");
        assert!(disabled.contains(&PathBuf::from("ignored/drop.rs")));
        assert!(disabled.contains(&PathBuf::from("nested/drop.rs")));
        assert!(disabled.contains(&PathBuf::from("space path.rs")));
    }

    #[test]
    fn gitignore_should_not_read_parent_dot_ignore_or_git_exclude_rules() {
        let parent = tempdir().expect("parent");
        let root = parent.path().join("checkout");
        fs::create_dir_all(root.join(".git/info")).expect("git metadata");
        write(parent.path(), ".gitignore", "from-parent.rs\n");
        write(&root, ".ignore", "from-dot-ignore.rs\n");
        write(&root, ".git/info/exclude", "from-git-exclude.rs\n");
        for file in [
            "from-parent.rs",
            "from-dot-ignore.rs",
            "from-git-exclude.rs",
        ] {
            write(&root, file, file);
        }

        let files =
            discover_repository_files(&root, &gitignore_policy(&[], &[]), None).expect("discovery");
        for file in [
            "from-parent.rs",
            "from-dot-ignore.rs",
            "from-git-exclude.rs",
        ] {
            assert!(files.contains(&PathBuf::from(file)), "missing {file}");
        }
    }

    #[test]
    fn explicit_and_protected_exclusions_should_override_gitignore_negations() {
        let checkout = tempdir().expect("checkout");
        let root = checkout.path();
        write(
            root,
            ".gitignore",
            "!generated/private.rs\n!vendor/sdk/lib.rs\n!.git/config\n",
        );
        write(root, "generated/private.rs", "private");
        write(root, "vendor/sdk/lib.rs", "sdk");
        write(root, ".git/config", "config");

        let files = discover_repository_files(
            root,
            &gitignore_policy(&["generated/**"], &["vendor/sdk/**"]),
            None,
        )
        .expect("discovery");
        assert!(!files.contains(&PathBuf::from("generated/private.rs")));
        assert!(files.contains(&PathBuf::from("vendor/sdk/lib.rs")));
        assert!(!files.contains(&PathBuf::from(".git/config")));
    }

    #[test]
    fn path_matcher_should_follow_nested_gitignore_rules() {
        let checkout = tempdir().expect("checkout");
        let root = checkout.path();
        write(root, ".gitignore", "root.rs\n");
        write(root, "nested/.gitignore", "*.rs\n!keep.rs\n");
        let mut matcher = RepositoryPathMatcher::new(root, gitignore_policy(&[], &[]));

        assert!(matcher.excludes(Path::new("root.rs"), false).expect("root"));
        assert!(
            matcher
                .excludes(Path::new("nested/drop.rs"), false)
                .expect("nested drop")
        );
        assert!(
            !matcher
                .excludes(Path::new("nested/keep.rs"), false)
                .expect("nested keep")
        );
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
