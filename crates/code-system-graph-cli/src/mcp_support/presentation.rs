//! Semantic, dual-channel MCP presentation for coding agents.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use code_system_graph_core::{
    AgentNextAction, ChangeImpactReport, ContractReport, ImpactReport, PullRequestInspection, SearchReport
};
use code_system_graph_model::{FreshnessSummary, ToolEnvelope, ToolStatus, TraceReport};
use serde::Serialize;
use serde_json::{Value, json};

use super::agent_views::{
    AGENT_DELIVERY_SCHEMA_VERSION, AgentPresentationContext, AgentQueryReport, AgentSourceContextReport, AgentStatusReport, AgentStructuredEnvelope, AgentTraceReport, explore_view, freshness_name, freshness_state_name, query_view, source_context_view, status_view, trace_view
};
use super::{
    AdminAudit, CacheCleanReport, GraphStatusReport, ManifestAdminReport, SourceContextReport
};
use crate::agent_markdown::MarkdownDocument;
use crate::{CommunityReport, ExploreReport, ScanSummary};

mod operations;
mod relations;
#[cfg(test)]
mod tests;

use operations::{
    render_cache_clean, render_changes, render_communities, render_contracts, render_explore, render_impact, render_manifest_admin, render_pull_request, render_scan
};
use relations::{
    add_relation_section, entity_display_label, entity_sentence, query_entity_summary, relation_is_component_of_summary, relation_markdown, relation_preview_markdown
};

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

pub(crate) struct AgentDelivery {
    pub(crate) markdown: String,
    pub(crate) structured_content: Value,
    pub(crate) is_error: bool,
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

    #[expect(
        clippy::too_many_lines,
        reason = "The exhaustive tool dispatch keeps Markdown and structured output paired by variant"
    )]
    pub(crate) fn deliver(
        self,
        maximum: usize,
        context: &AgentPresentationContext,
    ) -> AgentDelivery {
        let (rendered, structured_content) = match self {
            Self::Trace(envelope) => {
                let view = envelope
                    .data
                    .as_ref()
                    .map(|report| trace_view(report, context));
                let gap = context.scoped_gap(
                    view.as_ref()
                        .is_some_and(|view| view.segments.iter().any(event_relation)),
                );
                (
                    render_trace(envelope, context, view.as_ref()),
                    structured_agent("trace", envelope, view, gap.as_deref()),
                )
            }
            Self::Query(envelope) => {
                let view = envelope
                    .data
                    .as_ref()
                    .map(|report| query_view(report, context));
                let gap = context.scoped_gap(view.as_ref().is_some_and(|view| {
                    view.cross_repository_relations.iter().any(event_relation)
                }));
                (
                    render_query(envelope, context, view.as_ref()),
                    structured_agent("query", envelope, view, gap.as_deref()),
                )
            }
            Self::Status(envelope) => {
                let view = envelope
                    .data
                    .as_ref()
                    .map(|report| status_view(report, context));
                let gap = context.scoped_gap(true);
                (
                    render_status(envelope, context, view.as_ref()),
                    structured_agent("status", envelope, view, gap.as_deref()),
                )
            }
            Self::SourceContext(envelope) => {
                let view = envelope
                    .data
                    .as_ref()
                    .map(|report| source_context_view(report, context));
                let gap = context.scoped_gap(
                    view.as_ref()
                        .is_some_and(|view| view.projected_relations.iter().any(event_relation)),
                );
                (
                    render_source_context(envelope, context, view.as_ref()),
                    structured_agent("source_context", envelope, view, gap.as_deref()),
                )
            }
            Self::Explore(envelope) => (
                render_explore(envelope),
                structured_agent(
                    "explore",
                    envelope,
                    envelope.data.as_ref().map(explore_view),
                    None,
                ),
            ),
            Self::Communities(envelope) => (
                render_communities(envelope),
                structured_raw("communities", envelope),
            ),
            Self::Impact(envelope) => (
                render_impact(envelope, context),
                structured_raw_with_gap("impact", envelope, context.gap()),
            ),
            Self::AnalyzeChanges(envelope) => (
                render_changes(envelope, context),
                structured_raw_with_gap("analyze_changes", envelope, context.gap()),
            ),
            Self::AnalyzePullRequest(envelope) => (
                render_pull_request(envelope),
                structured_raw("analyze_pull_request", envelope),
            ),
            Self::Contracts(envelope) => (
                render_contracts(envelope, context),
                structured_raw_with_gap("contracts", envelope, context.gap()),
            ),
            Self::Scan(envelope) => (
                render_scan("Scan complete", envelope),
                structured_raw("scan", envelope),
            ),
            Self::UpdateWorkspace(envelope) => (
                render_manifest_admin("Workspace updated", envelope),
                structured_raw("update_workspace", envelope),
            ),
            Self::WriteManualLink(envelope) => (
                render_manifest_admin("Manual relationship updated", envelope),
                structured_raw("write_manual_link", envelope),
            ),
            Self::CleanCache(envelope) => (
                render_cache_clean(envelope),
                structured_raw("clean_cache", envelope),
            ),
            Self::RecomputeCommunities(envelope) => (
                render_scan("Communities recomputed", envelope),
                structured_raw("recompute_communities", envelope),
            ),
        };
        AgentDelivery {
            markdown: rendered.render(maximum),
            structured_content,
            is_error: self.status() == ToolStatus::Error,
        }
    }

    #[cfg(test)]
    pub(crate) fn render(
        self,
        maximum: usize,
        context: &AgentPresentationContext,
    ) -> (String, bool) {
        let delivery = self.deliver(maximum, context);
        (delivery.markdown, delivery.is_error)
    }

    #[cfg(test)]
    pub(crate) fn structured_content(self, context: &AgentPresentationContext) -> Value {
        self.deliver(usize::MAX, context).structured_content
    }

    fn status(self) -> ToolStatus {
        match self {
            Self::Trace(envelope) => envelope.status,
            Self::Query(envelope) => envelope.status,
            Self::Explore(envelope) => envelope.status,
            Self::Communities(envelope) => envelope.status,
            Self::Impact(envelope) => envelope.status,
            Self::AnalyzeChanges(envelope) => envelope.status,
            Self::AnalyzePullRequest(envelope) => envelope.status,
            Self::Status(envelope) => envelope.status,
            Self::Contracts(envelope) => envelope.status,
            Self::SourceContext(envelope) => envelope.status,
            Self::Scan(envelope) | Self::RecomputeCommunities(envelope) => envelope.status,
            Self::UpdateWorkspace(envelope) | Self::WriteManualLink(envelope) => envelope.status,
            Self::CleanCache(envelope) => envelope.status,
        }
    }
}

