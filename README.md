# Maul

**Adversarial HTTP proxy for LLM agent reliability.**

Maul sits between your agent and an OpenAI-compatible provider, so you can test how the agent behaves under realistic failure — without changing agent code or locking into a framework SDK.

```text
Agent  →  http://localhost:7777 (Maul)  →  real LLM base_url
```

> HTTP is the universal agent API surface. A proxy gives fault injection and observability with zero SDK lock-in.

---

## Why Maul?

Every serious agent stack (CrewAI, LangGraph, AutoGen, raw HTTP) eventually speaks OpenAI-compatible HTTP. That seam is the right place to:

- **Inject faults** — 5xx, 429 rate limits, and malformed tool calls
- **Enforce budgets** — call caps and observed token-based spend caps
- **Produce evidence** — deterministic request records and run summaries

Maul is built for **pre-deploy and CI**: spin up, run adversarial scenarios, emit a reliability report, tear down.

It is **not** a production policy gateway. That role belongs to sibling products (e.g. Vigil). Maul proves resilience; production enforces the policies those tests reveal.

---

## Status

**v0.1** — OpenAI-compatible reverse proxy, seeded fault injection, observed budget enforcement, a versioned shutdown report, and a CI orchestration command.

| Capability | Status |
|---|---|
| OpenAI-compatible reverse proxy | ✅ |
| Streaming response pass-through | ✅ |
| Hop-by-hop header filtering | ✅ |
| YAML config + seeded reproducibility | ✅ |
| `force_500` short-circuit fault | ✅ |
| `force_429` short-circuit fault | ✅ |
| `malformed_tool_call_json` (MutateAfter) | ✅ |
| `reliability_report.json` on shutdown | ✅ |
| `max_llm_calls` and observed cost budgets | ✅ |
| `maul test` CI orchestration | ✅ |
| Session correlation and inferred recovery scoring | 🚧 planned |
| Control plane (`/__maul/*`) + Python CLI | 🚧 planned |

---

## Quick start

### Requirements

- Rust toolchain (edition 2024 / recent stable)
- An OpenAI API key (or any OpenAI-compatible upstream)
- An agent or HTTP client that can set `base_url`

### Configure

```bash
cp maul.example.yaml maul.yaml
# edit upstream_base_url / listen address as needed
```

`maul.yaml` is gitignored. The API key is **not** in config — clients send `Authorization`; Maul forwards it.

### Run

```bash
cargo run -- --config maul.yaml
```

Validate configuration without starting the proxy:

```bash
cargo run -- --config maul.yaml --validate
```

### Smoke test

```bash
curl http://127.0.0.1:7777/v1/chat/completions \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4o-mini",
    "messages": [{"role": "user", "content": "Say hi in one sentence."}]
  }'
```

### Use from an SDK

```python
from openai import OpenAI
import os

client = OpenAI(
    base_url="http://127.0.0.1:7777/v1",
    api_key=os.environ["OPENAI_API_KEY"],
)
```

Same idea in TypeScript, LangChain, LiteLLM, etc.: point `base_url` at Maul.

---

## How it works

1. Agent calls Maul as if it were the provider.
2. Maul classifies `POST /v1/chat/completions` as billable, validates its request, and applies budget admission before fault injection.
3. Depending on `maul.yaml`, Maul may short-circuit, mutate the response, or pass through unchanged — including streamed SSE.
4. Maul meters returned token usage for priced models and writes a versioned `reliability_report.json` on shutdown.

**Important boundary:** Maul sees traffic on the LLM `base_url`. It does not sit on tool HTTP by default. Request-side tool-result poisoning is future work; the current response mutator targets `tool_calls` returned by the LLM.

**Scope (v0.1):** OpenAI-compatible APIs only. Anthropic’s native `/v1/messages` is out of scope until an adapter exists (compat gateways work today).

---

## Configuration

See [`maul.example.yaml`](./maul.example.yaml).

| Field | Purpose |
|---|---|
| `proxy_listen` | Bind address (example default `127.0.0.1:7777`) |
| `upstream_base_url` | Real provider / gateway |
| `scenarios` | Active fault scenarios (e.g. `force_500`, `malformed_tool_call_json`) |
| `probability` | Per-request injection chance |
| `seed` | Reproducible chaos (same seed → same decisions) |
| `budget` | `max_llm_calls` plus observed `max_cost_usd` |
| `model_prices` | Optional USD-per-million-token overrides for custom model IDs |

Only `POST /v1/chat/completions` consumes the call budget. `max_cost_usd` is an observed-spend cap: usage is known after a response, so concurrent requests can exceed it. Unknown or interrupted usage is reported as unavailable, never as zero cost.

---

## Roadmap

1. ~~Proxy, deterministic faults, budgets, versioned report, and `maul test`~~ — done for v0.1
2. **Session correlation** — make retry/recovery summaries attributable to a run
3. **Scenario packs** — request-side tool-result poisoning and additional response faults
4. **Control plane** — `/__maul/run|report|reset` for long-lived local sessions
5. **Provider adapters** — native protocols beyond OpenAI-compatible HTTP

