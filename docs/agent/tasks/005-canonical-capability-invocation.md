# Task 005: Canonical capability invocation and effect/resource authority

## Status

Active under
[ADR 0012](../../adr/0012-canonical-capability-invocation.md).

## Objective

Close the model-to-policy authority hole with one complete local vertical
slice: turn an authority-free model tool call into an exact revision-bound,
schema-validated, capability-derived `CanonicalInvocation`, authorize it
atomically, issue a sealed invocation-bound `InvocationPermit`, and migrate the
existing `artifact.read` turn through that path without changing Task 003
execution or replay semantics.

## Required vertical slice

1. Add strict `UntrustedToolCall` input containing only source call ID,
   capability ID, and raw JSON arguments. Reject all unknown authority fields.
2. Bind invocable execution-epoch entries to capability ID/version, canonical
   manifest digest, complete schema digest, and deriver revision. Reject legacy
   discovery-only cards for live invocation.
3. Add fixed-budget Draft 2020-12 instance evaluation before normalization and
   after normalization.
4. Add the closed Task 005 deriver path. `artifact.read` normalizes the existing
   checked reference/range and derives the exact local content-read effect,
   typed artifact resource, and local-builtin placement. Reject effects below
   the manifest minimum or above its maximum and resources outside its declared
   family.
5. Seal `CanonicalInvocation`: private fields, no `Deserialize`, no unchecked
   constructor, exact invocation digest and idempotency binding.
6. Replace caller-carried leases with a process-local atomic authorizer that
   selects trusted static policy or a harness-selected lease. Return either a
   sealed permit, a sealed approval-required outcome, or typed denial.
7. Bind each permit to the invocation digest and expiry. Make authorization
   idempotent, conflict-detecting, and single-consumption under concurrency.
8. Route the existing Task 003 `artifact.read` live read through exact
   canonicalization, same-scope static policy, and a matching permit. Keep its
   event payload version, event ordering, authorization cutoff, results,
   continuation, replay, and unverified terminal unchanged.
9. Add unit, adversarial, concurrency, integration, and compile-fail tests; run
   focused checks, the canonical repository gate, and Rust 1.88; record only
   reproduced evidence before moving the frontier.

## Constraints

- The model and every serialized tool-call input have no effect, resource,
  device, program, placement, lease, approval, verification, credential,
  evidence, permit, or idempotency authority.
- Canonicalization resolves exactly the capability revision paged into the
  execution epoch. ID-only or current-catalog-only lookup is insufficient.
- Raw and normalized instances pass the same exact schema. Derivation is
  deterministic, bounded, capability-specific, and I/O-free.
- Derived effect is within both manifest bounds. Typed resources are matched to
  declared resource families; policy does not authorize raw string prefixes.
- Path primitives reject traversal, sibling-prefix, control/backslash, and
  non-NFC aliases without touching a filesystem. They do not authorize a path
  executor in this task.
- Authorization binds invocation ID to digest before a decision. Failures and
  approval-required outcomes consume no lease. Permit insertion and lease
  decrement are one critical section; successful retry consumes at most once.
- `artifact.read` approval is statically `never` only after exact same-scope
  event evidence is established. It receives a sealed static-policy permit,
  not a special bypass and not a model-selected lease.
- Existing Task 003 replay never performs canonicalization, policy, or artifact
  I/O. It validates the same durable version-1 transcript as before.

## Non-goals

- No process or worker spawn, runtime loading, network access, SSH, sudo,
  credential/secret resolution, device/program execution, file mutation, MCP,
  or remote placement.
- No approval fulfillment, approval UI, durable permit/lease store,
  cross-process authorization transaction, executor protocol, completion
  verifier, or `task.completed` event.
- No invocation support for `device.process.run` or any capability other than
  the existing bounded `artifact.read` reference slice.
- No expansion of ADR 0011, retrieval behavior, context projection, embedding
  behavior, or Task 004 resource accounting.
- No change to the valid `artifact.read` capability version, JSON wire schema,
  range/result projection, event payload version, or replay outcome.

## Exit criteria

- Strict wire tests show every forbidden authority field is rejected and the
  accepted shape contains only call ID, capability ID, and arguments.
- Compile-fail tests prove both sealed types cannot be struct-literal
  constructed or used as `DeserializeOwned`.
- Epoch tests reject absent page-in and each version, manifest digest, schema
  digest, and deriver revision mismatch.
- Adversarial schema tests reject malformed JSON values before policy and
  reject a deriver normalization outside the schema.
- Effect tests reject a derived profile below minimum and above maximum.
- Resource tests derive the exact artifact identity and reject parent
  traversal, sibling-prefix paths, and canonically equivalent non-NFC paths.
- Policy tests prove failure does not consume, success consumes once,
  idempotent retry returns the same permit, digest conflict fails closed,
  approval-required consumes nothing, and a concurrent one-call race issues at
  most one permit.
- Kernel Task 003 tests prove valid read, invalid arguments, unauthorized
  reference, cancellation checkpoints, continuation, reopen replay, and final
  event/result behavior remain unchanged while execution requires a matching
  canonical invocation and static permit.
- Focused tests, strict Clippy, `./scripts/agent-check.sh`,
  `cargo +1.88.0 check --locked --workspace --all-targets`, and diff hygiene
  pass. Reproducible evidence is tracked before the task is marked complete.

