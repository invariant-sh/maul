# Changelog

## 0.1.0 — 2026-10-23

First stable release. `v0.1.0-alpha.1` was a source-only preview and has no binaries.

### Added

- OpenAI-compatible reverse proxy with streaming pass-through and hop-by-hop header filtering
- Seeded faults: `force_500`, `force_429`, and `malformed_tool_call_json`
- Atomic `max_llm_calls` admission and observed `max_cost_usd` budgets
- JSON and SSE usage parsing, with optional model price overrides
- Reliability report schema 0.2 (additive over 0.1), including session ids and a monotonic sequence
- `maul test` for CI, with `budget`, `cost`, and `resilience` thresholds
- Session-aware recovery scoring (`X-Maul-Session-Id`, JSON `user`, or `metadata.maul_session_id`)
- `setup-maul` and `maul-test` composite actions that install a pinned, checksum-verified binary
- GitHub Release archives and `.sha256` checksums for Linux x86_64, Linux arm64, macOS x86_64, and macOS arm64

### Install

Pin the action and the binary to `v0.1.0`. Asset names are `maul-v0.1.0-<target>.tar.gz`. Source install: `cargo install --git https://github.com/invariant-sh/maul.git --tag v0.1.0`.
