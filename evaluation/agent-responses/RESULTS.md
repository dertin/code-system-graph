# Evaluation decision for 1.2.1

Date: 2026-10-03. The measured candidate did not justify switching to Markdown: it missed the
planned 20% token-reduction gate. The maintainer subsequently required a breaking release without
compatibility code. **The release now uses canonical JSON by default, with equivalent Markdown
optional and no legacy mode.** This is a maintenance/adoption decision, not a new model result.
No model was downloaded or fine-tuned. Codex CLI 0.160.0 invoked `gpt-6-astra` remotely.

The replay measured candidate `2266194` (the later legacy-only compatibility fix does not alter
canonical output). Subsequent review fixes add declaration-line isolation for consumer identities
and rename the unqualified entity field to `symbol_name`. The token numbers below describe the
measured candidate, not a new model run of those review fixes; these numbers are historical candidate measurements.

## Recorded measurements

Total tokens are API-reported input plus output for an entire batch, including the Codex
wrapper. They are not payload estimates or independent per-task costs. Cached input remains
included. Runs used the same questions; A/B also changes facts, whereas B/C selects the same facts.

| Experiment / arm | Cases per batch × repetitions | Mean total tokens | Latency p50 / p95 (s) | Automated evidence checks |
| --- | --- | ---: | ---: | ---: |
| Frozen 1.2.0 baseline | 13 × 3 | 57,064.7 | 74.85 / 90.34 | Not scored with current rubric |
| Initial canonical Markdown | 13 × 3 | 69,386.7 | 81.92 / 105.90 | Not scored with current rubric |
| Initial canonical JSON | 13 × 3 | 50,179.0 | 85.59 / 87.34 | Not scored with current rubric |
| Compact canonical Markdown | 13 × 3 | 58,932.0 | 75.12 / 95.72 | 39/39 |
| Compact canonical JSON | 13 × 3 | 43,917.3 | 72.33 / 74.37 | 39/39 |
| Expanded canonical Markdown pilot | 42 × 5 | 138,269.4 | 248.21 / 255.44 | 210/210 |
| Expanded canonical JSON pilot | 42 × 5 | 95,296.4 | 244.28 / 245.92 | 210/210 |

Compaction removes diagnostic fields and records omitted defaults while preserving uncertainty,
source, evidence and exact actions. On the original corpus, compact Markdown still used 3.3%
more tokens than baseline; JSON used 23.0% fewer. On the expanded corpus JSON used 31.1% fewer
than Markdown. These observations do not establish savings in another host or general task success.

All 420 pilot observations passed the narrow checks for supported repositories/paths, exact
suggested actions, selected required participants and absence of unsupported bug claims. No run
executed tools. Case-cluster bootstrap (42 cases, 10,000 resamples, seed 121) gives a B/C check-rate
difference of 0 with interval [0, 0]. A saturated automated rubric cannot establish semantic
noninferiority; this is not the plan's human task-success confidence interval.

[Machine-readable results](evaluation-results.json) retain run usage, latency, prompt/answer hashes
and individual checks. Full replay captures/events remain in ignored local `results*/` directories.
Use the README commands to collect new captures; `summarize_eval.py` reads the smoke, compact and
pilot directories used here. The model alias does not pin a provider snapshot; temperature and
tokenizer versions are not exposed by this CLI.

## Deterministic validation and adoption limits

- 42 live MCP cases have identical semantic leaves in Markdown and JSON, with one content channel,
  source parity, valid fixture file/range locators and whole-result budget checks.
- All 44 suggested continuations executed successfully against the same fixture snapshot.
- After removing compatibility, both formats were recaptured with the unified execution policy:
  all 42 cases and 44 continuations still pass at 65,536 bytes. This run also caught and fixed a
  generated-client label incorrectly exposed as a file path; only file-artifact identities or
  evidence now supply artifact paths.
- Rust regressions cover identifier fidelity, bilateral event evidence, exact symbol/callsite
  handoffs, repository isolation, empty versus unavailable FTS, adversarial strings and budgets.
- Local workspace validation after compatibility removal: 1,069 tests passed, 7 ignored; nightly
  formatting/clippy, rustdoc with warnings denied and MSRV 1.96.0 passed. The smaller test count
  reflects deletion of the retired renderer tests. Cargo audit and cargo deny passed during release validation.

The replay batches can share context between cases and do not execute model-selected 2–4-hop
investigations. Blind human review, actual Themis adapter traces, end-to-end task-success and
host-specific token/latency evidence remain pending before claiming semantic noninferiority or general efficiency. Ranking, adaptive
source trimming and navigation-ID heuristics are deferred. Oversized canonical responses fail
explicitly with recovery guidance instead of silently discarding evidence. The release adopts the single canonical contract by maintainer decision without claiming those quality gates passed.
