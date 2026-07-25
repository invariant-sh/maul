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

**Early v0.1** — streaming pass-through proxy works today. Fault injection, budgets, scoring, and the control plane are on the roadmap.

| Capability | Status |
|---|---|
| OpenAI-compatible reverse proxy | ✅ |
| Streaming (SSE) pass-through | ✅ |
| Hop-by-hop header filtering | ✅ |
| YAML config + seeded reproducibility | ✅ (seed wired for upcoming chaos) |
| Fault scenarios | 🚧 planned |
| Budget / cost safety | 🚧 planned |
| `reliability_report.json` | 🚧 planned |
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
3. Responses stream back unchanged (today).
4. Later: Maul may short-circuit, mutate responses, or poison inbound tool results per `maul.yaml` scenarios — then record behavior into a score card.

**Important boundary:** Maul sees traffic on the LLM `base_url`. It does not sit on tool HTTP by default. Tool-result poisoning targets `role: "tool"` messages in the chat request; mutating `tool_calls` targets the LLM response.

**Scope (v0.1):** OpenAI-compatible APIs only. Anthropic’s native `/v1/messages` is out of scope until an adapter exists (compat gateways work today).

---

## Configuration

See [`maul.example.yaml`](./maul.example.yaml).

| Field | Purpose |
|---|---|
| `proxy_listen` | Bind address (default `0.0.0.0:7777`) |
| `upstream_base_url` | Real provider / gateway |
| `scenarios` | Faults to enable (when implemented) |
| `probability` | Per-request injection chance |
| `seed` | Reproducible chaos (same seed → same decisions) |
| `budget` | Call / cost caps (when enforced) |

---

## Roadmap

1. **Report + budget atomics** — latency/calls now; score card on shutdown  
2. **One end-to-end fault** — e.g. `force_500` (short-circuit) or `malformed_tool_call_json` (mutate-after)  
3. **Session correlation** — unlock real resilience scoring from subsequent traffic  
4. **Scenario packs** — response mutation → short-circuit → experimental systemic faults  
5. **Control plane + Python CLI** — `/__maul/run|report|reset` without process restarts  
6. **OSS hardening** — tests (wiremock), CI, `SECURITY.md`, typed errors  

Maul measures **behavior under failure**. Task correctness belongs in an eval harness (e.g. invariant-eval), not in the proxy.

---

## Security

- Treat Maul as a **local / CI chaos tool**, not a public edge proxy.
- Never log `Authorization` or bodies that may contain secrets.
- Keep real keys in the environment; do not commit `maul.yaml` with sensitive overrides.

A full `SECURITY.md` will land with the OSS quality pass.

---

## Contributing

Issues and PRs welcome once the fault-injection path is scaffolded. Prefer small, reviewable changes that keep `main` boring and logic in modules (`proxy/`, `fault/`, `budget`, `report`).

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
```

CI runs the same checks on every push/PR to `main` (see `.github/workflows/ci.yml`).

---

## Releases & packages

| Channel | What it is | For Maul |
|---|---|---|
| **GitHub Releases** | Version tags + notes (+ optional binaries) | Primary distribution for the `maul` binary |
| **crates.io** | Rust library/binary registry (“packages”) | Later, once LICENSE + API are stable |
| **GitHub Packages** | GH’s generic package host | Usually skip for Rust CLIs |

Until the first tagged release, install from source: `cargo install --git https://github.com/invariant-sh/maul.git`.

---

## License

TBD — license file will be added before the first tagged release.

---

## Related

Part of the [Invariant](https://github.com/invariant-sh) tooling family for trustworthy agent systems: adversarial testing (Maul), production controls (Vigil), and task evaluation (invariant-eval).
