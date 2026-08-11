//! Exhaustive typed MCP tool presentation.

use code_system_graph_core::{
    AgentNextAction, ChangeImpactReport, ContractReport, ImpactReport, PullRequestInspection, SearchReport
};
use code_system_graph_model::{ToolEnvelope, ToolStatus, TraceReport};

use super::{
    AdminAudit, CacheCleanReport, GraphStatusReport, ManifestAdminReport, SourceContextReport
};
use crate::agent_markdown::MarkdownDocument;
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
    pub(crate) fn render(self, maximum: usize) -> (String, bool) {
        match self {
            Self::Trace(envelope) => render_trace(envelope, maximum),
            Self::Query(envelope) => render_query(envelope, maximum),
            Self::Explore(envelope) => render_explore(envelope, maximum),
            Self::Communities(envelope) => render_communities(envelope, maximum),
            Self::Impact(envelope) => render_impact(envelope, maximum),
            Self::AnalyzeChanges(envelope) => render_changes(envelope, maximum),
            Self::AnalyzePullRequest(envelope) => render_pull_request(envelope, maximum),
            Self::Status(envelope) => render_status(envelope, maximum),
            Self::Contracts(envelope) => render_contracts(envelope, maximum),
            Self::SourceContext(envelope) => render_source_context(envelope, maximum),
            Self::Scan(envelope) => render_scan("scan", envelope, maximum),
            Self::UpdateWorkspace(envelope) => {
                render_manifest_admin("update_workspace", envelope, maximum)
            }
            Self::WriteManualLink(envelope) => {
                render_manifest_admin("write_manual_link", envelope, maximum)
            }
            Self::CleanCache(envelope) => render_cache_clean(envelope, maximum),
            Self::RecomputeCommunities(envelope) => {
                render_scan("recompute_communities", envelope, maximum)
            }
        }
    }
}

fn tool_document<T>(
    name: &str,
    envelope: &ToolEnvelope<T>,
    coverage: bool,
    path: Option<&str>,
) -> MarkdownDocument {
    MarkdownDocument::tool(
        name,
        envelope.schema_version,
        envelope.status,
        &envelope.freshness,
        &envelope.warnings,
        coverage && envelope.data.is_some(),
        path,
    )
}

fn finish(document: MarkdownDocument, status: ToolStatus, maximum: usize) -> (String, bool) {
    (document.render(maximum), status == ToolStatus::Error)
}

fn render_trace(envelope: &ToolEnvelope<TraceReport>, maximum: usize) -> (String, bool) {
    let path = envelope.data.as_ref().and_then(|report| {
        report
            .segments
            .first()
            .map(|segment| segment.source.stable_key.as_str())
    });
    let mut document = tool_document("trace", envelope, true, path);
    if let Some(report) = &envelope.data {
        document.debug_collection("Segments", &report.segments);
        document.scalar("Truncated", report.truncated);
        document.string_collection("Coverage", &report.coverage_gaps);
    }
    finish(document, envelope.status, maximum)
}

fn render_query(envelope: &ToolEnvelope<SearchReport>, maximum: usize) -> (String, bool) {
    let mut document = tool_document("query", envelope, true, None);
    if let Some(report) = &envelope.data {
        document.debug_collection("Hits", &report.hits);
        document.scalar("Total matches", report.total_matches);
        document.scalar("Offset", report.offset);
        document.scalar("Limit", report.limit);
        document.scalar("Truncated", report.truncated);
        document.debug("Coverage", &report.coverage);
        render_next_actions(&mut document, &report.next_actions);
    }
    finish(document, envelope.status, maximum)
}

fn render_explore(envelope: &ToolEnvelope<ExploreReport>, maximum: usize) -> (String, bool) {
    let path = envelope.data.as_ref().and_then(|report| {
        report
            .federated_handoffs
            .iter()
            .flat_map(|handoff| &handoff.evidence)
            .next()
            .map(|location| location.path.as_str())
            .or(Some(report.repository.root.as_str()))
    });
    let mut document = tool_document("explore", envelope, true, path);
    if let Some(report) = &envelope.data {
        document.debug("Repository", &report.repository);
        document.debug("Coverage", &report.coverage);
        document.debug_collection("Resolved symbols", &report.resolved_symbols);
        document.debug_collection("Local relationships", &report.local_relationships);
        document.debug_collection("Federated handoffs", &report.federated_handoffs);
        render_next_actions(&mut document, &report.next_actions);
        document.debug("Execution", &report.execution);
        document.source("Source", &report.source_markdown);
    }
    finish(document, envelope.status, maximum)
}

fn render_communities(envelope: &ToolEnvelope<CommunityReport>, maximum: usize) -> (String, bool) {
    let mut document = tool_document("communities", envelope, false, None);
    if let Some(report) = &envelope.data {
        document.text("Snapshot", &report.snapshot_id);
        document.text("Engine version", &report.engine_version);
        document.debug("Configuration", &report.config);
        document.debug_collection("Communities", &report.communities);
        document.scalar("Total communities", report.total_communities);
        document.scalar("Offset", report.offset);
        document.scalar("Limit", report.limit);
        document.scalar("Truncated", report.truncated);
        if let Some(delta) = &report.delta {
            document.debug("Delta", delta);
        }
    }
    finish(document, envelope.status, maximum)
}

