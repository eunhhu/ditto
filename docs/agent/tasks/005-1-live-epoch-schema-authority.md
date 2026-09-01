# Task 005.1: Live epoch, closed schema profile, and bounded authority lifetime

## Status

Reopened for the final pre-merge review closure under
[ADR 0012](../../adr/0012-canonical-capability-invocation.md). The prior tracked
[verification evidence](005-1-evidence.md) remains historical until the new
exit criteria below are reverified.

## Objective

Close the remaining pre-merge authority gaps in Task 005 without starting the
compact session-index task or an effectful worker: make replay evidence
incapable of minting live bindings, replace the over-broad Draft 2020-12
evaluator claim with a closed exact Ditto invocation profile, scope policy state
to one live epoch, and define a one-shot execution claim for future workers.

## Required vertical slice

1. Replace the dual-purpose epoch with serializable/deserializable
   `ExecutionEpochEvidence` and sealed, non-deserializable
   `LiveExecutionEpoch`.
2. Permit only a live epoch to issue a sealed `InvocableCapabilityBinding` that
   owns the exact epoch ID, model-visible card, complete schema, manifest, and
   revision. `InvocationCompiler` accepts only that binding.
3. Preserve the version-1 Task 003 selected-capability wire projection and
   legacy replay while proving deserialized evidence cannot authorize live
   work.
4. Add Ditto Invocation Schema Profile V1. Perform iterative byte/depth/work
   preflight before recursive schema validation, reject unsupported keywords
   and types, and evaluate its closed semantics deterministically.
5. Use representation-sensitive recursive JSON equality for `const`, `enum`,
   and `uniqueItems`; use exact `i128` arithmetic for admitted integer bounds
   and `multipleOf`; reject syntactic floats such as `1.0` for `integer`.
6. Make `InvocationAuthorizer` borrow one live epoch, reject other or expired
   epochs, cap outcomes at epoch expiry, and remove it from daemon-lifetime
   kernel state. Preserve the atomic one-call lease transaction and retry
   behavior inside the epoch window.
7. Add a sealed, non-cloneable, non-deserializable `ExecutionClaim`. Claiming a
   permit is atomic and succeeds at most once; define future effectful workers
   as claim-by-value consumers, but add no worker or dispatch path.
8. Keep `artifact.read` arguments/results, invalid-argument projection, event
   version/order, same-scope authorization, continuation, terminal, and replay
   semantics unchanged.
9. Give `LiveExecutionEpoch` a monotonic paging/sealed state and issue exactly
   one private-field, non-cloneable, non-serializable, non-deserializable
   `EpochAuthorizationTicket`. Policy consumes that ticket; cloned authorizer
   handles share its sole ledger. Ticket/authorizer drop never rearms the epoch,
   and post-seal paging fails.
10. Charge every recursively visited value during `const`, `enum`, and
    `uniqueItems` equality. Preserve representation-sensitive number equality
    without allocating comparison strings. Nested equality that exceeds the
    fixed evaluation work limit must fail with `EvaluationWorkExceeded`.
11. Run iterative argument byte/depth/work preflight before recursive canonical
    serialization for both raw calls and normalized deriver output.
12. Make the compiler the only raw `artifact.read` normalizer. Map typed raw
    ingress/schema rejection to the existing Task 003 error projections and
    decode the compiler-sealed normalized value for execution.

## Non-goals

- No compact session index, context projection change, or retrieval work.
- No effectful worker, subprocess, network, SSH, credential, approval
  fulfillment, device/program execution, file mutation, verifier, or
  `task.completed` event.
- No full JSON Schema Draft 2020-12 evaluator claim. Provider-neutral schema
  disclosure remains separate from the closed invocation profile.
- No durable or cross-process authorization ledger.

## Exit criteria

- Compile-fail cases prove `LiveExecutionEpoch`,
  `InvocableCapabilityBinding`, and `ExecutionClaim` cannot be deserialized or
  publicly constructed; `ExecutionClaim` is not cloneable.
- Replay evidence round-trips and replays, but cannot satisfy the compiler's
  live-binding parameter.
- Tests reject mismatched card, schema, manifest revision, deriver revision,
  capability ID, and epoch identity before normalization or policy.
- Profile tests distinguish `1` from `1.0` for `const`, `enum`, and
  `uniqueItems`; compare integers beyond 2^53 exactly; enforce exact integer
  `multipleOf`; and reject over-depth/over-work schemas before recursive
  evaluation.
- `artifact.read` with `length = 1.0` remains the same invalid-arguments tool
  result and performs no artifact read or policy success.
- Epoch-scoped policy tests prove mismatch/expiry rejection, fixed outcome
  expiry, failed authorization without consumption, one success and one
  idempotent retry, one winner under a concurrent one-call lease, and no
  daemon-owned authorizer state.
- A permit produces at most one sealed execution claim; a second claim fails.
- A second authorization ticket fails before and after the first ticket or
  authorizer is dropped; post-seal page-in fails; compile-fail cases prove a
  ticket cannot be cloned, serialized, deserialized, publicly constructed, or
  moved into two independent authorizers.
- A nested `uniqueItems` case passes the complete-value and simple pair-count
  envelopes but exhausts recursively metered equality work.
- Raw and normalized deep/over-work values fail iterative preflight before
  canonical serialization.
- `artifact.read` runs exactly one raw normalization while malformed reference,
  negative/excessive/fractional arguments retain their Task 003 codes, events,
  continuation, and no-read behavior.
- Focused tests, strict Clippy, `./scripts/agent-check.sh`, Rust 1.88 workspace
  check, and diff hygiene pass; tracked evidence is committed and the branch is
  pushed before opening a PR for independent `rust` and `msrv` checks.
