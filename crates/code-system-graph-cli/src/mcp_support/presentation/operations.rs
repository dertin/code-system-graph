//! Renderers for architecture, analysis, contract, and administrative MCP operations.

use std::collections::BTreeSet;

use code_system_graph_core::{
    ChangeImpactReport, ContractAction, ContractReport, ImpactReport, PullRequestInspection, ResolvedSymbol
};
use code_system_graph_model::ToolEnvelope;

use super::super::agent_views::{AgentPresentationContext, epistemic_status_name};
use super::super::{AdminAudit, CacheCleanReport, ManifestAdminReport};
use super::relations::{
    entity_display_label, entity_sentence, relation_markdown, relation_preview_markdown
};
use super::{
    SemanticMarkdown, add_gaps, add_next_actions, add_plain_gaps, code, enum_word, error_document, heading_text, location, plain, plural
};
use crate::{CommunityReport, ExploreReport, ScanSummary};

pub(super) fn render_explore(envelope: &ToolEnvelope<ExploreReport>) -> SemanticMarkdown {
    let Some(report) = envelope.data.as_ref() else {
        return error_document("Source exploration unavailable", envelope);
    };
    let mut document = SemanticMarkdown::semantic("Repository source exploration");
    document.add(format!(
        "Explored repository {} at {}.",
        code(&report.repository.alias),
        code(&report.repository.root),
    ));
    if !report.resolved_symbols.is_empty() {
        let symbols = report
            .resolved_symbols
            .iter()
            .map(|symbol| {
                format!(
                    "- {} ({}) at {}",
                    code(resolved_symbol_label(symbol)),
                    plain(&symbol.kind),
                    location(
                        &report.repository.alias,
                        &symbol.file_path,
                        u32::try_from(symbol.start_line).ok(),
                        None
                    )
                )
            })
            .collect::<Vec<_>>();
        document.add(format!("## Resolved symbols\n\n{}", symbols.join("\n")));
    }
    if !report.local_relationships.is_empty() {
        let relationships = report
            .local_relationships
            .iter()
            .map(|relationship| {
                let verb = match relationship.direction {
                    code_system_graph_core::LocalNeighborDirection::Callers => "is called by",
                    code_system_graph_core::LocalNeighborDirection::Callees => "calls",
                };
                format!(
                    "- {} **{verb}** {} at {}",
                    code(&relationship.anchor),
                    code(&relationship.neighbor.name),
                    location(
                        &report.repository.alias,
                        &relationship.neighbor.file_path,
                        u32::try_from(relationship.neighbor.start_line).ok(),
                        None
                    )
                )
            })
            .collect::<Vec<_>>();
        document.add(format!(
            "## Local call relationships\n\n{}",
            relationships.join("\n")
        ));
    }
    if !report.federated_handoffs.is_empty() {
        let handoffs = report
            .federated_handoffs
            .iter()
            .map(|handoff| {
                let repository = handoff
                    .remote_repository
                    .as_ref()
                    .map_or("unknown repository", |repository| repository.alias.as_str());
                format!(
                    "- Local anchor {} maps to persisted entity {} in {} ({})",
                    code(&handoff.anchor),
                    code(&handoff.label),
                    code(repository),
                    epistemic_status_name(handoff.status)
                )
            })
            .collect::<Vec<_>>();
        document.add(format!("## Federated handoffs\n\n{}", handoffs.join("\n")));
    }
    add_next_actions(&mut document, &report.next_actions);
    let mut gaps = report.coverage.gaps.clone();
    gaps.extend(report.execution.degradations.iter().cloned());
    add_plain_gaps(&mut document, envelope, &gaps);
    if !report.source_markdown.trim().is_empty() {
        document.untrusted_source(report.source_markdown.trim());
    }
    document
}

pub(super) fn resolved_symbol_label(symbol: &ResolvedSymbol) -> &str {
    symbol.qualified_name.as_deref().unwrap_or(&symbol.name)
}

