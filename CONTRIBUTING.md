# Contributing to Ditto

Ditto is intentionally small at the center. Contributions should preserve the distinction between the semantic microkernel and isolated capabilities.

## Before opening a change

1. Prefer a typed event or projection over hidden process-local state.
2. Prefer a manifest and an isolated worker over importing integration code into the daemon.
3. Do not add a model call for bookkeeping that can be deterministic.
4. Keep credentials opaque to the model and out of event payloads.
5. Include replayable evidence for changes to retrieval, policy, or improvement behavior.

## Local checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Architecture changes should include an ADR under `docs/adr/`. Wire-format changes should update the matching specification under `docs/specs/`.
