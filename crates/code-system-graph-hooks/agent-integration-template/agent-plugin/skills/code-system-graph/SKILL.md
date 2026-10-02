---
name: "{{SKILL_NAME}}"
description: {{SKILL_DESCRIPTION_YAML}}
---

# Code System Graph

Use Code System Graph to find relevant repositories and entities, follow
relationships, and support conclusions with workspace graph evidence. Confirm
current behavior in live source, Git state, and focused tests when the task
explicitly asks about implementation behavior.

## Verify the workspace

1. Use this skill only when the task belongs to workspace `{{WORKSPACE}}` and
   the nearest relevant `code-system-graph.yaml` declares that name.
2. Call `status` with `workspace: "{{WORKSPACE}}"` before any other Code System Graph MCP tool. Confirm the
   workspace and note freshness, partial coverage, or degradation that affects
   the task.
3. If the MCP is unavailable or reports another workspace, stop using it. Do
   not guess paths or reconfigure graph tooling; continue with live repository
   evidence and report the limitation.

## Choose the smallest useful tool

| Goal | MCP tool |
| --- | --- |
| Find repositories, entities, capabilities, or known terms | `query` |
| Inspect persisted context for a known entity | `source_context` |
| Find architectural or subsystem groupings | `communities` |
| Find a path between two resolved entities | `trace` |
| Estimate upstream or downstream change risk | `impact` |
| Inspect or validate API and data contracts | `contracts` |
| Analyze working-tree or staged changes | `analyze_changes` |
| Analyze a pull request and cross-repository overlap | `analyze_pull_request` |
| Inspect repository-local source, symbols, call paths, or tests | `explore`, when available |

Use `query` before tools that require stable node identifiers; never invent an
identifier. Keep scopes, depths, and result limits bounded. If results are
ambiguous or truncated, refine the question or paginate before broadening it.
Call only the tools needed for the current task.

For relationship questions, follow this sequence:

1. For a cross-repository question, call `query` separately for each named
   repository alias or concept. When resolving a named repository, set
   `node_kinds` to `["repository"]` so documentation matches cannot widen the
   workflow. Resolve both sides to real entities and read the semantic
   relationship previews; structural containment is intentionally omitted from
   those previews.
2. Call `source_context` for the selected entity to see its separately labeled
   outgoing and incoming persisted relationships, repository boundaries,
   evidence, and known gaps.
3. Call `trace` only when two endpoint identifiers are known and the question
   needs the observed persisted chain between them.
4. Stop when `query`, `source_context`, and, when needed, `trace` answer the
   cross-repository question. Do not call `explore` merely to re-check a
   repository alias, documentation line, or already observed persisted edge.
   Treat a confirmed repository relationship as architectural evidence and
   state that boundary plainly. Do not expand it into an implementation or
   runtime audit with `explore`, shell searches, or source reads unless the user
   explicitly asks for source-level or runtime verification; asking how one
   repository depends on another is still answered by the relationship and its
   retained evidence.
5. For repository-local source relationships, call `explore` with the resolved
   repository alias and one concrete symbol, source file, or call-path question.
   When the repository is already named and confirmed by `status`, call
   `explore` directly; do not add `query` or `source_context` unless the user
   also asks about persisted semantic relationships. Omit `max_files` unless a
   smaller bound is necessary. Do not send broad prompts such as “find every
   reference” or use `explore` to reread this skill. `query` does not embed
   CodeGraph call paths, so an empty query preview is not evidence that the
   local relationship is absent.

For change-effect questions, resolve the target with `query`, then call
`impact`; do not substitute a broad relationship trace for impact analysis.
Treat the Markdown as the concise semantic explanation and
`structuredContent` as the complete machine-readable mirror for exact IDs,
scores, pagination, and automation. A relationship is evidence only when the
response names its origin, relationship, target, repository scope, and
epistemic status. Prefer cross-repository previews when they answer the task.
In a workspace with multiple registered repositories, treat a status report
with zero cross-repository relationships as a coverage gap rather than proof
that the repositories are independent.

## Use graph evidence correctly

- Use graph results to choose where to inspect when source-level verification
  is actually part of the request; do not silently widen an architectural
  relationship question into live-code investigation.
- Verify implementation claims with `explore` or focused source reads only
  when the task asks for those claims. Verify behavior changes with relevant
  tests when the task requires it.
- State only freshness, coverage, degradation, or evidence gaps that materially
  affect the conclusion.
- Do not infer that an entity or relationship does not exist when the relevant
  graph layer is stale, partial, unsupported, or capped.

## Keep maintenance explicit

Do not run `scan`, `sync`, `codegraph init`, admin tools, or configuration
mutations unless the user explicitly requests graph maintenance or repository
onboarding. For those tasks, read
[the operating guide](references/operating-guide.md) completely before acting.
Use live `--help` for the installed versions and make the smallest justified
change.