pub(super) fn render_communities(envelope: &ToolEnvelope<CommunityReport>) -> SemanticMarkdown {
    let Some(report) = envelope.data.as_ref() else {
        return error_document("Communities unavailable", envelope);
    };
    let mut document = SemanticMarkdown::semantic("Architecture communities");
    document.add(format!(
        "Showing {} of {} inferred communit{} from snapshot {}.{}",
        report.communities.len(),
        report.total_communities,
        if report.total_communities == 1 {
            "y"
        } else {
            "ies"
        },
        code(&report.snapshot_id),
        if report.truncated {
            " More communities are available."
        } else {
            ""
        }
    ));
    for community in &report.communities {
        let limitations = if community.limitations.is_empty() {
            String::new()
        } else {
            format!(" Limitations: {}", community.limitations.join("; "))
        };
        document.add(format!(
            "## {}\n\nThis community groups {} entities across {} repositor{} and {} service{}.{}",
            heading_text(&community.label),
            community.members.len(),
            community.repositories.len(),
            if community.repositories.len() == 1 {
                "y"
            } else {
                "ies"
            },
            community.services.len(),
            plural(community.services.len()),
            limitations
        ));
    }
    add_plain_gaps(&mut document, envelope, &[]);
    document
}

pub(super) fn render_impact(
    envelope: &ToolEnvelope<ImpactReport>,
    context: &AgentPresentationContext,
) -> SemanticMarkdown {
    let Some(report) = envelope.data.as_ref() else {
        return error_document("Impact analysis unavailable", envelope);
    };
    let target = context.entity(&report.target.node);
    let mut document = SemanticMarkdown::semantic(&format!(
        "Impact of {}",
        heading_text(entity_display_label(&target))
    ));
    document.add(format!(
        "{} The analysis found risk {} with {} directly affected, {} transitively affected, {} possibly affected, and {} unresolved due to coverage.",
        entity_sentence(&target),
        enum_word(&report.risk),
        report.direct_consumers.len(),
        report.transitive_consumers.len(),
        report.possibly_affected.len(),
        report.unknown_due_to_coverage.len()
    ));
    add_impact_entities(
        &mut document,
        "Directly affected",
        &report.direct_consumers,
        context,
    );
    add_impact_entities(
        &mut document,
        "Transitively affected",
        &report.transitive_consumers,
        context,
    );
    add_impact_entities(
        &mut document,
        "Possibly affected",
        &report.possibly_affected,
        context,
    );
    if !report.reasons.is_empty() {
        let reasons = report
            .reasons
            .iter()
            .map(|reason| format!("- {}", plain(&reason.explanation)))
            .collect::<Vec<_>>();
        document.add(format!("## Why\n\n{}", reasons.join("\n")));
    }
    add_gaps(
        &mut document,
        envelope,
        context,
        false,
        &report.coverage.gaps,
    );
    document
}

pub(super) fn render_changes(
    envelope: &ToolEnvelope<ChangeImpactReport>,
    context: &AgentPresentationContext,
) -> SemanticMarkdown {
    let Some(report) = envelope.data.as_ref() else {
        return error_document("Change analysis unavailable", envelope);
    };
    let mut document = SemanticMarkdown::semantic("Change impact");
    document.add(format!(
        "Conclusion: **{}**. The change maps to {} graph entit{}, {} contract{}, and {} impact report{}; highest risk is {}.",
        enum_word(&report.summary.conclusion),
        report.summary.changed_entities,
        if report.summary.changed_entities == 1 { "y" } else { "ies" },
        report.summary.changed_contracts,
        plural(report.summary.changed_contracts),
        report.summary.impact_reports,
        plural(report.summary.impact_reports),
        enum_word(&report.summary.highest_risk)
    ));
    if !report.changed_entities.is_empty() {
        let entities = report
            .changed_entities
            .iter()
            .map(|changed| format!("- {}", entity_sentence(&context.entity(&changed.node))))
            .collect::<Vec<_>>();
        document.add(format!("## Changed entities\n\n{}", entities.join("\n")));
    }
    add_gaps(
        &mut document,
        envelope,
        context,
        false,
        &report.coverage.gaps,
    );
    document
}

pub(super) fn render_pull_request(
    envelope: &ToolEnvelope<PullRequestInspection>,
) -> SemanticMarkdown {
    let Some(report) = envelope.data.as_ref() else {
        return error_document("Pull request unavailable", envelope);
    };
    let mut document = SemanticMarkdown::semantic(&format!(
        "Pull request #{}: {}",
        report.coordinates.number,
        heading_text(&report.metadata.title)
    ));
    document.add(format!(
        "{} in {}/{} is {}. CI is {}; review is {} with {} approval{}. {} changed file{} were retained.",
        code(&report.metadata.url),
        plain(&report.coordinates.owner),
        plain(&report.coordinates.repository),
        enum_word(&report.metadata.state),
        enum_word(&report.ci.state),
        enum_word(&report.review.state),
        report.review.approvals,
        plural(report.review.approvals),
        report.changed_files.len(),
        plural(report.changed_files.len())
    ));
    if !report.changed_files.is_empty() {
        let files = report
            .changed_files
            .iter()
            .map(|file| {
                let path = file
                    .new_path
                    .as_deref()
                    .or(file.old_path.as_deref())
                    .unwrap_or("unknown path");
                format!(
                    "- {} — {}, +{} / -{} lines",
                    code(path),
                    enum_word(&file.status),
                    file.additions,
                    file.deletions
                )
            })
            .collect::<Vec<_>>();
        document.add(format!("## Changed files\n\n{}", files.join("\n")));
    }
    let warnings = report
        .warnings
        .iter()
        .map(|warning| warning.message.clone())
        .collect::<Vec<_>>();
    add_plain_gaps(&mut document, envelope, &warnings);
    document
}