Maul measures **behavior under failure**. Task correctness belongs in **Holds** (eval harness), not in the proxy.

---

## Security

- Treat Maul as a **local / CI chaos tool**, not a public edge proxy.
- Maul **forwards** `Authorization`; **never log** that header (or bodies that may contain secrets).
- Keep real keys in the environment; do not commit `maul.yaml` with sensitive overrides.

See [`SECURITY.md`](./SECURITY.md) for the full policy and how to report vulnerabilities.

---

## Contributing

Issues and PRs welcome. Prefer small, reviewable changes that keep `main` boring and logic in modules (`proxy/`, `fault/`, `budget`, `report`).

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
```

CI runs the same checks on every push/PR to `main` (see `.github/workflows/ci.yml`).

**Test layout:**

```text
tests/
  budget.rs          # atomic admission, cost accounting, and concurrency
  budget_types.rs    # exact micro-USD conversions
  headers.rs         # hop-by-hop + Accept-Encoding:identity + Content-Encoding strip
  upstream.rs        # URL builder
  config.rs          # YAML load / error paths
  fault.rs           # scenarios/seed + JSON/SSE/gzip mutator edge cases
  mutate_after.rs    # handle/apply_mutate_after vs wiremock (gzip client, SSE, force_500)
  openai.rs          # route, model, and error-envelope contracts
  pipeline.rs        # budget/fault/usage end-to-end behavior
  pricing.rs         # registry and override behavior
  properties.rs      # cost and transform properties
  report.rs          # collector flush → JSON
  reverse_proxy.rs   # pass-through + identity encoding vs wiremock
  usage.rs           # JSON/SSE usage and request transforms
```

Production code stays in `src/` without embedded test modules. The crate is a **library + thin binary** so `tests/*` can call `maul::proxy` / `maul::config` like any other consumer.

### Demo `force_500`

```bash
cp maul.example.yaml maul.yaml
# set:
#   scenarios: [force_500]
#   probability: 1.0
#   seed: 42
cargo run
```

In another terminal:

```bash
curl -sS http://127.0.0.1:7777/v1/chat/completions \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}'
```

**What you should see**

```text
# Maul log
WARN maul::fault: injecting fault scenario="force_500" ...

# curl
HTTP/1.1 500 Internal Server Error
{"error":{"message":"maul: injected fault force_500","type":"server_error","code":"force_500"}}
```

After Ctrl+C on Maul, `reliability_report.json` should show `faults_injected >= 1`.

### CI mode

Run an agent command against an isolated Maul instance:

```bash
maul test \
  --config maul.yaml \
  --agent "python app/agent.py" \
  --report artifacts/reliability_report.json \
  --fail-on resilience,cost
```

The command sets `MAUL_BASE_URL` and `OPENAI_BASE_URL`, uses an ephemeral loopback port, flushes the same report artifact, and distinguishes agent failures from threshold failures. It does not capture prompts or credentials by default.

## Report artifact

The report uses schema version `0.1` and separates `total_proxy_requests` from
`billable_llm_calls`. Each request record includes its admission decision, model,
fault, usage outcome, and integer `cost_usd` in micro-USD. The top-level report
also includes the budget snapshot, pricing registry version, and a process-run
summary. Maul does not infer task correctness or claim per-agent recovery
without session correlation; those responsibilities belong to Holds.

### Demo `malformed_tool_call_json`

```yaml
scenarios: [malformed_tool_call_json]
probability: 1.0
seed: 42
```

Point a tool-calling agent (or the `python_test` demos) at Maul. Upstream still runs; Maul rewrites
`tool_calls[].function.arguments` to invalid JSON (`{maul:not-json`) on the way back so the agent’s
tool loop has to recover or fail. Works for non-streaming JSON **and** SSE (`text/event-stream`) —
CrewAI/LangGraph often stream by default.

---

## Releases & packages

| Channel | What it is | For Maul |
|---|---|---|
| **GitHub Releases** | Version tags + notes (+ optional binaries) | Primary distribution for the `maul` binary |
| **crates.io** | Rust library/binary registry (“packages”) | Later, once LICENSE + API are stable |
| **GitHub Packages** | GH’s generic package host | Usually skip for Rust CLIs |

Until a stable tagged release is published, install from source: `cargo install --git https://github.com/invariant-sh/maul.git`.

---

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).

---

## Related

Part of the [Invariant](https://github.com/invariant-sh) tooling family for trustworthy agent systems:

| Tool | Role |
|---|---|
| **Maul** (this repo) | Adversarial proxy — prove resilience under failure |
| **Holds** | Task / eval harness — did the agent solve the job? |
| **Vigil** | Production controls — enforce policy at the edge |

Install from source: `cargo install --git https://github.com/invariant-sh/maul.git`.