---
name: "{{SKILL_NAME}}"
description: {{SKILL_DESCRIPTION_YAML}}
---

# Code System Graph

Use Code System Graph to find relevant repositories and entities, follow
relationships, and support conclusions with workspace graph evidence. Confirm
current behavior in live source, Git state, and focused tests.

## Verify the workspace

1. Use this skill only when the task belongs to workspace `{{WORKSPACE}}` and
   the nearest relevant `code-system-graph.yaml` declares that name.
2. Call `status` before any other Code System Graph MCP tool. Confirm the
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

## Use graph evidence correctly

- Use graph results to choose where to inspect and which relationships to
  verify; do not treat them as a replacement for live code or tests.
- Verify implementation claims with `explore` or focused source reads. Verify
  behavior changes with relevant tests when the task requires it.
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