pub(super) fn render_contracts(
    envelope: &ToolEnvelope<ContractReport>,
    context: &AgentPresentationContext,
) -> SemanticMarkdown {
    let Some(report) = envelope.data.as_ref() else {
        return error_document("Contracts unavailable", envelope);
    };
    let mut document = SemanticMarkdown::semantic("Contract analysis");
    let mut seen_links = BTreeSet::new();
    let all_links = report
        .links
        .iter()
        .chain(report.contracts.iter().flat_map(|contract| &contract.links))
        .filter(|link| seen_links.insert(link.edge.id.clone()))
        .collect::<Vec<_>>();
    add_contract_overview(&mut document, report, context, all_links.len());
    if !all_links.is_empty() {
        let links = all_links
            .iter()
            .filter_map(|link| context.relation_for_edge(&link.edge, None))
            .map(|relation| format!("- {}", relation_markdown(&relation)))
            .collect::<Vec<_>>();
        if !links.is_empty() {
            document.add(format!(
                "## Observed contract relationships\n\n{}",
                links.join("\n")
            ));
        }
    }
    let issues = report
        .issues
        .iter()
        .map(|issue| {
            issue
                .entity_id
                .as_deref()
                .and_then(|edge_id| context.relation_for_edge_id(edge_id))
                .map_or_else(
                    || issue.message.clone(),
                    |relation| {
                        format!(
                            "{} Affected relationship: {}",
                            issue.message,
                            relation_preview_markdown(&relation)
                        )
                    },
                )
        })
        .collect::<Vec<_>>();
    add_gaps(&mut document, envelope, context, false, &issues);
    document
}

fn add_contract_overview(
    document: &mut SemanticMarkdown,
    report: &ContractReport,
    context: &AgentPresentationContext,
    direct_links: usize,
) {
    let coverage = if report.complete {
        "complete for this contract operation"
    } else {
        "incomplete for this contract operation"
    };
    let bounded = if report.truncated {
        " The result was bounded."
    } else {
        ""
    };
    match report.action {
        ContractAction::List => {
            document.add(format!(
                "## Contract inventory\n\nThe list operation returned {} contract{}. Relationships are intentionally not loaded in this bounded list view; use `show` for one contract's observed links. Coverage is {coverage}.{bounded}",
                report.contracts.len(),
                plural(report.contracts.len()),
            ));
            let contracts = report
                .contracts
                .iter()
                .map(|contract| {
                    let entity = context.entity(&contract.contract);
                    format!(
                        "- {} — {}",
                        code(entity_display_label(&entity)),
                        entity_sentence(&entity)
                    )
                })
                .collect::<Vec<_>>();
            if !contracts.is_empty() {
                document.add(format!("## Contracts\n\n{}", contracts.join("\n")));
            }
        }
        ContractAction::Validate => {
            let scope = report.contracts.first().map_or_else(
                || "across all contracts".to_owned(),
                |contract| {
                    format!(
                        "for {}",
                        code(entity_display_label(&context.entity(&contract.contract)))
                    )
                },
            );
            let conclusion = match report.valid {
                Some(true) => "found no errors or uncertain relationships",
                Some(false) => {
                    "found errors or uncertain relationships; review the known gaps below"
                }
                None => "did not produce a conclusive validity result",
            };
            document.add(format!(
                "## Validation result\n\nValidation completed {scope} and {conclusion}. Coverage is {coverage}.{bounded}"
            ));
        }
        ContractAction::Show => {
            document.add(format!(
                "## Contract details\n\nThe show operation returned {} contract{} with {direct_links} observed direct relationship{}. Coverage is {coverage}.{bounded}",
                report.contracts.len(),
                plural(report.contracts.len()),
                plural(direct_links),
            ));
            add_contract_entities(document, report, context);
        }
        ContractAction::Diff => {
            document.add(format!(
                "## Contract differences\n\nCompared {} contract{} and found {} structural difference{} with {} compatibility result{}. Coverage is {coverage}.{bounded}",
                report.contracts.len(),
                plural(report.contracts.len()),
                report.differences.len(),
                plural(report.differences.len()),
                report.compatibility.len(),
                plural(report.compatibility.len()),
            ));
            add_contract_entities(document, report, context);
        }
        ContractAction::ExplainLink => {
            document.add(format!(
                "## Relationship explanation\n\nExamined {} contract{} and found {direct_links} observed direct relationship{}. Coverage is {coverage}.{bounded}",
                report.contracts.len(),
                plural(report.contracts.len()),
                plural(direct_links),
            ));
            add_contract_entities(document, report, context);
        }
    }
}

