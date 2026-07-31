use std::path::PathBuf;

use code_system_graph_model::RepoId;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{
    AffectedTestsRequest, LocalCodeIntelligenceProvider, ProviderBudget, ProviderCapability, ProviderRequest, ProviderStatus, ResolveSymbolsRequest
};

/// Exact source symbol anchor selected by a focused extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolAnchor {
    /// Extracted symbol name or qualified name.
    pub symbol: String,
    /// Repository-relative source path.
    pub source_path: String,
    /// One-based source line.
    pub start_line: usize,
}

/// Provider corroboration outcome for one source symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum SymbolCorroboration {
    /// Exactly one provider symbol matched the extracted name, path, and line.
    Confirmed {
        /// Extracted source symbol.
        symbol: String,
        /// Repository-relative source path.
        source_path: String,
        /// Provider-local identifier retained only as evidence metadata.
        local_id: Option<String>,
    },
    /// No exact provider symbol matched all extracted coordinates.
    Unresolved {
        /// Extracted source symbol.
        symbol: String,
        /// Bounded reason.
        reason: String,
    },
    /// More than one exact provider symbol matched.
    Ambiguous {
        /// Extracted source symbol.
        symbol: String,
        /// Number of exact candidates.
        candidate_count: usize,
    },
}

/// Bounded optional `CodeGraph` corroboration for one repository scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorroborationReport {
    /// Exact provider capability report observed before focused requests.
    #[serde(default)]
    pub capability: Option<ProviderCapability>,
    /// Symbol outcomes in input order.
    pub symbols: Vec<SymbolCorroboration>,
    /// Repository-relative tests reported as affected.
    pub affected_tests: Vec<String>,
    /// Explicit degradation messages; an empty result is never interpreted as safety.
    pub degradations: Vec<String>,
}

/// Runs focused, best-effort symbol and affected-test corroboration.
///
/// Provider failures are returned as degradations so factual extraction can continue without
/// treating unavailable local intelligence as negative evidence.
pub async fn corroborate_repository(
    provider: &dyn LocalCodeIntelligenceProvider,
    repo_id: RepoId,
    project_path: PathBuf,
    anchors: &[SymbolAnchor],
    changed_files: &[String],
    budget: ProviderBudget,
    cancellation: CancellationToken,
) -> CorroborationReport {
    let mut symbols = Vec::with_capacity(anchors.len());
    let mut degradations = Vec::new();
    let capability_result = provider
        .probe(ProviderRequest {
            repo_id: repo_id.clone(),
            project_path: project_path.clone(),
            budget,
            cancellation: cancellation.clone(),
        })
        .await;
    let capability = match capability_result {
        Ok(capability) => Some(capability),
        Err(error) => {
            degradations.push(error.to_string());
            None
        }
    };
    if capability
        .as_ref()
        .is_none_or(|capability| capability.status != ProviderStatus::Available)
    {
        if let Some(capability) = &capability {
            degradations.extend(
                capability
                    .degradations
                    .iter()
                    .map(|degradation| degradation.message.clone()),
            );
            degradations.push(format!(
                "{} local intelligence status is {:?}",
                capability.provider, capability.status
            ));
        }
        symbols.extend(
            anchors
                .iter()
                .map(|anchor| SymbolCorroboration::Unresolved {
                    symbol: anchor.symbol.clone(),
                    reason: "local intelligence provider is not available".to_owned(),
                }),
        );
        degradations.sort();
        degradations.dedup();
        return CorroborationReport {
            capability,
            symbols,
            affected_tests: Vec::new(),
            degradations,
        };
    }
    for anchor in anchors {
        let (outcome, observed_degradations) = corroborate_symbol(
            provider,
            &repo_id,
            &project_path,
            anchor,
            budget,
            &cancellation,
        )
        .await;
        symbols.push(outcome);
        degradations.extend(observed_degradations);
    }

    let (affected_tests, affected_degradations) = corroborate_affected_tests(
        provider,
        repo_id,
        project_path,
        changed_files,
        budget,
        cancellation,
    )
    .await;
    degradations.extend(affected_degradations);
    CorroborationReport {
        capability,
        symbols,
        affected_tests,
        degradations,
    }
}

