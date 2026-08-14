# Contributing to Maul

By participating you agree to the [Code of Conduct](./CODE_OF_CONDUCT.md).
Org-wide process lives in [invariant-sh/.github](https://github.com/invariant-sh/.github/blob/main/CONTRIBUTING.md).

## Setup

Requires a recent stable Rust toolchain (edition 2024).

```bash
cp maul.example.yaml maul.yaml
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo run -- --config maul.example.yaml --validate
```

Keep production code in `src/` and tests in `tests/`. Prefer small PRs that keep
logic in `proxy/`, `fault/`, `budget`, and `report`.

## Pull requests

Target `main`. CI (`.github/workflows/ci.yml`) must be green. Do not commit
`maul.yaml`, API keys, or request/response bodies.

## Security

Report vulnerabilities privately. See [SECURITY.md](./SECURITY.md).