fn add_contract_entities(
    document: &mut SemanticMarkdown,
    report: &ContractReport,
    context: &AgentPresentationContext,
) {
    for contract in &report.contracts {
        let entity = context.entity(&contract.contract);
        document.add(format!(
            "## {}\n\n{}",
            heading_text(entity_display_label(&entity)),
            entity_sentence(&entity),
        ));
    }
}

pub(super) fn render_scan(
    title: &str,
    envelope: &ToolEnvelope<AdminAudit<ScanSummary>>,
) -> SemanticMarkdown {
    let Some(audit) = envelope.data.as_ref() else {
        return error_document("Scan failed", envelope);
    };
    let report = &audit.result;
    let mut document = SemanticMarkdown::semantic(title);
    document.add(format!(
        "Workspace {} published snapshot {} with {} entities, {} relationships, {} evidence records, and {} communities. The scan {} the existing snapshot.",
        code(&audit.workspace),
        code(&report.snapshot_id),
        report.node_count,
        report.edge_count,
        report.evidence_count,
        report.community_count,
        if report.reused_snapshot { "reused" } else { "replaced" }
    ));
    add_plain_gaps(&mut document, envelope, &report.degradations);
    document
}

pub(super) fn render_manifest_admin(
    title: &str,
    envelope: &ToolEnvelope<AdminAudit<ManifestAdminReport>>,
) -> SemanticMarkdown {
    let Some(audit) = envelope.data.as_ref() else {
        return error_document("Workspace update failed", envelope);
    };
    let mut document = SemanticMarkdown::semantic(title);
    document.add(format!(
        "{} The manifest change was {} and the follow-up scan published snapshot {} with {} entities and {} relationships.",
        plain(&audit.result.mutation.summary),
        if audit.result.mutation.applied { "applied" } else { "previewed" },
        code(&audit.result.scan.snapshot_id),
        audit.result.scan.node_count,
        audit.result.scan.edge_count
    ));
    if let Some(backup) = &audit.result.mutation.backup {
        document.add(format!(
            "A recoverable manifest backup is available at {}.",
            code(backup)
        ));
    }
    add_plain_gaps(&mut document, envelope, &audit.result.scan.degradations);
    document
}

pub(super) fn render_cache_clean(
    envelope: &ToolEnvelope<AdminAudit<CacheCleanReport>>,
) -> SemanticMarkdown {
    let Some(audit) = envelope.data.as_ref() else {
        return error_document("Cache cleanup failed", envelope);
    };
    let mut document = SemanticMarkdown::semantic("Query cache cleaned");
    document.add(format!(
        "Removed {} reusable query entr{} for workspace {}. Graph snapshots were not changed.",
        audit.result.removed_entries,
        if audit.result.removed_entries == 1 {
            "y"
        } else {
            "ies"
        },
        code(&audit.workspace)
    ));
    add_plain_gaps(&mut document, envelope, &[]);
    document
}

fn add_impact_entities(
    document: &mut SemanticMarkdown,
    heading: &str,
    items: &[code_system_graph_core::ImpactItem],
    context: &AgentPresentationContext,
) {
    if items.is_empty() {
        return;
    }
    let lines = items
        .iter()
        .map(|item| {
            let entity = context.entity(&item.node);
            format!(
                "- {} — {} relationship hop{}, classified as {}.",
                entity_sentence(&entity),
                item.depth,
                plural(item.depth),
                enum_word(&item.classification)
            )
        })
        .collect::<Vec<_>>();
    document.add(format!("## {heading}\n\n{}", lines.join("\n")));
}
