//! Bounded, cancellable local Git change discovery and commit gating.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use code_system_graph_model::{CheckoutId, NativePath, NativePathEncoding, RepoId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const DEFAULT_OUTPUT_CAP: usize = 16 * 1024 * 1024;
const DEFAULT_STDERR_CAP: usize = 256 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Selects the repository state represented by a [`ChangeSet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeScope {
    /// Tracked worktree changes not present in the index.
    Unstaged,
    /// Changes currently present in the index.
    Staged,
    /// Staged, tracked worktree, and untracked changes as separate layers.
    All,
    /// Changes from the merge base of a validated ref to `HEAD`.
    Compare {
        /// Branch, tag, or other non-expression Git ref.
        reference: String,
    },
    /// The changes introduced by one full hexadecimal commit object ID.
    Commit {
        /// Full SHA-1 or SHA-256 commit object ID.
        sha: String,
    },
    /// Changes between two validated refs, equivalent to `base..head`.
    Range {
        /// Older endpoint.
        base: String,
        /// Newer endpoint.
        head: String,
    },
    /// A provider-specific pull request.
    PullRequest {
        /// External provider identifier, such as `github`.
        provider: String,
        /// Provider-local pull-request number.
        number: u64,
    },
}

/// Identifies the layer from which a changed file was discovered.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSourceLayer {
    /// The Git index.
    Staged,
    /// Tracked files in the working tree.
    Worktree,
    /// Untracked worktree entries.
    Untracked,
    /// A committed tree comparison.
    Commit,
}

/// Git-level classification of a changed file.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChangedFileStatus {
    /// A newly added tracked file.
    Added,
    /// A modified tracked file.
    Modified,
    /// A deleted tracked file.
    Deleted,
    /// A renamed tracked file.
    Renamed,
    /// A copied tracked file.
    Copied,
    /// An untracked file whose body was not read.
    Untracked,
}

/// Kind of one changed line in a zero-context hunk.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChangedLineKind {
    /// A line removed from the old side.
    Removed,
    /// A line added to the new side.
    Added,
}

/// Position-only changed-line evidence.
///
/// Source text is deliberately excluded so a persisted [`ChangeSet`] cannot retain diff bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangedLine {
    /// Whether this line was added or removed.
    pub kind: ChangedLineKind,
    /// One-based line on the old side, when applicable.
    pub old_line: Option<u32>,
    /// One-based line on the new side, when applicable.
    pub new_line: Option<u32>,
}

/// One zero-context unified-diff hunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeHunk {
    /// One-based old-side starting line.
    pub old_start: u32,
    /// Number of old-side lines covered.
    pub old_count: u32,
    /// One-based new-side starting line.
    pub new_start: u32,
    /// Number of new-side lines covered.
    pub new_count: u32,
    /// Changed line positions, without source text.
    pub lines: Vec<ChangedLine>,
}

/// One changed file, preserving native path bytes where the platform permits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangedFile {
    /// Git change classification.
    pub status: ChangedFileStatus,
    /// Original repository-relative path, when one exists.
    pub old_path: Option<NativePath>,
    /// Resulting repository-relative path, when one exists.
    pub new_path: Option<NativePath>,
    /// Whether Git reported a binary numstat.
    pub binary: bool,
    /// Position-only zero-context hunks.
    pub hunks: Vec<ChangeHunk>,
    /// State layer that supplied this record.
    pub source: ChangeSourceLayer,
}

/// Version map for analyzers whose output is tied to a change set.
pub type AnalyzerVersions = BTreeMap<String, String>;

/// Immutable, fingerprinted description of a repository change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeSet {
    /// Stable registered repository identity.
    pub repo_id: RepoId,
    /// Stable concrete checkout identity.
    pub checkout_id: CheckoutId,
    /// Canonical, lossless worktree root.
    pub worktree: NativePath,
    /// Canonical, lossless common Git directory.
    pub git_common_dir: NativePath,
    /// Requested scope.
    pub scope: ChangeScope,
    /// Symbolic checkout `HEAD`, when attached.
    pub checkout_head_ref: Option<String>,
    /// Commit currently checked out, or the empty-tree object for an unborn checkout.
    pub checkout_head_sha: String,
    /// Scope base ref or object ID, when applicable.
    pub base_ref: Option<String>,
    /// Scope head ref or object ID, when applicable.
    pub head_ref: Option<String>,
    /// Resolved scope head, or the empty-tree object for an unborn local scope.
    pub head_sha: String,
    /// BLAKE3 hash of exact bounded staged Git output.
    pub staged_hash: String,
    /// BLAKE3 hash of exact bounded tracked and untracked worktree output.
    pub worktree_hash: String,
    /// BLAKE3 fingerprint of exact bounded Git output and identity metadata.
    pub exact_diff_fingerprint: String,
    /// Workspace manifest fingerprint used by analysis.
    pub workspace_manifest_hash: String,
    /// Contract registry fingerprint used by analysis.
    pub contract_registry_hash: String,
    /// Analyzer names and exact versions used by analysis.
    pub analyzer_versions: AnalyzerVersions,
    /// Deterministically ordered changed files.
    pub files: Vec<ChangedFile>,
}

/// Input required to collect one local change set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeRequest {
    /// Registered repository identity.
    pub repo_id: RepoId,
    /// Concrete checkout identity.
    pub checkout_id: CheckoutId,
    /// Any path accepted by `git -C`, normally the expected worktree root.
    pub worktree: PathBuf,
    /// State to collect.
    pub scope: ChangeScope,
    /// Current workspace manifest fingerprint.
    pub workspace_manifest_hash: String,
    /// Current contract registry fingerprint.
    pub contract_registry_hash: String,
    /// Current analyzer versions.
    pub analyzer_versions: AnalyzerVersions,
}

/// Read-only source of repository changes.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ChangeProvider: Send + Sync {
    /// Collects a bounded and fingerprinted change set.
    ///
    /// # Errors
    ///
    /// Returns [`ChangeError`] for invalid input, unsupported scopes, Git failures, cancellation,
    /// timeouts, malformed output, and output-budget exhaustion.
    async fn changes(
        &self,
        request: &ChangeRequest,
        cancellation: &CancellationToken,
    ) -> Result<ChangeSet, ChangeError>;
}

/// Configuration for the production local Git provider.
#[derive(Debug, Clone)]
pub struct GitCliChangeProvider {
    git_binary: OsString,
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
}

impl Default for GitCliChangeProvider {
    fn default() -> Self {
        Self {
            git_binary: OsString::from("git"),
            timeout: DEFAULT_TIMEOUT,
            stdout_cap: DEFAULT_OUTPUT_CAP,
            stderr_cap: DEFAULT_STDERR_CAP,
        }
    }
}

impl GitCliChangeProvider {
    /// Creates a provider using `git` from the process search path and conservative budgets.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a provider with explicit executable and process budgets.
    ///
    /// Zero output limits are rejected when [`ChangeProvider::changes`] is called.
    #[must_use]
    pub fn with_limits(
        git_binary: impl Into<OsString>,
        timeout: Duration,
        stdout_cap: usize,
        stderr_cap: usize,
    ) -> Self {
        Self {
            git_binary: git_binary.into(),
            timeout,
            stdout_cap,
            stderr_cap,
        }
    }

    async fn git(
        &self,
        worktree: &Path,
        args: &[OsString],
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, ChangeError> {
        if self.stdout_cap == 0 || self.stderr_cap == 0 {
            return Err(ChangeError::InvalidLimit);
        }
        if cancellation.is_cancelled() {
            return Err(ChangeError::Cancelled);
        }

        let mut command = Command::new(&self.git_binary);
        command
            .arg("-C")
            .arg(worktree)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|source| ChangeError::Spawn { source })?;
        let stdout = child.stdout.take().ok_or(ChangeError::MissingPipe)?;
        let stderr = child.stderr.take().ok_or(ChangeError::MissingPipe)?;
        let stdout_cap = self.stdout_cap;
        let stderr_cap = self.stderr_cap;
        let stdout_task = tokio::spawn(async move { read_bounded(stdout, stdout_cap).await });
        let stderr_task = tokio::spawn(async move { read_bounded(stderr, stderr_cap).await });

        let status = tokio::select! {
            () = cancellation.cancelled() => {
                terminate(&mut child).await;
                stdout_task.abort();
                stderr_task.abort();
                return Err(ChangeError::Cancelled);
            }
            () = tokio::time::sleep(self.timeout) => {
                terminate(&mut child).await;
                stdout_task.abort();
                stderr_task.abort();
                return Err(ChangeError::Timeout { timeout: self.timeout });
            }
            result = child.wait() => result.map_err(|source| ChangeError::Wait { source })?,
        };
        let stdout = stdout_task
            .await
            .map_err(|source| ChangeError::ReaderTask { source })??;
        let stderr = stderr_task
            .await
            .map_err(|source| ChangeError::ReaderTask { source })??;
        if !status.success() {
            return Err(ChangeError::GitFailed {
                status: status.code(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            });
        }
        Ok(stdout)
    }

    async fn text_query(
        &self,
        worktree: &Path,
        args: &[&str],
        cancellation: &CancellationToken,
    ) -> Result<String, ChangeError> {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        let bytes = self.git(worktree, &args, cancellation).await?;
        let value = std::str::from_utf8(&bytes).map_err(|_| ChangeError::NonUtf8Metadata)?;
        Ok(value.trim_end_matches(['\r', '\n']).to_owned())
    }

    async fn collect_layer(
        &self,
        worktree: &Path,
        source: ChangeSourceLayer,
        revision_args: &[OsString],
        cancellation: &CancellationToken,
    ) -> Result<LayerResult, ChangeError> {
        let mut raw_args = diff_prefix("--raw");
        raw_args.extend_from_slice(revision_args);
        let raw = self.git(worktree, &raw_args, cancellation).await?;

        let mut numstat_args = diff_prefix("--numstat");
        numstat_args.extend_from_slice(revision_args);
        let numstat = self.git(worktree, &numstat_args, cancellation).await?;

        let mut patch_args = diff_prefix("--patch");
        patch_args.push(OsString::from("--unified=0"));
        patch_args.push(OsString::from("--no-prefix"));
        patch_args.extend_from_slice(revision_args);
        let patch = self.git(worktree, &patch_args, cancellation).await?;

        let mut files = parse_raw(&raw, source)?;
        apply_numstat(&numstat, &mut files)?;
        apply_patch(&patch, &mut files)?;
        let material = framed_material(&[&raw, &numstat, &patch]);
        Ok(LayerResult { files, material })
    }

    async fn collect_untracked(
        &self,
        worktree: &Path,
        cancellation: &CancellationToken,
    ) -> Result<LayerResult, ChangeError> {
        let args = [
            OsString::from("ls-files"),
            OsString::from("--others"),
            OsString::from("--exclude-standard"),
            OsString::from("-z"),
        ];
        let output = self.git(worktree, &args, cancellation).await?;
        let files = output
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| ChangedFile {
                status: ChangedFileStatus::Untracked,
                old_path: None,
                new_path: Some(native_path_bytes(path)),
                binary: false,
                hunks: Vec::new(),
                source: ChangeSourceLayer::Untracked,
            })
            .collect();
        Ok(LayerResult {
            files,
            material: framed_material(&[&output]),
        })
    }
}

#[async_trait]
impl ChangeProvider for GitCliChangeProvider {
    #[expect(
        clippy::too_many_lines,
        reason = "scope orchestration remains linear so every Git layer is auditable"
    )]
    async fn changes(
        &self,
        request: &ChangeRequest,
        cancellation: &CancellationToken,
    ) -> Result<ChangeSet, ChangeError> {
        validate_scope(&request.scope)?;
        let top = self
            .text_query(
                &request.worktree,
                &["rev-parse", "--show-toplevel"],
                cancellation,
            )
            .await?;
        let canonical_worktree = canonicalize(Path::new(&top))?;
        let common = self
            .text_query(
                &canonical_worktree,
                &["rev-parse", "--git-common-dir"],
                cancellation,
            )
            .await?;
        let common_path = Path::new(&common);
        let common_path = if common_path.is_absolute() {
            common_path.to_path_buf()
        } else {
            canonical_worktree.join(common_path)
        };
        let canonical_common = canonicalize(&common_path)?;
        let symbolic = self
            .text_query(
                &canonical_worktree,
                &["symbolic-ref", "-q", "HEAD"],
                cancellation,
            )
            .await;
        let checkout_head_ref = match symbolic {
            Ok(value) => Some(value),
            Err(ChangeError::GitFailed { .. }) => None,
            Err(error) => return Err(error),
        };
        let checkout_head_sha = match self
            .text_query(
                &canonical_worktree,
                &["rev-parse", "--verify", "HEAD^{commit}"],
                cancellation,
            )
            .await
        {
            Ok(commit) => commit,
            Err(ChangeError::GitFailed { .. })
                if checkout_head_ref.is_some()
                    && matches!(
                        &request.scope,
                        ChangeScope::Unstaged | ChangeScope::Staged | ChangeScope::All
                    ) =>
            {
                self.text_query(
                    &canonical_worktree,
                    &["hash-object", "-t", "tree", "--stdin"],
                    cancellation,
                )
                .await?
            }
            Err(error) => return Err(error),
        };

        let mut staged = LayerResult::default();
        let mut worktree = LayerResult::default();
        let mut committed = LayerResult::default();
        let (base_ref, head_ref, head_sha) = match &request.scope {
            ChangeScope::Unstaged => {
                worktree = self
                    .collect_layer(
                        &canonical_worktree,
                        ChangeSourceLayer::Worktree,
                        &[],
                        cancellation,
                    )
                    .await?;
                (None, Some("HEAD".to_owned()), checkout_head_sha.clone())
            }
            ChangeScope::Staged => {
                staged = self
                    .collect_layer(
                        &canonical_worktree,
                        ChangeSourceLayer::Staged,
                        &[OsString::from("--cached")],
                        cancellation,
                    )
                    .await?;
                (None, Some("HEAD".to_owned()), checkout_head_sha.clone())
            }
            ChangeScope::All => {
                staged = self
                    .collect_layer(
                        &canonical_worktree,
                        ChangeSourceLayer::Staged,
                        &[OsString::from("--cached")],
                        cancellation,
                    )
                    .await?;
                worktree = self
                    .collect_layer(
                        &canonical_worktree,
                        ChangeSourceLayer::Worktree,
                        &[],
                        cancellation,
                    )
                    .await?;
                let untracked = self
                    .collect_untracked(&canonical_worktree, cancellation)
                    .await?;
                worktree.files.extend(untracked.files);
                worktree.material.extend(untracked.material);
                (None, Some("HEAD".to_owned()), checkout_head_sha.clone())
            }
            ChangeScope::Compare { reference } => {
                let expression = OsString::from(format!("{reference}...HEAD"));
                committed = self
                    .collect_layer(
                        &canonical_worktree,
                        ChangeSourceLayer::Commit,
                        &[expression],
                        cancellation,
                    )
                    .await?;
                (
                    Some(reference.clone()),
                    Some("HEAD".to_owned()),
                    checkout_head_sha.clone(),
                )
            }
            ChangeScope::Commit { sha } => {
                committed = self
                    .collect_layer(
                        &canonical_worktree,
                        ChangeSourceLayer::Commit,
                        &[OsString::from(format!("{sha}^!"))],
                        cancellation,
                    )
                    .await?;
                let resolved = self
                    .text_query(
                        &canonical_worktree,
                        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
                        cancellation,
                    )
                    .await?;
                (None, Some(sha.clone()), resolved)
            }
            ChangeScope::Range { base, head } => {
                committed = self
                    .collect_layer(
                        &canonical_worktree,
                        ChangeSourceLayer::Commit,
                        &[OsString::from(format!("{base}..{head}"))],
                        cancellation,
                    )
                    .await?;
                let resolved = self
                    .text_query(
                        &canonical_worktree,
                        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
                        cancellation,
                    )
                    .await?;
                (Some(base.clone()), Some(head.clone()), resolved)
            }
            ChangeScope::PullRequest { .. } => return Err(ChangeError::PullRequestUnsupported),
        };

        let staged_hash = hash_material(b"code-system-graph-staged-v1", &staged.material);
        let worktree_hash = hash_material(b"code-system-graph-worktree-v1", &worktree.material);
        let mut files = staged.files;
        files.extend(worktree.files);
        files.extend(committed.files);
        sort_and_deduplicate(&mut files);

        let worktree_native = native_path(&canonical_worktree);
        let common_native = native_path(&canonical_common);
        let exact_diff_fingerprint = exact_fingerprint(
            request,
            &worktree_native,
            &common_native,
            &checkout_head_sha,
            base_ref.as_deref(),
            head_ref.as_deref(),
            &head_sha,
            &[&staged.material, &worktree.material, &committed.material],
        )?;
        Ok(ChangeSet {
            repo_id: request.repo_id.clone(),
            checkout_id: request.checkout_id.clone(),
            worktree: worktree_native,
            git_common_dir: common_native,
            scope: request.scope.clone(),
            checkout_head_ref,
            checkout_head_sha,
            base_ref,
            head_ref,
            head_sha,
            staged_hash,
            worktree_hash,
            exact_diff_fingerprint,
            workspace_manifest_hash: request.workspace_manifest_hash.clone(),
            contract_registry_hash: request.contract_registry_hash.clone(),
            analyzer_versions: request.analyzer_versions.clone(),
            files,
        })
    }
}

/// Current fingerprints used to determine whether analysis remains valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeValidityInput {
    /// Current repository identity.
    pub repo_id: RepoId,
    /// Current checkout identity.
    pub checkout_id: CheckoutId,
    /// Current canonical worktree.
    pub worktree: NativePath,
    /// Current canonical common Git directory.
    pub git_common_dir: NativePath,
    /// Current checked-out commit.
    pub checkout_head_sha: String,
    /// Current staged output hash.
    pub staged_hash: String,
    /// Current worktree output hash.
    pub worktree_hash: String,
    /// Current exact diff fingerprint.
    pub exact_diff_fingerprint: String,
    /// Current workspace manifest hash.
    pub workspace_manifest_hash: String,
    /// Current contract registry hash.
    pub contract_registry_hash: String,
    /// Current analyzer versions.
    pub analyzer_versions: AnalyzerVersions,
}

impl From<&ChangeSet> for ChangeValidityInput {
    fn from(value: &ChangeSet) -> Self {
        Self {
            repo_id: value.repo_id.clone(),
            checkout_id: value.checkout_id.clone(),
            worktree: value.worktree.clone(),
            git_common_dir: value.git_common_dir.clone(),
            checkout_head_sha: value.checkout_head_sha.clone(),
            staged_hash: value.staged_hash.clone(),
            worktree_hash: value.worktree_hash.clone(),
            exact_diff_fingerprint: value.exact_diff_fingerprint.clone(),
            workspace_manifest_hash: value.workspace_manifest_hash.clone(),
            contract_registry_hash: value.contract_registry_hash.clone(),
            analyzer_versions: value.analyzer_versions.clone(),
        }
    }
}

/// Machine-readable cause of stale change analysis.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StaleReason {
    /// Repository registration changed.
    RepositoryChanged,
    /// Checkout identity changed.
    CheckoutChanged,
    /// Canonical worktree changed.
    WorktreeChanged,
    /// Common Git directory changed.
    GitCommonDirectoryChanged,
    /// Checked-out commit changed.
    HeadChanged,
    /// Git index changed.
    StagedChangesChanged,
    /// Tracked or untracked worktree state changed.
    WorktreeChangesChanged,
    /// Exact diff fingerprint changed.
    ExactDiffChanged,
    /// Workspace manifest changed.
    WorkspaceManifestChanged,
    /// Contract registry changed.
    ContractRegistryChanged,
    /// Analyzer version set changed.
    AnalyzerVersionsChanged,
}

/// Result of validating a previously analyzed change set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ChangeValidity {
    /// Every relevant fingerprint still matches.
    Valid,
    /// One or more inputs changed and analysis must not be reused.
    Stale {
        /// Deterministically ordered stale causes.
        reasons: Vec<StaleReason>,
        /// Safe operator action.
        remediation: String,
    },
}

/// Compares every analysis-relevant identity and fingerprint.
#[must_use]
pub fn validate_change_set(
    change_set: &ChangeSet,
    current: &ChangeValidityInput,
) -> ChangeValidity {
    let mut reasons = Vec::new();
    compare(
        &change_set.repo_id,
        &current.repo_id,
        StaleReason::RepositoryChanged,
        &mut reasons,
    );
    compare(
        &change_set.checkout_id,
        &current.checkout_id,
        StaleReason::CheckoutChanged,
        &mut reasons,
    );
    compare(
        &change_set.worktree,
        &current.worktree,
        StaleReason::WorktreeChanged,
        &mut reasons,
    );
    compare(
        &change_set.git_common_dir,
        &current.git_common_dir,
        StaleReason::GitCommonDirectoryChanged,
        &mut reasons,
    );
    compare(
        &change_set.checkout_head_sha,
        &current.checkout_head_sha,
        StaleReason::HeadChanged,
        &mut reasons,
    );
    compare(
        &change_set.staged_hash,
        &current.staged_hash,
        StaleReason::StagedChangesChanged,
        &mut reasons,
    );
    compare(
        &change_set.worktree_hash,
        &current.worktree_hash,
        StaleReason::WorktreeChangesChanged,
        &mut reasons,
    );
    compare(
        &change_set.exact_diff_fingerprint,
        &current.exact_diff_fingerprint,
        StaleReason::ExactDiffChanged,
        &mut reasons,
    );
    compare(
        &change_set.workspace_manifest_hash,
        &current.workspace_manifest_hash,
        StaleReason::WorkspaceManifestChanged,
        &mut reasons,
    );
    compare(
        &change_set.contract_registry_hash,
        &current.contract_registry_hash,
        StaleReason::ContractRegistryChanged,
        &mut reasons,
    );
    compare(
        &change_set.analyzer_versions,
        &current.analyzer_versions,
        StaleReason::AnalyzerVersionsChanged,
        &mut reasons,
    );
    reasons.sort_unstable();
    reasons.dedup();
    if reasons.is_empty() {
        ChangeValidity::Valid
    } else {
        ChangeValidity::Stale {
            reasons,
            remediation: "Recollect the change set and rerun analysis before committing."
                .to_owned(),
        }
    }
}

/// Git commit selection semantics modeled without executing a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitSelection {
    /// Default `git commit`: commit the index exactly as represented.
    Default,
    /// `git commit -a`: stage and commit all tracked modifications and deletions.
    AllTracked,
    /// `git commit --only -- <pathspecs>`: commit only selected worktree paths.
    Only {
        /// Validated literal repository-relative pathspecs.
        pathspecs: Vec<String>,
    },
    /// `git commit --include -- <pathspecs>`: add selected paths to the existing index.
    Include {
        /// Validated literal repository-relative pathspecs.
        pathspecs: Vec<String>,
    },
}

/// Commit command intent evaluated against an analyzed change set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommitIntent {
    /// Selection behavior requested by the caller.
    pub selection: CommitSelection,
}

/// Explicit file/layer subset a modeled commit would select.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommitFileSelection {
    /// Resulting path, or the old path for a deletion.
    pub path: NativePath,
    /// Included source layers.
    pub layers: Vec<ChangeSourceLayer>,
}

/// Safety decision for a modeled commit command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommitGate {
    /// Whether the selected subset is explicit and unambiguous.
    pub allowed: bool,
    /// Explicit selected paths and layers.
    pub selected: Vec<CommitFileSelection>,
    /// Reasons the gate failed closed.
    pub reasons: Vec<String>,
}

/// Evaluates commit selection semantics without mutating Git state.
///
/// # Errors
///
/// Returns [`ChangeError::InvalidPathspec`] when a pathspec could be interpreted as an option,
/// revision expression, pathspec magic, glob, absolute path, or control-bearing input.
pub fn evaluate_commit_gate(
    change_set: &ChangeSet,
    intent: &CommitIntent,
) -> Result<CommitGate, ChangeError> {
    let pathspecs = match &intent.selection {
        CommitSelection::Only { pathspecs } | CommitSelection::Include { pathspecs } => {
            for pathspec in pathspecs {
                validate_pathspec(pathspec)?;
            }
            Some(pathspecs.as_slice())
        }
        CommitSelection::Default | CommitSelection::AllTracked => None,
    };
    let mut selected = BTreeMap::<NativePath, BTreeSet<ChangeSourceLayer>>::new();
    for file in &change_set.files {
        let Some(path) = effective_path(file) else {
            continue;
        };
        let matched = pathspecs.is_none_or(|specs| {
            specs
                .iter()
                .any(|spec| literal_pathspec_matches(spec, &path.bytes))
        });
        let include = match intent.selection {
            CommitSelection::Default => file.source == ChangeSourceLayer::Staged,
            CommitSelection::AllTracked => {
                matches!(
                    file.source,
                    ChangeSourceLayer::Staged | ChangeSourceLayer::Worktree
                )
            }
            CommitSelection::Only { .. } => {
                matched
                    && matches!(
                        file.source,
                        ChangeSourceLayer::Staged
                            | ChangeSourceLayer::Worktree
                            | ChangeSourceLayer::Untracked
                    )
            }
            CommitSelection::Include { .. } => {
                file.source == ChangeSourceLayer::Staged
                    || (matched
                        && matches!(
                            file.source,
                            ChangeSourceLayer::Worktree | ChangeSourceLayer::Untracked
                        ))
            }
        };
        if include {
            selected
                .entry(path.clone())
                .or_default()
                .insert(file.source);
        }
    }

    let mut reasons = Vec::new();
    if selected.is_empty() {
        reasons.push("The modeled command selects no analyzed changes.".to_owned());
    }
    if matches!(intent.selection, CommitSelection::Only { .. })
        && selected.values().any(|layers| layers.len() > 1)
    {
        reasons.push(
            "`--only` overlaps staged and worktree layers; the exact committed blob is ambiguous."
                .to_owned(),
        );
    }
    let selected = selected
        .into_iter()
        .map(|(path, layers)| CommitFileSelection {
            path,
            layers: layers.into_iter().collect(),
        })
        .collect();
    Ok(CommitGate {
        allowed: reasons.is_empty(),
        selected,
        reasons,
    })
}

/// Failures produced by local Git change discovery and commit modeling.
#[derive(Debug, Error)]
pub enum ChangeError {
    /// Pull-request scopes require an external provider.
    #[error("the local Git provider does not support pull-request scopes")]
    PullRequestUnsupported,
    /// A ref contains revision-expression or option syntax.
    #[error("invalid Git ref `{0}`")]
    InvalidRef(String),
    /// A commit ID is not a full hexadecimal SHA-1 or SHA-256 object ID.
    #[error("invalid full commit SHA `{0}`")]
    InvalidSha(String),
    /// A pathspec is unsafe or cannot be modeled literally.
    #[error("invalid literal pathspec `{0}`")]
    InvalidPathspec(String),
    /// An output budget was configured as zero.
    #[error("Git output limits must be greater than zero")]
    InvalidLimit,
    /// Git could not be started.
    #[error("failed to spawn Git")]
    Spawn {
        /// Underlying process error.
        #[source]
        source: std::io::Error,
    },
    /// A required child pipe was unavailable.
    #[error("Git child process did not expose a required pipe")]
    MissingPipe,
    /// Waiting for Git failed.
    #[error("failed while waiting for Git")]
    Wait {
        /// Underlying process error.
        #[source]
        source: std::io::Error,
    },
    /// Reading bounded process output failed.
    #[error("failed while reading bounded Git output")]
    Read {
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A process reader task failed.
    #[error("Git output reader task failed")]
    ReaderTask {
        /// Tokio task failure.
        #[source]
        source: tokio::task::JoinError,
    },
    /// Git exceeded the configured stream budget.
    #[error("Git {stream} exceeded its {limit}-byte output limit")]
    OutputLimit {
        /// Stream whose budget was exhausted.
        stream: &'static str,
        /// Configured maximum bytes.
        limit: usize,
    },
    /// Git returned a non-success status.
    #[error("Git failed with status {status:?}: {stderr}")]
    GitFailed {
        /// Exit status code, if the platform supplied one.
        status: Option<i32>,
        /// Bounded, lossy diagnostic stderr.
        stderr: String,
    },
    /// The operation exceeded its deadline.
    #[error("Git operation timed out after {timeout:?}")]
    Timeout {
        /// Configured deadline.
        timeout: Duration,
    },
    /// The caller cancelled the operation.
    #[error("Git operation was cancelled")]
    Cancelled,
    /// Git identity metadata was unexpectedly non-UTF-8.
    #[error("Git returned non-UTF-8 identity metadata")]
    NonUtf8Metadata,
    /// A canonical repository path could not be resolved.
    #[error("failed to canonicalize `{path}`")]
    Canonicalize {
        /// Path being resolved.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Git raw output was malformed or inconsistent.
    #[error("malformed bounded Git output: {0}")]
    MalformedGitOutput(String),
    /// Fingerprint identity serialization failed.
    #[error("failed to encode change identity")]
    FingerprintEncoding {
        /// Underlying serialization error.
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Default)]
struct LayerResult {
    files: Vec<ChangedFile>,
    material: Vec<u8>,
}

fn diff_prefix(mode: &str) -> Vec<OsString> {
    vec![
        OsString::from("diff"),
        OsString::from(mode),
        OsString::from("-z"),
        OsString::from("--find-renames"),
        OsString::from("--find-copies"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-color"),
    ]
}

async fn read_bounded<R>(reader: R, limit: usize) -> Result<Vec<u8>, ChangeError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let maximum = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut output = Vec::new();
    reader
        .take(maximum)
        .read_to_end(&mut output)
        .await
        .map_err(|source| ChangeError::Read { source })?;
    if output.len() > limit {
        return Err(ChangeError::OutputLimit {
            stream: "stream",
            limit,
        });
    }
    Ok(output)
}

async fn terminate(child: &mut tokio::process::Child) {
    let _ignored = child.start_kill();
    let _ignored = child.wait().await;
}

fn canonicalize(path: &Path) -> Result<PathBuf, ChangeError> {
    std::fs::canonicalize(path).map_err(|source| ChangeError::Canonicalize {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_scope(scope: &ChangeScope) -> Result<(), ChangeError> {
    match scope {
        ChangeScope::Compare { reference } => validate_ref(reference),
        ChangeScope::Commit { sha } => validate_sha(sha),
        ChangeScope::Range { base, head } => {
            validate_ref(base)?;
            validate_ref(head)
        }
        ChangeScope::PullRequest { provider, .. } => {
            if provider.is_empty() || provider.chars().any(char::is_control) {
                return Err(ChangeError::InvalidRef(provider.clone()));
            }
            Err(ChangeError::PullRequestUnsupported)
        }
        ChangeScope::Unstaged | ChangeScope::Staged | ChangeScope::All => Ok(()),
    }
}

fn validate_ref(reference: &str) -> Result<(), ChangeError> {
    let valid_chars = reference
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte));
    let invalid = reference.is_empty()
        || reference.starts_with('-')
        || reference.starts_with('/')
        || reference.ends_with('/')
        || reference.contains("..")
        || reference.contains("//")
        || reference.contains("@{")
        || reference.ends_with('.')
        || Path::new(reference)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lock"))
        || !valid_chars;
    if invalid {
        Err(ChangeError::InvalidRef(reference.to_owned()))
    } else {
        Ok(())
    }
}

fn validate_sha(sha: &str) -> Result<(), ChangeError> {
    if matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ChangeError::InvalidSha(sha.to_owned()))
    }
}

fn validate_pathspec(pathspec: &str) -> Result<(), ChangeError> {
    let path = Path::new(pathspec);
    let invalid = pathspec.is_empty()
        || pathspec.starts_with('-')
        || pathspec.starts_with(":(")
        || pathspec.contains("..")
        || pathspec.contains(['*', '?', '[', ']'])
        || pathspec.chars().any(char::is_control)
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir));
    if invalid {
        Err(ChangeError::InvalidPathspec(pathspec.to_owned()))
    } else {
        Ok(())
    }
}

fn parse_raw(raw: &[u8], source: ChangeSourceLayer) -> Result<Vec<ChangedFile>, ChangeError> {
    let mut files = Vec::new();
    let mut cursor = 0;
    while cursor < raw.len() {
        let header = take_nul(raw, &mut cursor)?;
        if header.is_empty() {
            continue;
        }
        if header.first() != Some(&b':') {
            return Err(ChangeError::MalformedGitOutput(
                "raw record does not start with ':'".to_owned(),
            ));
        }
        let status_byte = header
            .split(u8::is_ascii_whitespace)
            .next_back()
            .and_then(|field| field.first())
            .copied()
            .ok_or_else(|| ChangeError::MalformedGitOutput("missing raw status".to_owned()))?;
        let first_path = take_nul(raw, &mut cursor)?;
        let (status, old_path, new_path) = match status_byte {
            b'A' => (ChangedFileStatus::Added, None, Some(first_path)),
            b'M' | b'T' | b'U' => (
                ChangedFileStatus::Modified,
                Some(first_path),
                Some(first_path),
            ),
            b'D' => (ChangedFileStatus::Deleted, Some(first_path), None),
            b'R' => (
                ChangedFileStatus::Renamed,
                Some(first_path),
                Some(take_nul(raw, &mut cursor)?),
            ),
            b'C' => (
                ChangedFileStatus::Copied,
                Some(first_path),
                Some(take_nul(raw, &mut cursor)?),
            ),
            other => {
                return Err(ChangeError::MalformedGitOutput(format!(
                    "unsupported raw status `{}`",
                    char::from(other)
                )));
            }
        };
        files.push(ChangedFile {
            status,
            old_path: old_path.map(native_path_bytes),
            new_path: new_path.map(native_path_bytes),
            binary: false,
            hunks: Vec::new(),
            source,
        });
    }
    Ok(files)
}

fn take_nul<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], ChangeError> {
    let remaining = bytes
        .get(*cursor..)
        .ok_or_else(|| ChangeError::MalformedGitOutput("cursor exceeds output".to_owned()))?;
    let length = remaining
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| ChangeError::MalformedGitOutput("missing NUL delimiter".to_owned()))?;
    let value = &remaining[..length];
    *cursor = cursor.saturating_add(length).saturating_add(1);
    Ok(value)
}

fn apply_numstat(numstat: &[u8], files: &mut [ChangedFile]) -> Result<(), ChangeError> {
    let mut cursor = 0;
    let mut index = 0;
    while cursor < numstat.len() {
        let record = take_nul(numstat, &mut cursor)?;
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let added = fields.next();
        let removed = fields.next();
        let path = fields.next();
        let (Some(added), Some(removed), Some(path)) = (added, removed, path) else {
            return Err(ChangeError::MalformedGitOutput(
                "invalid numstat record".to_owned(),
            ));
        };
        let file = files.get_mut(index).ok_or_else(|| {
            ChangeError::MalformedGitOutput("numstat has too many records".to_owned())
        })?;
        file.binary = added == b"-" || removed == b"-";
        if path.is_empty() {
            let _old = take_nul(numstat, &mut cursor)?;
            let _new = take_nul(numstat, &mut cursor)?;
        }
        index = index.saturating_add(1);
    }
    if index != files.len() {
        return Err(ChangeError::MalformedGitOutput(
            "raw and numstat record counts differ".to_owned(),
        ));
    }
    Ok(())
}

fn apply_patch(patch: &[u8], files: &mut [ChangedFile]) -> Result<(), ChangeError> {
    let mut file_index: Option<usize> = None;
    let mut current_hunk: Option<ChangeHunk> = None;
    let mut old_line = 0_u32;
    let mut new_line = 0_u32;
    for line in patch.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"diff --git ") {
            finish_hunk(files, file_index, current_hunk.take())?;
            file_index = Some(file_index.map_or(0, |index| index.saturating_add(1)));
            continue;
        }
        if line.starts_with(b"@@ ") {
            finish_hunk(files, file_index, current_hunk.take())?;
            let hunk = parse_hunk_header(line)?;
            old_line = hunk.old_start;
            new_line = hunk.new_start;
            current_hunk = Some(hunk);
            continue;
        }
        let Some(hunk) = current_hunk.as_mut() else {
            continue;
        };
        if line.starts_with(b"+") && !line.starts_with(b"+++") {
            hunk.lines.push(ChangedLine {
                kind: ChangedLineKind::Added,
                old_line: None,
                new_line: Some(new_line),
            });
            new_line = new_line.saturating_add(1);
        } else if line.starts_with(b"-") && !line.starts_with(b"---") {
            hunk.lines.push(ChangedLine {
                kind: ChangedLineKind::Removed,
                old_line: Some(old_line),
                new_line: None,
            });
            old_line = old_line.saturating_add(1);
        } else if line.starts_with(b" ") {
            old_line = old_line.saturating_add(1);
            new_line = new_line.saturating_add(1);
        }
    }
    finish_hunk(files, file_index, current_hunk)?;
    if file_index.is_some_and(|index| index >= files.len()) {
        return Err(ChangeError::MalformedGitOutput(
            "patch has more file sections than raw output".to_owned(),
        ));
    }
    Ok(())
}

fn finish_hunk(
    files: &mut [ChangedFile],
    file_index: Option<usize>,
    hunk: Option<ChangeHunk>,
) -> Result<(), ChangeError> {
    if let Some(hunk) = hunk {
        let index = file_index.ok_or_else(|| {
            ChangeError::MalformedGitOutput("hunk precedes file header".to_owned())
        })?;
        files
            .get_mut(index)
            .ok_or_else(|| {
                ChangeError::MalformedGitOutput("patch file index exceeds raw output".to_owned())
            })?
            .hunks
            .push(hunk);
    }
    Ok(())
}

fn parse_hunk_header(line: &[u8]) -> Result<ChangeHunk, ChangeError> {
    let text = std::str::from_utf8(line)
        .map_err(|_| ChangeError::MalformedGitOutput("non-ASCII hunk header".to_owned()))?;
    let mut fields = text.split_ascii_whitespace();
    if fields.next() != Some("@@") {
        return Err(ChangeError::MalformedGitOutput(
            "invalid hunk marker".to_owned(),
        ));
    }
    let old = fields
        .next()
        .ok_or_else(|| ChangeError::MalformedGitOutput("missing old hunk range".to_owned()))?;
    let new = fields
        .next()
        .ok_or_else(|| ChangeError::MalformedGitOutput("missing new hunk range".to_owned()))?;
    let (old_start, old_count) = parse_range(old, '-')?;
    let (new_start, new_count) = parse_range(new, '+')?;
    Ok(ChangeHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

fn parse_range(value: &str, prefix: char) -> Result<(u32, u32), ChangeError> {
    let value = value
        .strip_prefix(prefix)
        .ok_or_else(|| ChangeError::MalformedGitOutput("invalid hunk range prefix".to_owned()))?;
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    let start = start
        .parse()
        .map_err(|_| ChangeError::MalformedGitOutput("invalid hunk start".to_owned()))?;
    let count = count
        .parse()
        .map_err(|_| ChangeError::MalformedGitOutput("invalid hunk count".to_owned()))?;
    Ok((start, count))
}

fn sort_and_deduplicate(files: &mut Vec<ChangedFile>) {
    files.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| effective_path(left).cmp(&effective_path(right)))
            .then_with(|| left.status.cmp(&right.status))
            .then_with(|| left.old_path.cmp(&right.old_path))
    });
    files.dedup();
}

fn effective_path(file: &ChangedFile) -> Option<&NativePath> {
    file.new_path.as_ref().or(file.old_path.as_ref())
}

fn literal_pathspec_matches(pathspec: &str, path: &[u8]) -> bool {
    let spec = pathspec.trim_end_matches('/').as_bytes();
    path == spec
        || (path.starts_with(spec)
            && path
                .get(spec.len())
                .is_some_and(|separator| *separator == b'/'))
}

fn framed_material(parts: &[&[u8]]) -> Vec<u8> {
    let capacity = parts.iter().map(|part| part.len().saturating_add(8)).sum();
    let mut output = Vec::with_capacity(capacity);
    for part in parts {
        output.extend_from_slice(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        output.extend_from_slice(part);
    }
    output
}

fn hash_material(namespace: &[u8], material: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace);
    hasher.update(&[0]);
    hasher.update(material);
    hasher.finalize().to_hex().to_string()
}

#[expect(
    clippy::too_many_arguments,
    reason = "all identity inputs are fingerprint material"
)]
fn exact_fingerprint(
    request: &ChangeRequest,
    worktree: &NativePath,
    common: &NativePath,
    checkout_head_sha: &str,
    base_ref: Option<&str>,
    head_ref: Option<&str>,
    head_sha: &str,
    materials: &[&[u8]],
) -> Result<String, ChangeError> {
    let identity = serde_json::to_vec(&(
        &request.repo_id,
        &request.checkout_id,
        worktree,
        common,
        &request.scope,
        checkout_head_sha,
        base_ref,
        head_ref,
        head_sha,
        &request.workspace_manifest_hash,
        &request.contract_registry_hash,
        &request.analyzer_versions,
    ))
    .map_err(|source| ChangeError::FingerprintEncoding { source })?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"code-system-graph-exact-diff-v1");
    hasher.update(
        &u64::try_from(identity.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(&identity);
    for material in materials {
        hasher.update(
            &u64::try_from(material.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hasher.update(material);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn compare<T: PartialEq>(stored: &T, current: &T, reason: StaleReason, out: &mut Vec<StaleReason>) {
    if stored != current {
        out.push(reason);
    }
}

#[cfg(unix)]
fn native_path(path: &Path) -> NativePath {
    use std::os::unix::ffi::OsStrExt;

    NativePath {
        encoding: NativePathEncoding::UnixBytes,
        bytes: path.as_os_str().as_bytes().to_vec(),
        display: path.to_string_lossy().into_owned(),
    }
}

#[cfg(windows)]
fn native_path(path: &Path) -> NativePath {
    use std::os::windows::ffi::OsStrExt;

    NativePath {
        encoding: NativePathEncoding::WindowsWide,
        bytes: path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect(),
        display: path.to_string_lossy().into_owned(),
    }
}

#[cfg(not(any(unix, windows)))]
fn native_path(path: &Path) -> NativePath {
    NativePath {
        encoding: NativePathEncoding::Utf8,
        bytes: path.to_string_lossy().as_bytes().to_vec(),
        display: path.to_string_lossy().into_owned(),
    }
}

fn native_path_bytes(bytes: &[u8]) -> NativePath {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let value = OsStr::from_bytes(bytes);
        NativePath {
            encoding: NativePathEncoding::UnixBytes,
            bytes: bytes.to_vec(),
            display: value.to_string_lossy().into_owned(),
        }
    }
    #[cfg(not(unix))]
    {
        NativePath {
            encoding: NativePathEncoding::Utf8,
            bytes: bytes.to_vec(),
            display: String::from_utf8_lossy(bytes).into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command as StdCommand;

    use tempfile::TempDir;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn path(value: &str) -> NativePath {
        native_path_bytes(value.as_bytes())
    }

    fn file(name: &str, source: ChangeSourceLayer) -> ChangedFile {
        ChangedFile {
            status: ChangedFileStatus::Modified,
            old_path: Some(path(name)),
            new_path: Some(path(name)),
            binary: false,
            hunks: Vec::new(),
            source,
        }
    }

    fn sample_set(files: Vec<ChangedFile>) -> ChangeSet {
        ChangeSet {
            repo_id: RepoId::new("repo:test"),
            checkout_id: CheckoutId::new("checkout:test"),
            worktree: path("/tmp/repo"),
            git_common_dir: path("/tmp/repo/.git"),
            scope: ChangeScope::All,
            checkout_head_ref: Some("refs/heads/main".to_owned()),
            checkout_head_sha: "a".repeat(40),
            base_ref: None,
            head_ref: Some("HEAD".to_owned()),
            head_sha: "a".repeat(40),
            staged_hash: "staged".to_owned(),
            worktree_hash: "worktree".to_owned(),
            exact_diff_fingerprint: "exact".to_owned(),
            workspace_manifest_hash: "manifest".to_owned(),
            contract_registry_hash: "contracts".to_owned(),
            analyzer_versions: BTreeMap::from([("core".to_owned(), "1".to_owned())]),
            files,
        }
    }

    fn git(repo: &Path, args: &[&str]) -> TestResult {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .stdin(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("git failed with {status}").into())
        }
    }

    fn repo() -> Result<TempDir, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        git(temp.path(), &["init", "-q"])?;
        git(temp.path(), &["config", "core.autocrlf", "false"])?;
        git(
            temp.path(),
            &["config", "user.name", "Code System Graph Test"],
        )?;
        git(
            temp.path(),
            &["config", "user.email", "test@example.invalid"],
        )?;
        fs::write(temp.path().join("tracked.txt"), "one\n")?;
        git(temp.path(), &["add", "tracked.txt"])?;
        git(temp.path(), &["commit", "-qm", "initial"])?;
        Ok(temp)
    }

    fn request(repo: &Path, scope: ChangeScope) -> ChangeRequest {
        ChangeRequest {
            repo_id: RepoId::new("repo:test"),
            checkout_id: CheckoutId::new("checkout:test"),
            worktree: repo.to_path_buf(),
            scope,
            workspace_manifest_hash: "manifest".to_owned(),
            contract_registry_hash: "contracts".to_owned(),
            analyzer_versions: BTreeMap::from([("core".to_owned(), "1".to_owned())]),
        }
    }

    async fn collect(repo: &Path, scope: ChangeScope) -> Result<ChangeSet, ChangeError> {
        GitCliChangeProvider::new()
            .changes(&request(repo, scope), &CancellationToken::new())
            .await
    }

    #[test]
    fn ref_validation_accepts_simple_names() {
        assert!(validate_ref("refs/heads/feature-x").is_ok());
    }

    #[test]
    fn ref_validation_rejects_option_injection() {
        assert!(validate_ref("--output=/tmp/pwn").is_err());
    }

    #[test]
    fn ref_validation_rejects_range_injection() {
        assert!(validate_ref("main..evil").is_err());
    }

    #[test]
    fn sha_validation_accepts_full_sha1() {
        assert!(validate_sha(&"a".repeat(40)).is_ok());
    }

    #[test]
    fn sha_validation_rejects_short_object_id() {
        assert!(validate_sha("deadbeef").is_err());
    }

    #[test]
    fn pathspec_validation_rejects_options() {
        assert!(validate_pathspec("-a").is_err());
    }

    #[test]
    fn pathspec_validation_rejects_magic() {
        assert!(validate_pathspec(":(top)src").is_err());
    }

    #[test]
    fn pathspec_matching_includes_directory_children() {
        assert!(literal_pathspec_matches("src", b"src/lib.rs"));
    }

    #[test]
    fn raw_parser_handles_rename() -> TestResult {
        let raw = b":100644 100644 aaaaaaa bbbbbbb R100\0old.rs\0new.rs\0";
        let parsed = parse_raw(raw, ChangeSourceLayer::Staged)?;
        assert_eq!(parsed[0].status, ChangedFileStatus::Renamed);
        Ok(())
    }

    #[test]
    fn raw_parser_preserves_non_utf8_path() -> TestResult {
        let raw = b":000000 100644 0000000 bbbbbbb A\0bad-\xff\0";
        let parsed = parse_raw(raw, ChangeSourceLayer::Staged)?;
        assert_eq!(
            parsed[0]
                .new_path
                .as_ref()
                .map(|value| value.bytes.as_slice()),
            Some(&b"bad-\xff"[..])
        );
        Ok(())
    }

    #[test]
    fn numstat_marks_binary() -> TestResult {
        let mut files = vec![file("image.bin", ChangeSourceLayer::Staged)];
        apply_numstat(b"-\t-\timage.bin\0", &mut files)?;
        assert!(files[0].binary);
        Ok(())
    }

    #[test]
    fn patch_parser_records_positions_without_text() -> TestResult {
        let mut files = vec![file("a.txt", ChangeSourceLayer::Worktree)];
        apply_patch(
            b"diff --git a.txt a.txt\n@@ -1 +1,2 @@\n-old secret\n+new secret\n+second\n",
            &mut files,
        )?;
        assert_eq!(files[0].hunks[0].lines.len(), 3);
        Ok(())
    }

    #[test]
    fn validity_is_valid_for_identical_input() {
        let set = sample_set(Vec::new());
        assert_eq!(
            validate_change_set(&set, &ChangeValidityInput::from(&set)),
            ChangeValidity::Valid
        );
    }

    #[test]
    fn validity_reports_manifest_change() {
        let set = sample_set(Vec::new());
        let mut input = ChangeValidityInput::from(&set);
        input.workspace_manifest_hash = "new".to_owned();
        assert!(matches!(
            validate_change_set(&set, &input),
            ChangeValidity::Stale { reasons, .. }
                if reasons == vec![StaleReason::WorkspaceManifestChanged]
        ));
    }

    #[test]
    fn default_commit_gate_selects_only_staged() -> TestResult {
        let set = sample_set(vec![
            file("a", ChangeSourceLayer::Staged),
            file("b", ChangeSourceLayer::Worktree),
        ]);
        let gate = evaluate_commit_gate(
            &set,
            &CommitIntent {
                selection: CommitSelection::Default,
            },
        )?;
        assert_eq!(gate.selected.len(), 1);
        Ok(())
    }

    #[test]
    fn all_tracked_commit_gate_excludes_untracked() -> TestResult {
        let set = sample_set(vec![
            file("a", ChangeSourceLayer::Worktree),
            file("b", ChangeSourceLayer::Untracked),
        ]);
        let gate = evaluate_commit_gate(
            &set,
            &CommitIntent {
                selection: CommitSelection::AllTracked,
            },
        )?;
        assert_eq!(gate.selected.len(), 1);
        Ok(())
    }

    #[test]
    fn only_commit_gate_fails_on_layer_ambiguity() -> TestResult {
        let set = sample_set(vec![
            file("a", ChangeSourceLayer::Staged),
            file("a", ChangeSourceLayer::Worktree),
        ]);
        let gate = evaluate_commit_gate(
            &set,
            &CommitIntent {
                selection: CommitSelection::Only {
                    pathspecs: vec!["a".to_owned()],
                },
            },
        )?;
        assert!(!gate.allowed);
        Ok(())
    }

    #[test]
    fn include_commit_gate_keeps_existing_index() -> TestResult {
        let set = sample_set(vec![
            file("staged", ChangeSourceLayer::Staged),
            file("src/new", ChangeSourceLayer::Worktree),
        ]);
        let gate = evaluate_commit_gate(
            &set,
            &CommitIntent {
                selection: CommitSelection::Include {
                    pathspecs: vec!["src".to_owned()],
                },
            },
        )?;
        assert_eq!(gate.selected.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn unstaged_scope_reports_tracked_modification() -> TestResult {
        let repo = repo()?;
        fs::write(repo.path().join("tracked.txt"), "two\n")?;
        let set = collect(repo.path(), ChangeScope::Unstaged).await?;
        assert_eq!(set.files[0].source, ChangeSourceLayer::Worktree);
        Ok(())
    }

    #[tokio::test]
    async fn staged_scope_reports_index_addition() -> TestResult {
        let repo = repo()?;
        fs::write(repo.path().join("added.txt"), "added\n")?;
        git(repo.path(), &["add", "added.txt"])?;
        let set = collect(repo.path(), ChangeScope::Staged).await?;
        assert_eq!(set.files[0].status, ChangedFileStatus::Added);
        Ok(())
    }

    #[tokio::test]
    async fn staged_scope_supports_an_unborn_checkout() -> TestResult {
        let repo = tempfile::tempdir()?;
        git(repo.path(), &["init", "-q"])?;
        fs::write(repo.path().join("first.txt"), "first commit\n")?;
        git(repo.path(), &["add", "first.txt"])?;

        let set = collect(repo.path(), ChangeScope::Staged).await?;

        assert!(
            set.checkout_head_ref
                .as_deref()
                .is_some_and(|head| head.starts_with("refs/heads/"))
        );
        assert_eq!(set.checkout_head_sha, set.head_sha);
        assert!(matches!(set.checkout_head_sha.len(), 40 | 64));
        assert_eq!(set.files.len(), 1);
        assert_eq!(set.files[0].status, ChangedFileStatus::Added);
        assert_eq!(set.files[0].source, ChangeSourceLayer::Staged);
        Ok(())
    }

    #[tokio::test]
    async fn all_scope_keeps_layers_distinct() -> TestResult {
        let repo = repo()?;
        fs::write(repo.path().join("tracked.txt"), "staged\n")?;
        git(repo.path(), &["add", "tracked.txt"])?;
        fs::write(repo.path().join("tracked.txt"), "worktree\n")?;
        fs::write(repo.path().join("untracked.txt"), "untracked body\n")?;
        let set = collect(repo.path(), ChangeScope::All).await?;
        assert_eq!(set.files.len(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn delete_is_classified() -> TestResult {
        let repo = repo()?;
        fs::remove_file(repo.path().join("tracked.txt"))?;
        let set = collect(repo.path(), ChangeScope::Unstaged).await?;
        assert_eq!(set.files[0].status, ChangedFileStatus::Deleted);
        Ok(())
    }

    #[tokio::test]
    async fn rename_is_classified() -> TestResult {
        let repo = repo()?;
        git(repo.path(), &["mv", "tracked.txt", "renamed.txt"])?;
        let set = collect(repo.path(), ChangeScope::Staged).await?;
        assert_eq!(set.files[0].status, ChangedFileStatus::Renamed);
        Ok(())
    }

    #[tokio::test]
    async fn compare_scope_uses_merge_base() -> TestResult {
        let repo = repo()?;
        let initial = String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?;
        fs::write(repo.path().join("later.txt"), "later\n")?;
        git(repo.path(), &["add", "later.txt"])?;
        git(repo.path(), &["commit", "-qm", "later"])?;
        let set = collect(
            repo.path(),
            ChangeScope::Compare {
                reference: initial.trim().to_owned(),
            },
        )
        .await?;
        assert_eq!(set.files.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn commit_scope_reports_one_commit() -> TestResult {
        let repo = repo()?;
        fs::write(repo.path().join("later.txt"), "later\n")?;
        git(repo.path(), &["add", "later.txt"])?;
        git(repo.path(), &["commit", "-qm", "later"])?;
        let sha = String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?;
        let set = collect(
            repo.path(),
            ChangeScope::Commit {
                sha: sha.trim().to_owned(),
            },
        )
        .await?;
        assert_eq!(set.files.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn range_scope_reports_endpoint_difference() -> TestResult {
        let repo = repo()?;
        let base = String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?;
        fs::write(repo.path().join("later.txt"), "later\n")?;
        git(repo.path(), &["add", "later.txt"])?;
        git(repo.path(), &["commit", "-qm", "later"])?;
        let head = String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?;
        let set = collect(
            repo.path(),
            ChangeScope::Range {
                base: base.trim().to_owned(),
                head: head.trim().to_owned(),
            },
        )
        .await?;
        assert_eq!(set.files.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn pull_request_scope_is_rejected_locally() -> TestResult {
        let repo = repo()?;
        let result = collect(
            repo.path(),
            ChangeScope::PullRequest {
                provider: "github".to_owned(),
                number: 1,
            },
        )
        .await;
        assert!(matches!(result, Err(ChangeError::PullRequestUnsupported)));
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_request_fails_before_spawn() -> TestResult {
        let repo = repo()?;
        let token = CancellationToken::new();
        token.cancel();
        let result = GitCliChangeProvider::new()
            .changes(&request(repo.path(), ChangeScope::All), &token)
            .await;
        assert!(matches!(result, Err(ChangeError::Cancelled)));
        Ok(())
    }

    #[tokio::test]
    async fn output_cap_is_enforced() -> TestResult {
        let repo = repo()?;
        fs::write(repo.path().join("large.txt"), "x".repeat(4096))?;
        git(repo.path(), &["add", "large.txt"])?;
        let provider = GitCliChangeProvider::with_limits("git", Duration::from_secs(5), 64, 1024);
        let result = provider
            .changes(
                &request(repo.path(), ChangeScope::Staged),
                &CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(ChangeError::OutputLimit { .. })));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn linked_worktree_uses_shared_common_directory() -> TestResult {
        let repo = repo()?;
        let linked_parent = tempfile::tempdir()?;
        let linked = linked_parent.path().join("linked");
        let linked_text = linked.to_string_lossy().into_owned();
        git(
            repo.path(),
            &["worktree", "add", "-q", "-b", "linked-test", &linked_text],
        )?;
        let set = collect(&linked, ChangeScope::All).await?;
        assert_ne!(set.worktree.bytes, set.git_common_dir.bytes);
        Ok(())
    }
}