async fn corroborate_symbol(
    provider: &dyn LocalCodeIntelligenceProvider,
    repo_id: &RepoId,
    project_path: &std::path::Path,
    anchor: &SymbolAnchor,
    budget: ProviderBudget,
    cancellation: &CancellationToken,
) -> (SymbolCorroboration, Vec<String>) {
    let request = ProviderRequest {
        repo_id: repo_id.clone(),
        project_path: project_path.to_path_buf(),
        budget,
        cancellation: cancellation.clone(),
    };
    let result = provider
        .resolve_symbols(ResolveSymbolsRequest {
            request,
            query: anchor.symbol.clone(),
        })
        .await;
    let Ok(result) = result else {
        let error = result.err().map_or_else(
            || "local intelligence unavailable".to_owned(),
            |error| error.to_string(),
        );
        return (
            SymbolCorroboration::Unresolved {
                symbol: anchor.symbol.clone(),
                reason: "local intelligence unavailable".to_owned(),
            },
            vec![error],
        );
    };
    let exact = result
        .symbols
        .into_iter()
        .filter(|candidate| {
            symbol_name_matches(candidate, &anchor.symbol)
                && portable_path(&candidate.file_path) == portable_path(&anchor.source_path)
                && candidate.start_line == anchor.start_line
        })
        .collect::<Vec<_>>();
    let outcome = match exact.as_slice() {
        [candidate] => SymbolCorroboration::Confirmed {
            symbol: anchor.symbol.clone(),
            source_path: anchor.source_path.clone(),
            local_id: candidate.local_id.clone(),
        },
        [] => SymbolCorroboration::Unresolved {
            symbol: anchor.symbol.clone(),
            reason: "provider returned no exact name/path/line match".to_owned(),
        },
        candidates => SymbolCorroboration::Ambiguous {
            symbol: anchor.symbol.clone(),
            candidate_count: candidates.len(),
        },
    };
    let degradations = result
        .execution
        .degradations
        .into_iter()
        .map(|degradation| degradation.message)
        .collect();
    (outcome, degradations)
}

async fn corroborate_affected_tests(
    provider: &dyn LocalCodeIntelligenceProvider,
    repo_id: RepoId,
    project_path: PathBuf,
    changed_files: &[String],
    budget: ProviderBudget,
    cancellation: CancellationToken,
) -> (Vec<String>, Vec<String>) {
    if changed_files.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let request = ProviderRequest {
        repo_id,
        project_path,
        budget,
        cancellation,
    };
    match provider
        .get_affected_tests(AffectedTestsRequest {
            request,
            changed_files: changed_files.to_vec(),
            max_depth: 4,
        })
        .await
    {
        Ok(Some(mut result)) => {
            result.affected_tests.sort();
            result.affected_tests.dedup();
            let degradations = result
                .execution
                .degradations
                .into_iter()
                .map(|degradation| degradation.message)
                .collect();
            (result.affected_tests, degradations)
        }
        Ok(None) => (
            Vec::new(),
            vec!["affected-test capability unavailable".to_owned()],
        ),
        Err(error) => (Vec::new(), vec![error.to_string()]),
    }
}

fn symbol_name_matches(candidate: &crate::ResolvedSymbol, anchor: &str) -> bool {
    candidate.name == anchor || candidate.qualified_name.as_deref() == Some(anchor)
}

