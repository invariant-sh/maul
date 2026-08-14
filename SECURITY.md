# Security Policy

## What Maul is (and is not)

Maul is a **local / CI adversarial proxy** for testing how LLM agents behave under failure.

It is **not** a public edge proxy, API gateway, or production policy layer. Do not expose Maul on the open internet. If you need production controls, that belongs to a sibling product (e.g. Vigil), not Maul.

## Credentials and forwarding

- Clients send `Authorization` (and similar) headers; **Maul forwards them upstream**.
- **Do not log** `Authorization`, API keys, cookies, or request/response bodies that may contain secrets or PII.
- Keep real keys in the environment (or a secret store). Prefer `maul.example.yaml` → local `maul.yaml` (gitignored). Never commit live keys or production `maul.yaml`.

## Reporting a vulnerability

If you believe you found a security issue in Maul:

1. **Do not** open a public GitHub issue with exploit details.
2. Open a [private security advisory](https://github.com/invariant-sh/maul/security/advisories/new) on this repository.
3. Include: Maul version/commit, reproduction steps, and impact.

We aim to acknowledge reports within a few business days.

## Safe defaults for operators

- Bind to loopback or a private CI network (`proxy_listen`), not `0.0.0.0` on a public host unless you fully trust the network.
- Treat `reliability_report.json` as potentially sensitive. v0.2 stores path/status/latency/fault labels, session ids, and run ids — not prompts or credentials. Keep GitHub Actions artifact retention short (the composite action defaults to 7 days).
- `X-Maul-Session-Id` is an internal correlation header. Maul strips it before forwarding upstream. Do not put secrets in that header, JSON `user`, or `metadata.maul_session_id`.
- The GitHub Action installs only Linux x86_64 and macOS arm64 binaries and verifies the published SHA256. Pin the action and binary to the same release tag; do not use `latest`.
- Do not pass provider secrets to fork pull requests. The action injects `openai-api-key` only into the spawned agent environment and never writes it to config, outputs, or reports.
- Rotate any key that may have been pasted into a shell history, issue, or commit by mistake.
