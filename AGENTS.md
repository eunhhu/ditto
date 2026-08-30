# Ditto agent operating contract

This file applies to the whole repository. More specific `AGENTS.md` files add
rules for their directories.

## Mission

Build Ditto as a local-first semantic agent microkernel:

> Context is compiled. Capabilities are paged. Effects are leased. Improvements
> are promoted.

The model owns intent, strategy, and judgment. The harness owns context,
capabilities, effects, persistence, verification, and execution lifetime.

## Start every substantial task

1. Read `docs/agent/NEXT.md` and `docs/agent/HANDOFF.md`.
2. Read only the task-specific documents linked from `docs/agent/README.md`.
3. Inspect the implementation and tests that own the contract before editing.
4. Keep the requested scope; do not broaden the architecture merely because a
   related abstraction could be useful later.

Do not preload every document. The repository instructions intentionally use
progressive disclosure.

## Non-negotiable invariants

- No model call solely for housekeeping.
- No capability implementation loaded before use.
- No full capability catalogue in model context.
- No durable context without provenance and scope.
- No model inference represented as a user assertion.
- No public client choosing trusted event actors or internal event kinds.
- No side effect without a canonical, capability-derived effect profile.
- No credential material in model context, events, logs, or test fixtures.
- No privileged action without a bounded, expiring lease.
- No verified completion without contract-specific evidence.
- No permanent improvement from one successful trajectory.
- No periodic LLM heartbeat when an event can wake the task.
- No fake success path for a deferred subsystem.

## Autonomy boundary

For an implementation request, make in-scope local changes, run non-destructive
checks, fix failures, and update affected documentation without asking for
routine confirmation. Stop before external writes unrelated to the repository,
destructive operations, credentials, costs, or a material scope expansion.

Prefer a clear local decision plus an ADR over pausing on reversible engineering
choices. Ask only when two plausible interpretations would produce materially
different public contracts and the repository has no existing decision.

## Engineering loop

1. Define the observable contract and failure modes.
2. Implement one end-to-end vertical slice rather than many disconnected stubs.
3. Add a regression test for the central invariant and adversarial tests for
   trust boundaries.
4. Run `./scripts/agent-check.sh`.
5. Update `docs/agent/HANDOFF.md` with facts, not aspirations.
6. Update `docs/agent/NEXT.md` only when exit criteria actually pass.
7. Commit coherent changes. Do not mix unrelated cleanup into the slice.

If a check cannot run, record the exact command, reason, and remaining risk in
the handoff. Never translate “not run” into “passed”.

## Architecture change rule

Add or amend an ADR before changing any of these:

- source-of-truth ownership;
- public wire contracts;
- capability disclosure levels;
- effect or lease semantics;
- credential boundaries;
- completion verification;
- self-improvement promotion;
- mandatory infrastructure or runtime language.

## Definition of done

A task is done only when implementation, tests, documentation, and replayable
or inspectable evidence agree. Compilation alone is not completion. A model
stream ending is never task evidence.
