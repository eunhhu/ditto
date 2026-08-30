# Contributing to Ditto

Ditto is intentionally small at the center. Contributions must preserve the
distinction between the semantic microkernel and isolated capabilities.

Read `AGENTS.md` first. Long-running contributors and coding agents continue
from `docs/agent/NEXT.md` and keep `docs/agent/HANDOFF.md` factual.

## Before opening a change

1. Prefer a typed event or rebuildable projection over hidden process-local state.
2. Prefer a manifest and isolated worker over importing integration code into the daemon.
3. Do not add a model call for bookkeeping that can be deterministic.
4. Keep credentials opaque to the model and out of event payloads.
5. Derive effects from canonical arguments; never trust a model's self-reported effect.
6. Include replayable evidence for changes to retrieval, policy, completion, or improvement behavior.
7. Build one complete vertical slice rather than several disconnected placeholders.

## Local gate

```bash
./scripts/agent-check.sh
```

The declared minimum Rust version is tested separately in CI. Executable
artifacts use a committed `Cargo.lock`; CI must use `--locked` once present.

Architecture changes require an ADR under `docs/adr/`. Wire-format changes must
update the matching specification under `docs/specs/`.
