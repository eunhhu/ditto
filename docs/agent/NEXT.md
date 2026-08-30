# Implementation frontier

Work in order unless the user explicitly changes priorities. Complete one task
file's exit criteria before moving the marker.

## Active — 003 Read-only tool continuation loop

Task file: [`tasks/003-read-only-tool-loop.md`](tasks/003-read-only-tool-loop.md)

Goal: page a full `artifact.read` schema, accept a structured tool call, execute
the builtin read through policy-free read authority, return the result, and
continue the same model epoch. Completion remains explicitly unverified unless
a task-specific verifier exists.

## Completed

- [`002 First frontier provider`](tasks/002-first-provider.md): closed
  `gpt-5.6` OpenAI Responses adapter with fixed-origin HTTPS, external redacted
  credentials, deterministic prompt and schema projection, bounded SSE
  correlation, text/tool/structured-output streaming, exact optional usage and
  terminal semantics, cancellation/deadline propagation, pre-response-only
  retry, explicit remote-storage policy, and response-ID continuation.
- [`001 Provider-neutral model IR`](tasks/001-model-ir.md): versioned request and
  validated stream contract, compact context and Draft 2020-12 schema
  projections, typed generation/replay capabilities, deterministic fixture
  driver, cancellation/deadline handling, correlated tool and reasoning
  lifecycles, usage/continuation, and distinct OpenAI/Anthropic source-shape
  coverage.

## Later

- durable context projection and shared lexical/embedding task signature;
- canonical capability invocation and effect derivation;
- device registry and local process worker;
- SSH as placement transport;
- gateway inspector and approval UX;
- evidence-gated improvement compiler.
