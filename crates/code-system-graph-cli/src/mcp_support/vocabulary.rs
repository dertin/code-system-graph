//! Human-readable vocabulary shared by semantic MCP views and Markdown.

use code_system_graph_model::{
    EdgeKind, EpistemicStatus, NodeKind, OverallFreshness, RepoFreshnessState
};

pub(super) const fn relationship_phrase(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "contains",
        EdgeKind::Provides => "provides",
        EdgeKind::Consumes => "consumes",
        EdgeKind::CallsRemote => "calls remotely",
        EdgeKind::Publishes => "publishes",
        EdgeKind::Subscribes => "subscribes to",
        EdgeKind::DeliversTo => "delivers to",
        EdgeKind::DependsOnPackage => "depends on package",
        EdgeKind::DependsOnRepository => "depends on repository",
        EdgeKind::ReadsTable => "reads table",
        EdgeKind::WritesTable => "writes table",
        EdgeKind::Deploys => "deploys",
        EdgeKind::Configures => "configures",
        EdgeKind::Documents => "documents",
        EdgeKind::OwnedBy => "is owned by",
        EdgeKind::ImplementedBy => "is implemented by",
        EdgeKind::Validates => "validates",
        EdgeKind::ChangedIn => "was changed in",
        EdgeKind::Affects => "affects",
        EdgeKind::Precedes => "precedes",
        EdgeKind::Reverts => "reverts",
        EdgeKind::CompatibleWith => "is compatible with",
        EdgeKind::IncompatibleWith => "is incompatible with",
        EdgeKind::MemberOf => "belongs to",
        EdgeKind::ManualLink => "is manually linked to",
    }
}

pub(super) const fn inverse_relationship_phrase(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "is contained by",
        EdgeKind::Provides => "is provided by",
        EdgeKind::Consumes => "is consumed by",
        EdgeKind::CallsRemote => "is called remotely by",
        EdgeKind::Publishes => "is published by",
        EdgeKind::Subscribes => "has subscriber",
        EdgeKind::DeliversTo => "receives deliveries from",
        EdgeKind::DependsOnPackage => "is required by",
        EdgeKind::DependsOnRepository => "is a dependency of",
        EdgeKind::ReadsTable => "is read by",
        EdgeKind::WritesTable => "is written by",
        EdgeKind::Deploys => "is deployed by",
        EdgeKind::Configures => "is configured by",
        EdgeKind::Documents => "is documented by",
        EdgeKind::OwnedBy => "owns",
        EdgeKind::ImplementedBy => "implements",
        EdgeKind::Validates => "is validated by",
        EdgeKind::ChangedIn => "contains change to",
        EdgeKind::Affects => "is affected by",
        EdgeKind::Precedes => "is preceded by",
        EdgeKind::Reverts => "is reverted by",
        EdgeKind::CompatibleWith => "is compatible with",
        EdgeKind::IncompatibleWith => "is incompatible with",
        EdgeKind::MemberOf => "has member",
        EdgeKind::ManualLink => "is manually linked from",
    }
}

pub(super) const fn node_kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Repository => "repository",
        NodeKind::Service => "service",
        NodeKind::Package => "package",
        NodeKind::Artifact => "artifact",
        NodeKind::SymbolRef => "symbol",
        NodeKind::TestCase => "test",
        NodeKind::HttpOperation => "HTTP operation",
        NodeKind::GraphqlOperation => "GraphQL operation",
        NodeKind::RpcMethod => "RPC method",
        NodeKind::EventChannel => "event channel",
        NodeKind::EventSchema => "event schema",
        NodeKind::Database => "database",
        NodeKind::DatabaseTable => "database table",
        NodeKind::DatabaseColumn => "database column",
        NodeKind::ConfigKey => "configuration key",
        NodeKind::Deployment => "deployment",
        NodeKind::Document => "document",
        NodeKind::Adr => "architecture decision",
        NodeKind::Owner => "owner",
        NodeKind::ChangeSet => "change set",
        NodeKind::PullRequest => "pull request",
        NodeKind::Community => "community",
    }
}

pub(super) const fn freshness_name(freshness: OverallFreshness) -> &'static str {
    match freshness {
        OverallFreshness::Fresh => "fresh",
        OverallFreshness::Stale => "stale",
        OverallFreshness::Partial => "partial",
        OverallFreshness::Unknown => "unknown",
    }
}

pub(super) const fn freshness_state_name(state: RepoFreshnessState) -> &'static str {
    match state {
        RepoFreshnessState::Fresh => "fresh",
        RepoFreshnessState::WorkingTreeChanged => "working tree changed",
        RepoFreshnessState::CommitsBehind => "commits behind",
        RepoFreshnessState::ConfigChanged => "configuration changed",
        RepoFreshnessState::ExtractorChanged => "extractor changed",
        RepoFreshnessState::CodegraphPending => "CodeGraph pending",
        RepoFreshnessState::Partial => "partial",
        RepoFreshnessState::Corrupt => "corrupt",
        RepoFreshnessState::Unknown => "unknown",
        RepoFreshnessState::Unavailable => "unavailable",
    }
}

pub(super) const fn epistemic_status_name(status: EpistemicStatus) -> &'static str {
    match status {
        EpistemicStatus::Confirmed => "confirmed",
        EpistemicStatus::Inferred => "inferred",
        EpistemicStatus::Ambiguous => "ambiguous",
        EpistemicStatus::Stale => "stale",
        EpistemicStatus::Incomplete => "incomplete",
    }
}