fn render_impact(envelope: &ToolEnvelope<ImpactReport>, maximum: usize) -> (String, bool) {
    let mut document = tool_document("impact", envelope, true, None);
    if let Some(report) = &envelope.data {
        document.text("Risk model version", &report.risk_model_version);
        document.debug("Target", &report.target);
        document.debug("Direction", &report.direction);
        document.debug("Risk", &report.risk);
        document.debug("Risk score", &report.risk_score);
        document.debug_collection("Reasons", &report.reasons);
        document.debug_collection("Direct consumers", &report.direct_consumers);
        document.debug_collection("Transitive consumers", &report.transitive_consumers);
        document.debug_collection("Possibly affected", &report.possibly_affected);
        document.debug_collection("Unknown due to coverage", &report.unknown_due_to_coverage);
        document.debug_collection("Affected repositories", &report.affected_repositories);
        document.debug_collection("Affected services", &report.affected_services);
        document.debug_collection("Affected contracts", &report.affected_contracts);
        document.debug_collection("Affected communities", &report.affected_communities);
        document.debug_collection("Local impact summaries", &report.local_impact_summaries);
        document.debug_collection("Test recommendations", &report.test_recommendations);
        document.debug_collection("Depth buckets", &report.depth_buckets);
        document.debug("Coverage", &report.coverage);
        document.debug("Truncation", &report.truncation);
    }
    finish(document, envelope.status, maximum)
}

fn render_changes(envelope: &ToolEnvelope<ChangeImpactReport>, maximum: usize) -> (String, bool) {
    let mut document = tool_document("analyze_changes", envelope, true, None);
    if let Some(report) = &envelope.data {
        document.text("Analyzer version", &report.analyzer_version);
        document.text("Analyzer fingerprint", &report.analyzer_fingerprint);
        document.debug("Change set", &report.change_set);
        document.debug_collection("Mappings", &report.mappings);
        document.debug_collection("Changed entities", &report.changed_entities);
        document.debug_collection("Contract deltas", &report.contract_deltas);
        document.debug_collection("Impacts", &report.impacts);
        document.debug_collection("Touched repositories", &report.touched_repositories);
        document.debug_collection("Touched services", &report.touched_services);
        document.debug_collection("Touched communities", &report.touched_communities);
        document.debug_collection("Touched contracts", &report.touched_contracts);
        document.debug("Coverage", &report.coverage);
        document.debug("Summary", &report.summary);
    }
    finish(document, envelope.status, maximum)
}

fn render_pull_request(
    envelope: &ToolEnvelope<PullRequestInspection>,
    maximum: usize,
) -> (String, bool) {
    let path = envelope
        .data
        .as_ref()
        .and_then(|report| report.changed_files.first())
        .and_then(|file| file.new_path.as_deref().or(file.old_path.as_deref()));
    let mut document = tool_document("analyze_pull_request", envelope, false, path);
    if let Some(report) = &envelope.data {
        document.debug("Coordinates", &report.coordinates);
        document.debug("Metadata", &report.metadata);
        document.debug_collection("Changed files", &report.changed_files);
        document.debug("CI", &report.ci);
        document.debug("Review", &report.review);
        document.debug("Rate limit", &report.rate_limit);
        document.debug_collection("Provider warnings", &report.warnings);
        document.text("Fingerprint", &report.fingerprint);
        document.scalar("From cache", report.from_cache);
    }
    finish(document, envelope.status, maximum)
}

fn render_status(envelope: &ToolEnvelope<GraphStatusReport>, maximum: usize) -> (String, bool) {
    let mut document = tool_document("status", envelope, false, None);
    if let Some(report) = &envelope.data {
        document.text("Workspace", &report.workspace);
        document.scalar("Database schema", report.schema_version);
        document.scalar("Integrity ok", report.integrity_ok);
        document.debug("Snapshot", &report.snapshot);
        document.debug_collection("Repositories", &report.repositories);
    }
    finish(document, envelope.status, maximum)
}

fn render_contracts(envelope: &ToolEnvelope<ContractReport>, maximum: usize) -> (String, bool) {
    let mut document = tool_document("contracts", envelope, false, None);
    if let Some(report) = &envelope.data {
        document.scalar("Result version", report.result_version);
        document.debug("Action", &report.action);
        document.debug_collection("Contracts", &report.contracts);
        document.debug_collection("Links", &report.links);
        document.debug_collection("Compatibility", &report.compatibility);
        document.debug_collection("Differences", &report.differences);
        document.debug_collection("Issues", &report.issues);
        document.debug("Valid", &report.valid);
        document.scalar("Complete", report.complete);
        document.scalar("Truncated", report.truncated);
    }
    finish(document, envelope.status, maximum)
}