fn structured_agent<T: Serialize, U>(
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

fn structured_raw<T: Serialize>(tool: &str, envelope: &ToolEnvelope<T>) -> Value {
    structured_raw_with_gap(tool, envelope, None)
}

fn structured_raw_with_gap<T: Serialize>(
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

fn event_relation(relation: &super::agent_views::AgentRelationView) -> bool {
    relation.derivation == super::agent_views::AgentRelationDerivation::EventDeliveryPath
}

type SemanticMarkdown = MarkdownDocument;

fn render_trace(
    envelope: &ToolEnvelope<TraceReport>,
    context: &AgentPresentationContext,
    view: Option<&AgentTraceReport>,
) -> SemanticMarkdown {
    let Some(view) = view else {
        return error_document("Trace unavailable", envelope);
    };
    let mut document = SemanticMarkdown::semantic("Relationship trace");
    if view.segments.is_empty() {
        document.add("No observed relationship chain connects the requested entities within the configured depth.");
    } else {
        document.add(format!(
            "Found a chain of {} observed relationship{}; {} cross repository boundaries.",
            view.segments.len(),
            plural(view.segments.len()),
            view.cross_repository_hops
        ));
        for (index, relation) in view.segments.iter().enumerate() {
            document.add(format!(
                "## Hop {}\n\n{}",
                index + 1,
                relation_markdown(relation)
            ));
        }
    }
    if view.truncated {
        document.add("The trace stopped at the configured bound, so a longer chain may exist.");
    }
    add_gaps(
        &mut document,
        envelope,
        context,
        view.segments.iter().any(event_relation),
        &view.coverage_gaps,
    );
    document
}

#[expect(
    clippy::too_many_lines,
    reason = "The renderer keeps one ordered semantic document and cross-result deduplication state"
)]
fn render_query(
    envelope: &ToolEnvelope<SearchReport>,
    context: &AgentPresentationContext,
    view: Option<&AgentQueryReport>,
) -> SemanticMarkdown {
    let Some(view) = view else {
        return error_document("Search unavailable", envelope);
    };
    let mut document = SemanticMarkdown::semantic("Architecture search results");
    let mut summary = format!(
        "The current graph found {} ranked result{}. This page shows {} distinct entit{}",
        view.total_matches,
        plural(view.total_matches),
        view.results.len(),
        if view.results.len() == 1 { "y" } else { "ies" },
    );
    if view.collapsed_duplicate_matches > 0 {
        let _ = write!(
            summary,
            "; {} duplicate graph representation{} {} merged",
            view.collapsed_duplicate_matches,
            plural(view.collapsed_duplicate_matches),
            if view.collapsed_duplicate_matches == 1 {
                "was"
            } else {
                "were"
            }
        );
    }
    if view.subordinate_matches_omitted > 0 {
        let _ = write!(
            summary,
            "; {} structural subordinate{} {} omitted from the prose",
            view.subordinate_matches_omitted,
            plural(view.subordinate_matches_omitted),
            if view.subordinate_matches_omitted == 1 {
                "was"
            } else {
                "were"
            }
        );
    }
    summary.push('.');
    if view.truncated {
        summary.push_str(" More ranked matches are available.");
    }
    document.add(summary);
    if !view.cross_repository_relations.is_empty() {
        let relationships = view
            .cross_repository_relations
            .iter()
            .map(|relation| format!("- {}", relation_preview_markdown(relation)))
            .collect::<Vec<_>>();
        document.add(format!(
            "## Cross-repository relationships\n\n{}",
            relationships.join("\n\n")
        ));
    }
    if view.results.is_empty() {
        document.add("No entity matched the requested terms in the current graph snapshot.");
    }
    let mut presented_relations = view
        .cross_repository_relations
        .iter()
        .map(|relation| relation.edge_id.as_str())
        .collect::<BTreeSet<_>>();
    for (index, hit) in view.results.iter().enumerate() {
        let mut block = format!(
            "## {}. {}\n\n{} It {}.",
            index + 1,
            heading_text(entity_display_label(&hit.entity)),
            query_entity_summary(&hit.entity, &hit.roles),
            hit.matched_because
        );
        let remaining_relations = hit
            .relation_previews
            .iter()
            .filter(|relation| {
                !presented_relations.contains(relation.edge_id.as_str())
                    && !relation_is_component_of_summary(relation, &view.cross_repository_relations)
            })
            .collect::<Vec<_>>();
        if hit.relation_previews.is_empty() {
            block.push_str(
                "\n\nNo semantic dependency is attached to this entity in the persisted snapshot. Structural file-containment relationships are omitted from search previews.",
            );
        } else if remaining_relations.is_empty() {
            block.push_str("\n\nIts semantic relationships are summarized above.");
        } else {
            block.push_str("\n\nObserved relationships:");
            for relation in &remaining_relations {
                block.push_str("\n\n- ");
                block.push_str(&relation_preview_markdown(relation));
            }
        }
        presented_relations.extend(
            remaining_relations
                .iter()
                .map(|relation| relation.edge_id.as_str()),
        );
        document.add(block);
    }
    let has_source_context = view
        .next_actions
        .iter()
        .any(|action| action.tool == "source_context");
    let has_explore = view
        .next_actions
        .iter()
        .any(|action| action.tool == "explore");
    if has_source_context || has_explore {
        let guidance = match (has_source_context, has_explore) {
            (true, true) => {
                "Use `source_context` for persisted direct relationships and evidence; use `explore` for repository-local symbols and call paths from CodeGraph."
            }
            (true, false) => {
                "Use `source_context` for persisted direct relationships and evidence."
            }
            (false, true) => {
                "Use `explore` for repository-local symbols and call paths from CodeGraph."
            }
            (false, false) => unreachable!(),
        };
        document.add(guidance);
    }
    let semantic_gaps = view
        .coverage
        .gaps
        .iter()
        .filter(|gap| gap.as_str() != "Full-text scores were not provided.")
        .cloned()
        .collect::<Vec<_>>();
    add_gaps(
        &mut document,
        envelope,
        context,
        view.cross_repository_relations.iter().any(event_relation),
        &semantic_gaps,
    );
    document
}

