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

- **Inject faults** — timeouts, 5xx, malformed tool calls, poisoned tool results
- **Enforce budgets** — max LLM calls / spend before a runaway loop burns money
- **Score resilience** — did the agent retry, loop, give up, or recover?

Maul is built for **pre-deploy and CI**: spin up, run adversarial scenarios, emit a reliability report, tear down.

It is **not** a production policy gateway. That role belongs to sibling products (e.g. Vigil). Maul proves resilience; production enforces the policies those tests reveal.

---

## Status

**Early v0.1** — OpenAI-compatible reverse proxy with seeded fault injection and a shutdown reliability report. Budgets, richer scoring, and a control plane are on the roadmap.

| Capability | Status |
|---|---|
| OpenAI-compatible reverse proxy | ✅ |
| Streaming response pass-through | ✅ |
| Hop-by-hop header filtering | ✅ |
| YAML config + seeded reproducibility | ✅ |
| `force_500` short-circuit fault | ✅ |
| `malformed_tool_call_json` (MutateAfter) | ✅ |
| `reliability_report.json` on shutdown | ✅ |
| Budget enforcement | 🚧 planned |
| More fault scenarios | 🚧 planned |
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
cargo run
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
2. Maul forwards method, path, headers (minus hop-by-hop), and body to the real upstream.
3. Depending on `maul.yaml`, Maul may short-circuit, mutate the response (e.g. malformed tool-call JSON), or pass through unchanged — including streamed SSE.
4. Metrics land in `reliability_report.json` on shutdown.

**Important boundary:** Maul sees traffic on the LLM `base_url`. It does not sit on tool HTTP by default. Tool-result poisoning targets `role: "tool"` messages in the chat request; mutating `tool_calls` targets the LLM response.

**Scope (v0.1):** OpenAI-compatible APIs only. Anthropic’s native `/v1/messages` is out of scope until an adapter exists (compat gateways work today).

---

## Configuration

See [`maul.example.yaml`](./maul.example.yaml).

| Field | Purpose |
|---|---|
| `proxy_listen` | Bind address (default `0.0.0.0:7777`) |
| `upstream_base_url` | Real provider / gateway |
| `scenarios` | Active fault scenarios (e.g. `force_500`, `malformed_tool_call_json`) |
| `probability` | Per-request injection chance |
| `seed` | Reproducible chaos (same seed → same decisions) |
| `budget` | Call / cost caps (when enforced) |

---

## Roadmap

1. ~~Report + budget atomics~~ / ~~`force_500`~~ / ~~`malformed_tool_call_json`~~ — done for v0.1 alpha  
2. **Budget enforcement** — `max_llm_calls` / cost caps → short-circuit runaway loops  
3. **Session correlation** — unlock real resilience scoring from subsequent traffic  
4. **Scenario packs** — more response mutation / short-circuit / experimental systemic faults  
5. **Control plane + Python CLI** — `/__maul/run|report|reset` without process restarts  
6. **OSS hardening** — typed errors, richer score card

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
  headers.rs         # hop-by-hop + Accept-Encoding:identity + Content-Encoding strip
  upstream.rs        # URL builder
  config.rs          # YAML load / error paths
  fault.rs           # scenarios/seed + JSON/SSE/gzip mutator edge cases
  mutate_after.rs    # handle/apply_mutate_after vs wiremock (gzip client, SSE, force_500)
  report.rs          # collector flush → JSON
  reverse_proxy.rs   # pass-through + identity encoding vs wiremock
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
maul: injected fault force_500
```

After Ctrl+C on Maul, `reliability_report.json` should show `faults_injected >= 1`.

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