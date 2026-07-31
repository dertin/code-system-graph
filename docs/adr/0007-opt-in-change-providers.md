# ADR 0007: Local-First Change and Opt-In Pull-Request Providers

## Status

Accepted.

## Context

Change analysis must understand local Git state and hosted pull requests without making network
access mandatory or leaking source, credentials, or private repository metadata. GitHub and
Bitbucket expose different pagination, review, CI, and rate-limit contracts.

## Decision

Local Git is the default change provider. Code System Graph invokes Git with direct arguments, an explicit
worktree, disabled external diff/color, bounded output, cancellation, and deadlines. It stores
structured file/hunk metadata and fingerprints, not diff bodies.

Hosted pull-request providers share one typed boundary. Code System Graph 1.0.0 includes GitHub and
Bitbucket Cloud implementations; Bitbucket Data Center remains a distinct provider identity when
its API contract is selected. Network access requires both configured provider enablement and
explicit consent on each request. No hosted provider is contacted during scan, status, search,
trace, impact, or local change analysis.

Authentication tokens are ephemeral, redacted, excluded from serialization, cache keys, errors,
logs, and persistence. Production transport requires HTTPS except explicitly configured loopback
mock servers used by contract tests. Responses are byte/item/time bounded.

Provider caches contain only structured metadata, changed-file summaries, CI/review state, ETags,
and expiration/rate-limit metadata. Patch/source bodies are discarded before cache insertion.
Rate-limit responses return retry guidance without long automatic sleeps.

Every change result fingerprints repository/worktree identity, HEAD/base/head, staged and worktree
state, workspace/contract registry identities, and analyzer versions. Any relevant mismatch marks
the analysis stale and requires recomputation.

## Consequences

- Code System Graph remains fully functional offline.
- GitHub and Bitbucket access is visible, consensual, bounded, and testable through mock transport.
- Hosted-provider differences remain adapter details rather than leaking into impact logic.
- Pull-request overlap and review order are recommendations with reasons, never automatic merge or
  mutation operations.
- Change providers do not create commits, branches, comments, reviews, or pull-request updates.
