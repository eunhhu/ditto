# ADR 0006: Repository-native agent operating contract

## Status

Accepted.

## Context

Long-running coding agents lose quality when all architecture, task history, and
procedures are repeatedly loaded into one prompt or left only in conversation
history. Ditto also needs autonomous implementation without encouraging broad,
unverified rewrites.

## Decision

Use a compact root `AGENTS.md` for stable invariants and autonomy boundaries.
Use scoped `AGENTS.md` files and `docs/agent` as a progressive-disclosure context
router. `NEXT.md` names one active vertical slice, task files define executable
exit criteria, and `HANDOFF.md` records verified state for recovery after
compaction or interruption.

The canonical repository gate is `./scripts/agent-check.sh`. Agents update
handoff and task state only after the corresponding checks pass.

## Consequences

- Agent context stays small while detailed guidance remains discoverable.
- Repository evidence, not chat memory, owns project state.
- Multiple overlapping plans and generated skills are discouraged.
- A stale handoff is treated as a defect, not as harmless prose.
