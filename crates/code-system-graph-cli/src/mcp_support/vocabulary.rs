//! Human-readable vocabulary shared by semantic MCP views and Markdown.

use code_system_graph_model::EdgeKind;

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
