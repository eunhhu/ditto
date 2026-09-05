# Task 009: Explicit personal-agent request path

## Observable contract

The user can submit a request from the CLI, inspect its durable status, and
cancel it. The daemon explicitly composes its configured model with the existing
bounded artifact-read loop. Relevant current session memory reaches the model;
a direct answer does not require a tool call. See [ADR 0016](../../adr/0016-explicit-agent-runs.md).

## Failure model

Default-disabled execution must not invoke a model. Duplicate requests must not
repeat model work, changed retry intent must conflict, and an occupied runtime
must reject extra work without a hidden queue. Disconnect, cancellation, daemon
shutdown, storage failure, and restart must preserve honest state. Public input
cannot supply a provider, key, actor, effect, trusted context, or turn metadata.
Memory corrections and scope isolation must survive actual model composition.

## Exit criteria

1. Typed CLI/HTTP submission, lookup, cancellation, and disabled-provider errors.
2. Kernel-owned durable admission, bounded single-run lifetime, exact retry
   identity, terminal inspection after restart, and explicit interrupted state.
3. Corrected relevant memory, direct answer, bounded artifact-read continuation,
   and replay demonstrated with deterministic model fixtures.
4. Concurrency, unknown fields, invalid identity, scope, failure, cancellation,
   and graceful shutdown verified without paid calls.
5. Canonical gate, MSRV, actual CLI/daemon smoke, and factual evidence/handoff.

General local process tools, scheduling, GUI, production embeddings, automatic
cross-turn transcript/context extraction, and verified task completion remain
separate slices.
