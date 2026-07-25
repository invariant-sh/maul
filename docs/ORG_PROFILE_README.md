# Invariant

**Agent reliability infrastructure.** Find failures, measure quality, prevent incidents.

```text
Agent  →  Maul (chaos)  →  Holds (eval)  →  Vigil (production)
```

| Tool | What it does | Status |
|---|---|---|
| **[Maul](https://github.com/invariant-sh/maul)** | Adversarial HTTP proxy — inject LLM faults, enforce budgets, score resilience | Open source (v0.1) |
| **Holds** | Task / eval harness — did the agent actually solve the job? | Building in public |
| **Vigil** | Production gateway — enforce the policies Maul’s tests reveal | Building in public |

HTTP is the universal agent API surface. We instrument that seam — framework-agnostic, no SDK lock-in.

Website: [getinvariant.sh](https://getinvariant.sh) *(coming soon)*
