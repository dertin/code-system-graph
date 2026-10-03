# Evaluation decision for 1.2.1

Date: 2026-10-03. **Keep `legacy` as the default.** Canonical Markdown and JSON are opt-in
schema 6 delivery modes. Markdown did not meet the planned 20% token-reduction gate.
No model was downloaded or fine-tuned. Codex CLI 0.160.0 invoked `gpt-6-astra` remotely.

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
- Rust regressions cover identifier fidelity, bilateral event evidence, exact symbol/callsite
  handoffs, repository isolation, empty versus unavailable FTS, adversarial strings and budgets.
- Local workspace validation: 1,093 tests passed, 7 ignored; nightly formatting/clippy, rustdoc with
  warnings denied, MSRV 1.96.0, cargo audit and cargo deny passed.

The replay batches can share context between cases and do not execute model-selected 2–4-hop
investigations. Blind human review, actual Themis adapter traces, end-to-end task-success and
host-specific token/latency evidence remain gates for changing the default. Ranking, adaptive
source trimming and navigation-ID heuristics are deferred. Oversized canonical responses fail
explicitly with recovery guidance instead of silently discarding evidence. This release delivers
the fidelity fixes and experimental canonical contract without claiming those adoption gates passed.