fn portable_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use code_system_graph_model::RepoId;
    use tokio_util::sync::CancellationToken;

    use super::{SymbolAnchor, SymbolCorroboration, corroborate_repository};
    use crate::{
        AffectedTestsRequest, AffectedTestsResult, LocalCodeIntelligenceProvider, LocalContextRequest, LocalContextResult, LocalImpactRequest, LocalImpactResult, LocalNeighborResult, LocalNeighborsRequest, ProviderBudget, ProviderCapability, ProviderError, ProviderExecution, ProviderStatus, ProviderTransport, ResolveSymbolsRequest, ResolveSymbolsResult, ResolvedSymbol
    };

    struct FakeProvider {
        status: ProviderStatus,
        operation_calls: AtomicUsize,
    }

    impl FakeProvider {
        fn available() -> Self {
            Self {
                status: ProviderStatus::Available,
                operation_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl LocalCodeIntelligenceProvider for FakeProvider {
        fn provider_name(&self) -> &'static str {
            "fake"
        }

        async fn probe(
            &self,
            _request: crate::ProviderRequest,
        ) -> Result<ProviderCapability, ProviderError> {
            Ok(ProviderCapability {
                provider: "fake".to_owned(),
                version: Some("1.0.0".to_owned()),
                status: self.status,
                tools: Vec::new(),
                protocol_version: None,
                operations: Vec::new(),
                degradations: Vec::new(),
                remediation: None,
            })
        }

        async fn resolve_symbols(
            &self,
            input: ResolveSymbolsRequest,
        ) -> Result<ResolveSymbolsResult, ProviderError> {
            self.operation_calls.fetch_add(1, Ordering::Relaxed);
            Ok(ResolveSymbolsResult {
                symbols: vec![ResolvedSymbol {
                    local_id: Some("local:1".to_owned()),
                    name: input.query,
                    qualified_name: None,
                    kind: "function".to_owned(),
                    file_path: "src/routes.rs".to_owned(),
                    start_line: 10,
                    score: Some(1.0),
                }],
                execution: execution(),
            })
        }

        async fn get_local_neighbors(
            &self,
            _input: LocalNeighborsRequest,
        ) -> Result<LocalNeighborResult, ProviderError> {
            unreachable!("neighbors are not used by corroboration")
        }

        async fn get_local_impact(
            &self,
            _input: LocalImpactRequest,
        ) -> Result<LocalImpactResult, ProviderError> {
            unreachable!("impact is not used by corroboration")
        }

        async fn build_local_context(
            &self,
            _input: LocalContextRequest,
        ) -> Result<LocalContextResult, ProviderError> {
            unreachable!("context is not used by corroboration")
        }

        async fn get_affected_tests(
            &self,
            input: AffectedTestsRequest,
        ) -> Result<Option<AffectedTestsResult>, ProviderError> {
            self.operation_calls.fetch_add(1, Ordering::Relaxed);
            Ok(Some(AffectedTestsResult {
                changed_files: input.changed_files,
                affected_tests: vec!["tests/routes.rs".to_owned()],
                total_dependents_traversed: 1,
                execution: execution(),
            }))
        }

        async fn shutdown(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    fn execution() -> ProviderExecution {
        ProviderExecution {
            transport: ProviderTransport::Cli,
            output_bytes: 1,
            truncated: false,
            degradations: Vec::new(),
        }
    }

    #[tokio::test]
    async fn corroboration_should_require_exact_name_path_and_line() {
        let provider = FakeProvider::available();
        let report = corroborate_repository(
            &provider,
            RepoId::new("repo:api"),
            "/repo".into(),
            &[SymbolAnchor {
                symbol: "create_order".to_owned(),
                source_path: "src/routes.rs".to_owned(),
                start_line: 10,
            }],
            &["src/routes.rs".to_owned()],
            ProviderBudget::default(),
            CancellationToken::new(),
        )
        .await;

        assert!(matches!(
            report.symbols.as_slice(),
            [SymbolCorroboration::Confirmed { .. }]
        ));
        assert_eq!(report.affected_tests, vec!["tests/routes.rs"]);
        assert_eq!(provider.operation_calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn unavailable_capability_should_skip_every_focused_operation() {
        let provider = FakeProvider {
            status: ProviderStatus::IndexMissing,
            operation_calls: AtomicUsize::new(0),
        };
        let report = corroborate_repository(
            &provider,
            RepoId::new("repo:api"),
            "/repo".into(),
            &[SymbolAnchor {
                symbol: "create_order".to_owned(),
                source_path: "src/routes.rs".to_owned(),
                start_line: 10,
            }],
            &["src/routes.rs".to_owned()],
            ProviderBudget::default(),
            CancellationToken::new(),
        )
        .await;

        assert!(matches!(
            report.symbols.as_slice(),
            [SymbolCorroboration::Unresolved { .. }]
        ));
        assert!(report.affected_tests.is_empty());
        assert_eq!(provider.operation_calls.load(Ordering::Relaxed), 0);
    }
}
