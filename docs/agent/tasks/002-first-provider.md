# Task 002 — First frontier provider

## Objective

Implement one production frontier-provider adapter behind Task 001's model IR.
Use the provider's official current API documentation while implementing it.

## Required behavior

- streaming text and structured tool-call events;
- request cancellation and deadline propagation;
- usage reporting;
- opaque continuation support when the provider offers it;
- prompt-cache-stable request construction;
- retry only before observable output unless the provider supplies a safe
  idempotent continuation mechanism;
- credentials resolved outside request/event serialization.

## Non-goals

- no tool execution;
- no automatic fallback to another provider;
- no hidden model router;
- no completion verification.

## Acceptance tests

Use a deterministic mock transport for all CI tests. Optional live tests must be
explicitly enabled and must never log secrets. Cover malformed SSE/JSON,
provider error frames, cancellation, partial tool arguments, finish reasons,
usage, and continuation.