fn render_source_context(
    envelope: &ToolEnvelope<SourceContextReport>,
    maximum: usize,
) -> (String, bool) {
    let path = envelope.data.as_ref().and_then(|report| {
        report
            .evidence
            .iter()
            .find_map(|item| item.file_path.as_deref())
    });
    let mut document = tool_document("source_context", envelope, false, path);
    if let Some(report) = &envelope.data {
        document.text("Workspace", &report.workspace);
        document.debug("Entity", &report.entity);
        document.debug_collection("Related entities", &report.related_entities);
        document.debug_collection("Evidence", &report.evidence);
        document.scalar("Total evidence", report.total_evidence);
        document.scalar("Truncated", report.truncated);
    }
    finish(document, envelope.status, maximum)
}

fn render_scan(
    name: &str,
    envelope: &ToolEnvelope<AdminAudit<ScanSummary>>,
    maximum: usize,
) -> (String, bool) {
    let mut document = tool_document(name, envelope, false, None);
    if let Some(audit) = &envelope.data {
        render_audit_control(&mut document, audit);
        render_scan_summary(&mut document, &audit.result);
    }
    finish(document, envelope.status, maximum)
}

fn render_manifest_admin(
    name: &str,
    envelope: &ToolEnvelope<AdminAudit<ManifestAdminReport>>,
    maximum: usize,
) -> (String, bool) {
    let mut document = tool_document(name, envelope, false, None);
    if let Some(audit) = &envelope.data {
        render_audit_control(&mut document, audit);
        document.debug("Mutation", &audit.result.mutation);
        render_scan_summary(&mut document, &audit.result.scan);
    }
    finish(document, envelope.status, maximum)
}

fn render_cache_clean(
    envelope: &ToolEnvelope<AdminAudit<CacheCleanReport>>,
    maximum: usize,
) -> (String, bool) {
    let mut document = tool_document("clean_cache", envelope, false, None);
    if let Some(audit) = &envelope.data {
        render_audit_control(&mut document, audit);
        document.scalar("Removed entries", audit.result.removed_entries);
    }
    finish(document, envelope.status, maximum)
}

fn render_audit_control<T>(document: &mut MarkdownDocument, audit: &AdminAudit<T>) {
    document.text("Operation", &audit.operation);
    document.text("Workspace", &audit.workspace);
    document.scalar("Mutated", audit.mutated);
    document.scalar("Audit persisted", audit.audit_persisted);
}

fn render_next_actions(document: &mut MarkdownDocument, actions: &[AgentNextAction]) {
    let fragments = actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            let mut fragment = MarkdownDocument::fragment();
            let prefix = format!("Next action {}", index + 1);
            fragment.text(&format!("{prefix} tool"), &action.tool);
            fragment.debug(&format!("{prefix} arguments"), &action.arguments);
            fragment.text(&format!("{prefix} rationale"), &action.rationale);
            fragment.into_complete()
        })
        .collect();
    document.bounded_fragments("Next actions", actions.len(), false, fragments);
}

fn render_scan_summary(document: &mut MarkdownDocument, report: &ScanSummary) {
    document.debug("Execution", &report.execution);
    document.text("Scan workspace", &report.workspace);
    document.text("Snapshot", &report.snapshot_id);
    document.scalar("Node count", report.node_count);
    document.scalar("Edge count", report.edge_count);
    document.scalar("Evidence count", report.evidence_count);
    document.scalar("Community count", report.community_count);
    document.scalar("Community delta count", report.community_delta_count);
    document.scalar("Discovered input count", report.discovered_input_count);
    document.scalar("Changed input count", report.changed_input_count);
    document.scalar("Reused snapshot", report.reused_snapshot);
    document.scalar(
        "Corroborated symbol count",
        report.corroborated_symbol_count,
    );
    document.scalar("Affected test count", report.affected_test_count);
    document.scalar("Degradation count", report.degradation_count);
    document.string_collection("Degradations", &report.degradations);
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{
        FreshnessSummary, Node, NodeId, NodeKind, OverallFreshness, ToolEnvelope, ToolStatus, TraceReport, TraceSegment
    };

    use super::AgentToolResult;

    #[test]
    fn trace_variant_should_render_real_typed_fields() {
        let node = Node {
            id: NodeId::new("node:a"),
            kind: NodeKind::Service,
            repo_id: None,
            stable_key: "service:a".to_owned(),
            label: "A".to_owned(),
        };
        let envelope = ToolEnvelope {
            schema_version: 2,
            status: ToolStatus::Ok,
            data: Some(TraceReport {
                segments: Vec::<TraceSegment>::new(),
                truncated: false,
                coverage_gaps: vec![format!("{} has no path", node.id.as_str())],
            }),
            freshness: FreshnessSummary {
                overall: OverallFreshness::Fresh,
                stale_repositories: Vec::new(),
                reasons: Vec::new(),
            },
            warnings: Vec::new(),
        };
        let (rendered, is_error) = AgentToolResult::Trace(&envelope).render(4_096);
        assert!(!is_error);
        assert!(rendered.contains("## Segments"));
        assert!(rendered.contains("node:a has no path"));
    }
}
