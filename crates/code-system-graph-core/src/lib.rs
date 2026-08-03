//! Application logic and ports for federated repository intelligence.

mod batch;
mod builtin_extractors;
mod capability_dir;
mod change_analysis;
mod changes;
mod codegraph;
mod communities;
mod config;
mod contract_compat;
mod corroboration;
mod data_contracts;
mod documents;
mod event_graph;
mod events;
mod execution_policy;
mod extraction_budget;
mod extraction_graph;
mod extractor;
mod generated_client;
mod graphql_contracts;
mod graphql_graph;
mod http;
mod ignore_policy;
mod impact;
mod incremental;
mod infrastructure;
mod interfaces;
mod linker;
mod manifest;
mod manifest_edit;
mod package_graph;
mod packages;
mod protobuf_contracts;
mod protobuf_graph;
mod provider;
mod pull_requests;
mod query;
mod registry;
mod secret_safety;
mod source_graph;
mod source_http;
mod source_polyglot;
mod source_syntax;
mod test_links;
mod trace;
mod yaml;

pub use batch::{
    ArtifactKey, BatchAction, BatchPlanError, ExtractorBatch, ExtractorBatchPlan, PlannedBatch, affected_link_keys, load_extractor_batch, load_extractor_batch_with_budgets, plan_extractor_batches, store_extractor_batch
};
pub use builtin_extractors::{
    FocusedSourceExtractor, FocusedSourceLanguage, GeneratedClientMetadataExtractor, PackageManifestExtractor, charge_source_observation, precheck_focused_source_values
};
pub use capability_dir::{CapabilityDir, CapabilityError, MAX_REPOSITORY_CONFIG_BYTES};
pub use change_analysis::{
    CHANGE_ANALYZER_VERSION, ChangeAnalysisCoverage, ChangeAnalysisError, ChangeAnalysisOptions, ChangeAnalysisSummary, ChangeConclusion, ChangeImpactReport, ChangedArtifactMapping, ChangedEntity, ChangedEntityRole, ChangedPathSide, ContractCompatibilityInput, EvidenceMatchKind, HunkLineMatch, MappingCompleteness, SemanticContractDelta, analyze_changes, validate_change_analysis
};
pub use changes::{
    AnalyzerVersions, ChangeError, ChangeHunk, ChangeProvider, ChangeRequest, ChangeScope, ChangeSet, ChangeSourceLayer, ChangeValidity, ChangeValidityInput, ChangedFile, ChangedFileStatus, ChangedLine, ChangedLineKind, CommitFileSelection, CommitGate, CommitIntent, CommitSelection, GitCliChangeProvider, StaleReason, evaluate_commit_gate, validate_change_set
};
pub use codegraph::{CodeGraphConfig, CodeGraphProvider};
pub use communities::{
    CommunityError, analyze_communities, analyze_communities_with_progress, compare_community_snapshots
};
pub use config::{
    ConfigError, ConfigSource, EffectiveRepositoryConfig, apply_openapi_override, resolve_repository_config
};
pub use contract_compat::{
    CompatibilityFinding, CompatibilityReport, CompatibilityStatus, compare_database_contracts, compare_event_contracts, compare_graphql_contracts, compare_http_contracts, compare_package_contracts, compare_protobuf_contracts
};
pub use corroboration::{
    CorroborationReport, SymbolAnchor, SymbolCorroboration, corroborate_repository
};
pub use data_contracts::{
    DataAccessObservation, DataAccessRole, DataArtifactKind, DataArtifactReference, DataArtifactReferenceKind, DataDocument, DataEvidenceLine, DataExtractionError, DataFramework, DataOperation, DataWarning, DatabaseColumn, DatabaseForeignKey, DatabaseIndex, DatabaseTable, MigrationMetadata, extract_data_artifact, parse_literal_sql_source, parse_literal_sql_source_at_root
};
pub use documents::{
    DocumentKind, DocumentRecord, DocumentationDocument, DocumentationExtractionError, ExplicitReference, ExplicitReferenceKind, LineEvidence, OwnershipRule, extract_codeowners, extract_markdown, extract_service_catalog
};
pub use event_graph::{EventGraphFacts, event_documents_to_graph};
pub use events::{
    DeliverySemantics, EventBroker, EventDocument, EventEvidenceLine, EventExtractionError, EventObservation, EventRole, EventSchemaDefinition, EventSchemaField, extract_asyncapi, parse_event_source
};
pub use execution_policy::{
    ExecutionLimitExceeded, ExecutionPolicy, ExecutionPolicyOverrides, ExecutionResource, ExecutionSummary, InvalidExecutionPolicy, JobPhase, MonotonicClock, ScanJobTracker
};
pub use extraction_budget::{
    BoundedJsonWriter, EXTRACTION_CONTRACT_VERSION, ExtractionBudgetOverrides, ExtractionBudgets, ExtractionClock, ExtractionLimitExceeded, ExtractionResource, ExtractionTracker, InvalidExtractionBudget
};
pub use extraction_graph::{ExtractionGraphFacts, documents_to_graph};
pub use extractor::{
    BoundaryExtractor, ContentFingerprint, DiscoverContext, DiscoveredInput, ExtractInput, ExtractionBatch, ExtractionCompleteness, ExtractionReport, ExtractorError, FileDescriptor, MAX_EXTRACTOR_INPUT_BYTES, fingerprint_content
};
pub use generated_client::{
    GeneratedClientError, GeneratedClientMetadata, extract_generated_client_metadata
};
pub use graphql_contracts::{
    GraphqlArgumentDefinition, GraphqlDocument, GraphqlExtractionError, GraphqlFederationMetadata, GraphqlFieldDefinition, GraphqlFragment, GraphqlLineRange, GraphqlLiteralKind, GraphqlOperation, GraphqlOperationKind, GraphqlPersistedOperation, GraphqlResolver, GraphqlSelection, GraphqlTypeDefinition, GraphqlTypeKind, GraphqlTypeRef, extract_graphql_document, extract_graphql_document_with_tracker, extract_graphql_persisted_operations, extract_graphql_persisted_operations_with_tracker, parse_graphql_source, parse_graphql_source_with_tracker
};
pub use graphql_graph::{GraphqlGraphFacts, graphql_documents_to_graph};
pub use http::{
    BoundaryRole, HttpBoundary, HttpExtractionError, extract_openapi, extract_openapi_with_tracker, normalize_http_path
};
pub use ignore_policy::{
    DEFAULT_EXCLUDES, IGNORE_POLICY_VERSION, IgnorePatternError, IgnorePolicy, PROTECTED_EXCLUDES, validate_excludes, validate_include_defaults
};
pub use impact::{
    CompatibilityInput, ContractImpact, CoverageSummary, CriticalityAssignment, CriticalityTag, EnvironmentAssignment, ImpactClassification, ImpactCompatibilityStatus, ImpactContext, ImpactDepthBucket, ImpactDirection, ImpactError, ImpactItem, ImpactOptions, ImpactPathStep, ImpactReport, ImpactRequest, ImpactTarget, LocalEnrichmentInput, LocalEnrichmentStatus, LocalImpactItem, LocalImpactSummary, RecommendedCommand, RepositoryImpact, ResolvedTarget, RiskFactor, RiskLevel, ServiceImpact, TestRecommendation, TestRecommendationSource, TruncationInfo, analyze_impact
};
pub use incremental::{IncrementalPlan, plan_incremental_scan};
pub use infrastructure::{
    DeploymentKind, DeploymentUnit, InfrastructureArtifactKind, InfrastructureDocument, InfrastructureEvidence, InfrastructureEvidenceKind, InfrastructureExtractionError, InfrastructurePort, InfrastructureResource, InfrastructureResourceKind, InfrastructureSelector, extract_docker_compose, extract_helm, extract_kubernetes, extract_terraform
};
pub use interfaces::{
    Ambiguity, ConfigDoctorInput, ContractAction, ContractCompatibility, ContractCompatibilitySummary, ContractDifference, ContractFinding, ContractIssue, ContractIssueSeverity, ContractLink, ContractReport, ContractRequest, ContractView, DELIVERY_METADATA_VERSION, DoctorCategory, DoctorCheck, DoctorReport, DoctorRequest, DoctorStatus, DomainErrorKind, EvidenceMetadata, ExitCode, ExportFormat, ExportReport, ExportRequest, FreshnessDoctorInput, INTERFACE_RESULT_VERSION, INTERFACE_SCHEMA_VERSION, IntegrityDoctorInput, InterfaceError, MAX_EXPORT_EDGES, MAX_EXPORT_NODES, NextAction, Page, Pagination, ProviderDoctorInput, ProviderDoctorStatus, PublicSchema, PublicSchemaCatalog, SchemaDoctorInput, Summary, Warning, classify_exit_code, classify_interface_error, doctor, export_graph, inspect_contracts, paginate, public_schema_catalog
};
pub use linker::{
    LinkError, ManualLinkEndpoint, ManualLinkError, ManualLinkResolution, link_http_boundaries, merge_affected_link_neighborhoods, resolve_manual_links
};
pub use manifest::{
    ContractImplementationConfig, HttpConsumerConfig, HttpContractConfig, IntegrationTestConfig, ManifestError, ManualLinkConfig, RepositoryConfig, WorkspaceManifest, parse_manifest, validate_manual_links
};
pub use manifest_edit::{
    ManifestEdit, ManifestEditError, ManifestWriteReport, commit_manifest_edit, preview_add_manual_link, preview_add_repository, preview_remove_repository
};
pub use package_graph::{
    PackageDependencyFact, PackageGraphFacts, PackageIdentity, link_registered_package_owners, package_manifest_to_graph
};
pub use packages::{
    DependencyScope, LockfileMetadata, PackageCoordinate, PackageDependency, PackageEcosystem, PackageEvidenceLine, PackageManifest, PackageManifestError, PackageManifestValue, extract_package_manifest, extract_package_manifest_with_tracker
};
pub use protobuf_contracts::{
    ProtoEnum, ProtoEnumValue, ProtoField, ProtoFieldCardinality, ProtoFile, ProtoGeneratedMarker, ProtoGeneratedRole, ProtoMessage, ProtoRpcMethod, ProtoService, ProtoSyntax, ProtoWireType, ProtobufDocument, ProtobufExtractionError, extract_protobuf, extract_protobuf_with_tracker, parse_protobuf_generated_source
};
pub use protobuf_graph::{ProtobufGraphFacts, protobuf_documents_to_graph};
pub use provider::{
    AffectedTestsRequest, AffectedTestsResult, LocalCodeIntelligenceProvider, LocalContextRequest, LocalContextResult, LocalImpactRequest, LocalImpactResult, LocalNeighbor, LocalNeighborDirection, LocalNeighborResult, LocalNeighborsRequest, ProviderBudget, ProviderCapability, ProviderDegradation, ProviderError, ProviderExecution, ProviderOperation, ProviderOperationCapability, ProviderRequest, ProviderStatus, ProviderTransport, ResolveSymbolsRequest, ResolveSymbolsResult, ResolvedSymbol
};
pub use pull_requests::{
    BitbucketProvider, ChangedFileStatus as PullRequestChangedFileStatus, CheckState, ContractRole, GitHubProvider, PrAuthToken, PrHttpAuthentication, PrHttpMethod, PrHttpRequest, PrHttpResponse, PrHttpTransport, PrHttpTransportError, PullRequestChangedFile, PullRequestCheck, PullRequestCi, PullRequestContractChange, PullRequestCoordinates, PullRequestError, PullRequestInspectRequest, PullRequestInspection, PullRequestListPage, PullRequestListRequest, PullRequestListState, PullRequestMetadata, PullRequestOrder, PullRequestOrderSuggestion, PullRequestOverlap, PullRequestOverlapKind, PullRequestProvider, PullRequestProviderConfig, PullRequestProviderKind, PullRequestRateLimit, PullRequestReadiness, PullRequestRef, PullRequestRepository, PullRequestReview, PullRequestReviewSummary, PullRequestSemanticInput, PullRequestState, PullRequestSummary, PullRequestWarning, ReqwestPrHttpTransport, ReviewState, semantic_pull_request_overlap, suggest_pull_request_order
};
pub use query::{
    EdgeKindCost, PathSegment, PathSegmentScope, QueryError, SearchCoverage, SearchExplanation, SearchFilters, SearchHit, SearchReport, SearchRequest, TraversalAlgorithm, TraversalDirection, TraversalFilters, TraversalLimits, TraversalOptions, TraversalPath, TraversalReport, TraversalRequest, search, traverse
};
pub use registry::{RegisteredWorkspace, RegistryError, encode_native_path, register_workspace};
pub use secret_safety::{
    ConfigArtifactKind, ConfigExtractionError, SafeConfigDocument, SafeConfigKey, SensitiveKeyKind, classify_sensitive_key, extract_safe_config, is_safe_literal_reference
};
pub use source_graph::{SourceGraphFacts, source_observations_to_graph};
pub use source_http::{
    SourceEpistemicStatus, SourceFramework, SourceLanguage, SourceLineRange, SourceObservation, SourceRole, SourceWarning, normalize_source_http_path, parse_python_source, parse_python_source_with_tracker, parse_rust_source, parse_rust_source_with_tracker
};
pub use source_polyglot::{
    parse_go_source, parse_go_source_with_tracker, parse_java_source, parse_java_source_with_tracker, parse_javascript_source, parse_javascript_source_at_path, parse_javascript_source_at_path_with_tracker, parse_typescript_source, parse_typescript_source_at_path, parse_typescript_source_at_path_with_tracker
};
pub use source_syntax::{
    SourceSyntaxError, SourceSyntaxInspection, SourceSyntaxLanguage, inspect_source_syntax
};
pub use test_links::{
    DeclaredImplementation, DeclaredTestCase, declared_implementation, declared_test_case, link_declared_implementations, link_declared_tests
};
pub use trace::{FederatedGraph, TraceError};
