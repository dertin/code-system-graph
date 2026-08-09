# Code System Graph operating guide

Use this reference only for graph maintenance, repository onboarding,
configuration, freshness diagnosis, or scan-limit failures.

Inspect the installed `csgraph` and `codegraph` versions rather than assuming a
specific release is active. Use the official documentation for those versions
and their live CLI help when behavior differs.

## Inspect and refresh

Run commands from the workspace root or pass explicit manifest and database
paths:

```bash
csgraph --version
codegraph --version
csgraph config show --config <manifest>
csgraph status --config <manifest> --database <database> <workspace>
csgraph doctor --config <manifest> --database <database>
codegraph status <repository>
```

Use an incremental refresh for normal maintenance explicitly requested by the
user:

```bash
csgraph sync --config <manifest> --database <database> <workspace>
```

That command synchronizes initialized CodeGraph indexes by default. Do not add
`--no-codegraph` when validating an enabled integration. For an intentional full
graph recomputation, use:

```bash
csgraph scan --config <manifest> --database <database> --codegraph --force <workspace>
```

Run `status` and `doctor` again after either operation. Inspect structured JSON
when exact counts or diagnostics matter.

## Exclusions and limits

Set `useGitignore: true` on a repository entry or in local configuration when
tracked `.gitignore` rules should apply. Use explicit `excludes` for protected
or workspace-specific exclusions, which continue to win over `.gitignore`
negations. Confirm the effective policy and its origin with `csgraph config show
--repo <alias>` and the next scan's discovered-input counts.

Exclude generated and non-source content such as build output, virtual
environments, caches, rendered documentation, binary media, test reports, and
secret-bearing local environment files. Keep graph databases and tool indexes
out of source discovery.

For a scan-limit failure:

1. Capture the exact resource, repository, file, and reported limit.
2. Decide whether the artifact is generated, irrelevant, malformed, or valid
   source evidence.
3. Exclude generated or irrelevant artifacts at the narrowest stable pattern.
4. Split or correct malformed source data when that preserves its contract.
5. Increase only the documented matching budget when the valid artifact must
   remain indexed.
6. Use the smallest sufficient value, rescan, and record the residual risk.

Do not increase global limits merely to hide an unknown input. Do not bypass a
fixed tool cap; report it as an evidence limitation.

## Repository onboarding

When adding a repository to the bound workspace:

1. Confirm it is the intended Git repository and read its instructions.
2. Initialize CodeGraph only if repository indexing is desired and supported:
   `codegraph init <repository>`.
3. Add one stable alias and path to the workspace manifest.
4. Decide explicitly whether to enable `useGitignore`; add narrower explicit
   exclusions when workspace policy requires them.
5. Run `csgraph config show --repo <alias>` before scanning.
6. Run an incremental sync, or a first `scan --codegraph` when no snapshot
   exists.
7. Validate Code System Graph status, CodeGraph status, repository counts, and
   at least one meaningful query.
8. Exercise one trace, impact, or contract check if the repository has an
   expected connection to another repository.

An index with zero supported source files can be legitimate. Record the
language or content limitation instead of claiming missing files were indexed.

## Freshness interpretation

Keep these states separate:

- A Code System Graph snapshot can be structurally healthy but stale relative
  to Git or filesystem changes.
- A CodeGraph index can have zero pending indexed-language files while the Git
  worktree is still dirty because of unsupported or excluded files.
- A repository can be registered but contribute no nodes or evidence.
- Repository source coverage can be bounded or capped without invalidating the
  workspace snapshot.

Report the exact layer and evidence. Avoid collapsing all four into one
"synchronized" result.
