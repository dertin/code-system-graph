# Agent response evaluation (1.2.1)

The immutable 1.2.0 baseline is `artifacts/agent-response-examples/responses.json`.
`baseline.json` records its hash and provenance. The 42-case pilot includes the original
13 cases plus HTTP/data/event/test/document/service queries, pagination, status,
contracts, communities, impact, and local changes. Negative symbol/repository isolation,
adversarial strings, unavailable FTS and delivery budget boundaries also have Rust regressions.

```sh
cargo build --locked -p code-system-graph --bin csgraph
CSGRAPH_EVAL_OUT=evaluation/agent-responses/results-json CSGRAPH_EVAL_FORMAT=json \
  python3 evaluation/agent-responses/run_capture.py
CSGRAPH_EVAL_REUSE=1 CSGRAPH_EVAL_OUT=evaluation/agent-responses/results-markdown \
  CSGRAPH_EVAL_FORMAT=markdown python3 evaluation/agent-responses/run_capture.py
python3 evaluation/agent-responses/check_invariants.py \
  evaluation/agent-responses/results-json evaluation/agent-responses/results-markdown
```

Requires Git and CodeGraph 1.6.1. The runner discovers Cargo's actual target directory;
`CSGRAPH_EXAMPLES_BIN` overrides the binary. `.work/` contains disposable synthetic Git
repositories and indexes. `results*/` is ignored. Never run captures concurrently against
the same `.work/` database. Reuse the snapshot between formats. The runner preserves both
MCP channels, initialization instructions, fixture revisions and every executed continuation.
The checker compares all semantic leaf paths and values, not output sizes or visual goldens;
provider execution counts/retained-byte telemetry from separate calls are excluded explicitly.

For remote-model replay with a fixed installed Codex CLI and explicit model:

```sh
python3 evaluation/agent-responses/run_codex_eval.py \
  --baseline artifacts/agent-response-examples/responses.json \
  --markdown evaluation/agent-responses/results-markdown/responses.json \
  --json evaluation/agent-responses/results-json/responses.json \
  --output evaluation/agent-responses/results-llm-smoke \
  --model gpt-6-astra --repetitions 3
```

Use `--pilot --repetitions 5` without `--baseline` for the expanded corpus. This is remote
inference invoked locally, not local inference. Each repetition is a batch of independent
case questions, so tasks in the same model call may influence each other. A/B changes content
and format; B/C uses the same selected facts. Captured model usage includes the Codex wrapper
and response, not just payload tokens. The harness saves usage, events, latency, prompt hashes,
CLI/model identity and answers; unexpected tool execution fails the run. Repository content
is supplied as data and tools are prohibited by the prompt. Inspect answer correctness against
fixture evidence; `proves_bug=false` alone is not a task-success score.

This replay does not trace Themis's actual adapter or measure real 2–4-hop task completion.
Its token usage is an observation for this Codex invocation, not a tokenizer estimate or a
promise of savings in another host. Human blind review, paired confidence intervals and
Themis integration remain adoption gates. No ranking/filter/source-trimming heuristic is
adopted from the pilot alone. The legacy default remains until those gates are satisfied.
