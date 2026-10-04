//! Typed tool variants and common agent-envelope projection.

use code_system_graph_core::{
    ChangeImpactReport, ContractReport, ImpactReport, PullRequestInspection, SearchReport
};
use code_system_graph_model::{FreshnessSummary, ToolEnvelope, ToolStatus, TraceReport};
use serde::Serialize;
use serde_json::{Value, json};

use super::agent_views::{AGENT_DELIVERY_SCHEMA_VERSION, AgentStructuredEnvelope};
use super::{
    AdminAudit, CacheCleanReport, GraphStatusReport, ManifestAdminReport, SourceContextReport
};
use crate::{CommunityReport, ExploreReport, ScanSummary};

#[derive(Clone, Copy)]
pub(crate) enum AgentToolResult<'a> {
    Trace(&'a ToolEnvelope<TraceReport>),
    Query(&'a ToolEnvelope<SearchReport>),
    Explore(&'a ToolEnvelope<ExploreReport>),
    Communities(&'a ToolEnvelope<CommunityReport>),
    Impact(&'a ToolEnvelope<ImpactReport>),
    AnalyzeChanges(&'a ToolEnvelope<ChangeImpactReport>),
    AnalyzePullRequest(&'a ToolEnvelope<PullRequestInspection>),
    Status(&'a ToolEnvelope<GraphStatusReport>),
    Contracts(&'a ToolEnvelope<ContractReport>),
    SourceContext(&'a ToolEnvelope<SourceContextReport>),
    Scan(&'a ToolEnvelope<AdminAudit<ScanSummary>>),
    UpdateWorkspace(&'a ToolEnvelope<AdminAudit<ManifestAdminReport>>),
    WriteManualLink(&'a ToolEnvelope<AdminAudit<ManifestAdminReport>>),
    CleanCache(&'a ToolEnvelope<AdminAudit<CacheCleanReport>>),
    RecomputeCommunities(&'a ToolEnvelope<AdminAudit<ScanSummary>>),
}

impl AgentToolResult<'_> {
    pub(crate) const fn requires_presentation_context(self) -> bool {
        matches!(
            self,
            Self::Trace(_)
                | Self::Query(_)
                | Self::Impact(_)
                | Self::AnalyzeChanges(_)
                | Self::Status(_)
                | Self::Contracts(_)
                | Self::SourceContext(_)
        )
    }

    pub(crate) fn has_data(self) -> bool {
        match self {
            Self::Trace(envelope) => envelope.data.is_some(),
            Self::Query(envelope) => envelope.data.is_some(),
            Self::Explore(envelope) => envelope.data.is_some(),
            Self::Communities(envelope) => envelope.data.is_some(),
            Self::Impact(envelope) => envelope.data.is_some(),
            Self::AnalyzeChanges(envelope) => envelope.data.is_some(),
            Self::AnalyzePullRequest(envelope) => envelope.data.is_some(),
            Self::Status(envelope) => envelope.data.is_some(),
            Self::Contracts(envelope) => envelope.data.as_ref().is_some_and(|report| {
                report.action != code_system_graph_core::ContractAction::List
            }),
            Self::SourceContext(envelope) => envelope.data.is_some(),
            Self::Scan(envelope) | Self::RecomputeCommunities(envelope) => envelope.data.is_some(),
            Self::UpdateWorkspace(envelope) | Self::WriteManualLink(envelope) => {
                envelope.data.is_some()
            }
            Self::CleanCache(envelope) => envelope.data.is_some(),
        }
    }
}

pub(super) fn structured_agent<T: Serialize, U>(
    tool: &str,
    envelope: &ToolEnvelope<U>,
    data: Option<T>,
    context_gap: Option<&str>,
) -> Value {
    let (status, warnings) = structured_metadata(envelope, context_gap);
    let freshness = structured_freshness(tool, &envelope.freshness);
    serialize_value(&AgentStructuredEnvelope::new(
        tool, status, data, freshness, warnings,
    ))
}

pub(super) fn structured_raw<T: Serialize>(tool: &str, envelope: &ToolEnvelope<T>) -> Value {
    structured_raw_with_gap(tool, envelope, None)
}

pub(super) fn structured_raw_with_gap<T: Serialize>(
    tool: &str,
    envelope: &ToolEnvelope<T>,
    context_gap: Option<&str>,
) -> Value {
    let (status, warnings) = structured_metadata(envelope, context_gap);
    let freshness = structured_freshness(tool, &envelope.freshness);
    json!({
        "schema_version": AGENT_DELIVERY_SCHEMA_VERSION,
        "tool": tool,
        "status": status,
        "data": envelope.data,
        "freshness": freshness,
        "warnings": warnings,
    })
}

fn structured_freshness(tool: &str, freshness: &FreshnessSummary) -> FreshnessSummary {
    let mut scoped = freshness.clone();
    if tool != "status" {
        scoped.reasons.clear();
    }
    scoped
}

fn structured_metadata<T>(
    envelope: &ToolEnvelope<T>,
    context_gap: Option<&str>,
) -> (ToolStatus, Vec<String>) {
    let mut warnings = envelope.warnings.clone();
    if let Some(gap) = context_gap {
        warnings.push(gap.to_owned());
    }
    warnings.sort();
    warnings.dedup();
    let status = if context_gap.is_some() && envelope.status == ToolStatus::Ok {
        ToolStatus::Degraded
    } else {
        envelope.status
    };
    (status, warnings)
}

fn serialize_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|error| {
        json!({
            "schema_version": AGENT_DELIVERY_SCHEMA_VERSION,
            "status": "error",
            "data": null,
            "warnings": [format!("structured MCP result could not be serialized: {error}")]
        })
    })
}