fn render_source_context(
    envelope: &ToolEnvelope<SourceContextReport>,
    context: &AgentPresentationContext,
    view: Option<&AgentSourceContextReport>,
) -> SemanticMarkdown {
    let Some(view) = view else {
        return error_document("Entity context unavailable", envelope);
    };
    let mut document = SemanticMarkdown::semantic(&format!(
        "Context for {}",
        heading_text(entity_display_label(&view.entity))
    ));
    document.add(if view.projected_relations.is_empty() {
        format!(
            "{} The current snapshot contains {} direct semantic relationship{} and {} supporting evidence record{}.",
            entity_sentence(&view.entity),
            view.direct_relations,
            plural(view.direct_relations),
            view.total_evidence,
            plural(view.total_evidence),
        )
    } else {
        format!(
            "{} The current snapshot shows {} repository-level or derived integration{} and {} direct semantic relationship{} at this node level, supported by {} evidence record{}.",
            entity_sentence(&view.entity),
            view.projected_relations.len(),
            plural(view.projected_relations.len()),
            view.direct_relations,
            plural(view.direct_relations),
            view.total_evidence,
            plural(view.total_evidence),
        )
    });
    if view.structural_relations_omitted > 0 {
        document.add(format!(
            "{} structural containment or membership relationship{} {} omitted because structural links do not describe a dependency.",
            view.structural_relations_omitted,
            plural(view.structural_relations_omitted),
            if view.structural_relations_omitted == 1 { "was" } else { "were" },
        ));
    }
    if !view.projected_relations.is_empty() {
        let projected = view
            .projected_relations
            .iter()
            .map(|relation| format!("- {}", relation_preview_markdown(relation)))
            .collect::<Vec<_>>();
        document.add(format!(
            "## Repository-level and derived integrations\n\n{}",
            projected.join("\n\n")
        ));
    }
    let visible_outgoing = view
        .outgoing_relations
        .iter()
        .filter(|relation| !relation_is_component_of_summary(relation, &view.projected_relations))
        .cloned()
        .collect::<Vec<_>>();
    let visible_incoming = view
        .incoming_relations
        .iter()
        .filter(|relation| !relation_is_component_of_summary(relation, &view.projected_relations))
        .cloned()
        .collect::<Vec<_>>();
    let collapsed_observations = view
        .outgoing_relations
        .len()
        .saturating_sub(visible_outgoing.len())
        + view
            .incoming_relations
            .len()
            .saturating_sub(visible_incoming.len());
    if !visible_outgoing.is_empty() || view.projected_relations.is_empty() {
        add_relation_section(&mut document, "Outgoing relationships", &visible_outgoing);
    }
    if !visible_incoming.is_empty() || view.projected_relations.is_empty() {
        add_relation_section(&mut document, "Incoming relationships", &visible_incoming);
    }
    if collapsed_observations > 0 {
        document.add(format!(
            "{} direct event observation{} {} collapsed into the repository-level integration above; the original edges remain available in `structuredContent`.",
            collapsed_observations,
            plural(collapsed_observations),
            if collapsed_observations == 1 {
                "was"
            } else {
                "were"
            },
        ));
    }
    if view.relations_truncated {
        document.add("Some direct relationships were omitted by the relationship limit.");
    }
    if view.evidence_truncated {
        document.add("Some supporting evidence locations were omitted by the evidence limit.");
    }
    add_gaps(
        &mut document,
        envelope,
        context,
        view.projected_relations.iter().any(event_relation),
        &[],
    );
    document
}

