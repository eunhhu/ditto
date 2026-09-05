# Task 010: Bounded local process execution

## Observable contract

`ditto sort FILE [--unique]` submits a real local sort process, returns an
immutable output artifact and verified result, and supports request-ID status,
cancellation and retry. This is the first closed process profile; it does not
claim general shell execution or autonomous model dispatch.
See [ADR 0017](../../adr/0017-bounded-local-sort.md).

## Failure model

No client-selected executable, environment, effect, lease or completion.
No process without a matching, unexpired one-shot execution claim. No repeated
execution on retry/restart, unbounded output, abandoned child on cancellation,
or verified success from exit code alone. No model call for this operation.

## Exit criteria

1. Canonical capability/lease/claim to an actual lazily started OS process.
2. Bounded pipes, duration, private scratch, cleanup and explicit failure codes.
3. Independent line-contract verifier and inspectable durable result evidence.
4. Thin CLI/HTTP submission/status/cancel with shared kernel lifetime and retry.
5. Positive/negative process, authority, recovery and real binary tests; canonical
   gate, MSRV and factual handoff/evidence.
