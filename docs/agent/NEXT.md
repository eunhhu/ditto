# Implementation frontier

Work in order unless the user explicitly changes priorities. Complete one task
file's exit criteria before moving the marker.

## Active — 001 Provider-neutral model IR

Task file: [`tasks/001-model-ir.md`](tasks/001-model-ir.md)

Goal: define a provider-neutral request and streaming event contract that can
represent text, structured tool calls, usage, continuation, cancellation, and
provider completion without collapsing provider-specific capabilities.

## Queued — 002 First frontier provider

Task file: [`tasks/002-first-provider.md`](tasks/002-first-provider.md)

Goal: connect one frontier provider through the model IR with deterministic
fixtures and cancellation, without a tool executor yet.

## Queued — 003 Read-only tool continuation loop

Task file: [`tasks/003-read-only-tool-loop.md`](tasks/003-read-only-tool-loop.md)

Goal: page a full `artifact.read` schema, accept a structured tool call, execute
the builtin read through policy-free read authority, return the result, and
continue the same model epoch. Completion remains explicitly unverified unless
a task-specific verifier exists.

## Later

- durable context projection and shared lexical/embedding task signature;
- canonical capability invocation and effect derivation;
- device registry and local process worker;
- SSH as placement transport;
- gateway inspector and approval UX;
- evidence-gated improvement compiler.