fn render_status(
    envelope: &ToolEnvelope<GraphStatusReport>,
    context: &AgentPresentationContext,
    view: Option<&AgentStatusReport>,
) -> SemanticMarkdown {
    let Some(view) = view else {
        return error_document("Graph status unavailable", envelope);
    };
    let mut document = SemanticMarkdown::semantic("Graph status");
    let health = if view.integrity_ok {
        "valid"
    } else {
        "invalid"
    };
    document.add(format!(
        "## Summary\n\nWorkspace {} has a {} graph with {health} database integrity. It contains {} entities, {} relationships, and {} evidence records across {} repositor{}.",
        code(&view.workspace),
        freshness_name(envelope.freshness.overall),
        view.node_count,
        view.edge_count,
        view.evidence_count,
        view.repositories.len(),
        if view.repositories.len() == 1 { "y" } else { "ies" },
    ));
    document.add(format!(
        "Of those relationships, {} are semantic and {} are structural containment or membership links. {} semantic relationship{} cross{} a repository boundary.",
        view.semantic_relationship_count,
        view.structural_relationship_count,
        view.cross_repository_relationship_count,
        plural(view.cross_repository_relationship_count),
        if view.cross_repository_relationship_count == 1 { "es" } else { "" },
    ));
    if view.retained_cross_repository_event_integration_count > 0 {
        document.add(format!(
            "Additionally, {} cross-repository event integration{} {} retained from {} repository/channel pair{}.{}",
            view.retained_cross_repository_event_integration_count,
            plural(view.retained_cross_repository_event_integration_count),
            if view.retained_cross_repository_event_integration_count == 1 { "was" } else { "were" },
            view.event_delivery_pair_count,
            plural(view.event_delivery_pair_count),
            if view.event_delivery_projection_truncated { " The event projection is truncated." } else { "" },
        ));
    }
    if view.workspace_scoped_relationship_count > 0 {
        document.add(format!(
            "{} relationship{} connect{} a repository entity to a workspace-scoped entity such as a package, API, table, or deployment target; these are not counted as cross-repository links.",
            view.workspace_scoped_relationship_count,
            plural(view.workspace_scoped_relationship_count),
            if view.workspace_scoped_relationship_count == 1 { "s" } else { "" },
        ));
    }
    if view.repositories.len() > 1 && view.cross_repository_relationship_count == 0 {
        document.add(
            "Coverage gap: this multi-repository snapshot contains no observed relationship whose two endpoints belong to different repositories.",
        );
    }
    let unhealthy = view
        .repositories
        .iter()
        .filter(|repository| freshness_state_name(repository.state) != "fresh")
        .map(|repository| {
            let name = repository
                .repository_alias
                .as_deref()
                .unwrap_or(&repository.repository_id);
            match &repository.reason {
                Some(reason) => format!(
                    "- {} is {}: {}",
                    code(name),
                    freshness_state_name(repository.state),
                    plain(reason)
                ),
                None => format!(
                    "- {} is {}.",
                    code(name),
                    freshness_state_name(repository.state)
                ),
            }
        })
        .collect::<Vec<_>>();
    if !unhealthy.is_empty() {
        document.add(format!(
            "## Repositories needing attention\n\n{}",
            unhealthy.join("\n")
        ));
    }
    add_gaps(
        &mut document,
        envelope,
        context,
        true,
        &envelope.freshness.reasons,
    );
    document
}

