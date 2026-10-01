# ADR 0034: One turn contract until the first release

Status: accepted on 2026-10-01 for Task 033. Amends
[ADR 0009](0009-read-only-artifact-turn-loop.md) and every later ADR that kept
an earlier turn payload version replayable.

## Context

Each change to the turn contract added a payload version, and replay kept
every earlier one: eleven versions of instructions, context selection,
request and selection forms and failure grammar, each with its own branches
and tests. No Ditto data recorded under an earlier version exists: nothing
has been released, and no data directory is in use. On 2026-10-01 the user
asked for Ditto to be as light and thin as possible ("최대한 가볍고 얇게").

## Decision

- Until Ditto's first release there is exactly one turn contract,
  `TURN_PAYLOAD_VERSION`. A contract change replaces it instead of adding a
  version beside it.
- Replay accepts only the current contract. A turn recorded under another
  version is rejected as recorded under another contract; its events stay in
  the append-only journal unchanged.
- Run status and conversation history still read the terminal events of
  earlier turns, whose shape has not changed.
- The first release ends this rule: from then on a contract change adds a
  version and replay keeps the earlier ones, as ADR 0009 required.

## Consequences

Replay, request building and instructions have one form each: the
complete-set context, the time note in the latest message, the stepped
history window, requests by digest, the selection by reference, text in
chunks and typed failures. The frozen version-1 failure grammar, the
legacy instructions and the full-form payloads are gone, with their tests.

## Alternatives

- **Keeping every version.** Pays forever for history that does not exist.
- **Migrating old events.** Rewrites the journal, which ADR 0009 forbids.
