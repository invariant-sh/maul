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
- Treat `reliability_report.json` as potentially sensitive if paths or payloads were logged in future versions; today it stores path/status/latency/fault labels only.
- Rotate any key that may have been pasted into a shell history, issue, or commit by mistake.