fn add_next_actions(document: &mut SemanticMarkdown, actions: &[AgentNextAction]) {
    if actions.is_empty() {
        return;
    }
    let actions = actions
        .iter()
        .map(|action| format!("- Use {}: {}", code(&action.tool), plain(&action.rationale)))
        .collect::<Vec<_>>();
    document.add(format!("## Useful next steps\n\n{}", actions.join("\n")));
}

fn add_gaps(
    document: &mut SemanticMarkdown,
    envelope: &impl EnvelopeMetadata,
    context: &AgentPresentationContext,
    include_events: bool,
    gaps: &[String],
) {
    let mut combined = gaps.to_vec();
    if let Some(gap) = context.scoped_gap(include_events) {
        combined.push(gap);
    }
    add_plain_gaps(document, envelope, &combined);
}

fn add_plain_gaps(
    document: &mut SemanticMarkdown,
    envelope: &impl EnvelopeMetadata,
    gaps: &[String],
) {
    let mut messages = envelope.warnings().to_vec();
    messages.extend(gaps.iter().cloned());
    messages.sort();
    messages.dedup();
    if messages.is_empty() {
        return;
    }
    let lines = messages
        .iter()
        .map(|message| format!("- {}", plain(message)))
        .collect::<Vec<_>>();
    document.add(format!("## Known gaps\n\n{}", lines.join("\n")));
}

trait EnvelopeMetadata {
    fn warnings(&self) -> &[String];
}

impl<T> EnvelopeMetadata for ToolEnvelope<T> {
    fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

fn error_document<T>(title: &str, envelope: &ToolEnvelope<T>) -> SemanticMarkdown {
    let mut document = SemanticMarkdown::semantic(title);
    document.add("The tool could not produce semantic result data for this request.");
    add_plain_gaps(&mut document, envelope, &[]);
    document
}

fn location(
    repository: &str,
    path: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
) -> String {
    let suffix = match (start_line, end_line) {
        (Some(start), Some(end)) if start != end => format!(":{start}-{end}"),
        (Some(start), _) => format!(":{start}"),
        _ => String::new(),
    };
    code(&format!("{repository}/{path}{suffix}"))
}

fn enum_word<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
        .replace(['_', '-'], " ")
}

fn code(value: &str) -> String {
    format!("`{}`", value.replace('`', "'").replace(['\n', '\r'], " "))
}

fn plain(value: &str) -> String {
    value.replace(['\n', '\r'], " ").trim().to_owned()
}

fn heading_text(value: &str) -> String {
    plain(value)
        .replace('`', "'")
        .replace(['#', '*', '_', '[', ']', '<', '>'], "")
        .replace('|', "¦")
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn human_join(values: &[&str]) -> String {
    match values {
        [] => "unknown".to_owned(),
        [value] => (*value).to_owned(),
        [left, right] => format!("{left} and {right}"),
        _ => {
            let (last, leading) = values.split_last().expect("non-empty values");
            format!("{}, and {last}", leading.join(", "))
        }
    }
}
