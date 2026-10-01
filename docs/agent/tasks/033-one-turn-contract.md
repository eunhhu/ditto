# Task 033: One turn contract until the first release

Contract and evidence in one file. Decision:
[ADR 0034](../../adr/0034-one-turn-contract.md). On 2026-10-01 the user asked
for the code to be consolidated and as light and thin as possible
("주루루 모듈이나 도구달지말고 종합해서 최대한 가볍고 얇게 짜라").

## Problem

Replay kept eleven turn payload versions, each with its own instructions,
context selection, request, capsule and selection forms or failure grammar,
for history that does not exist: nothing is released and no data directory
is in use.

## Contract

- Replay accepts only `TURN_PAYLOAD_VERSION` (11); any other version is
  rejected as recorded under another contract.
- One form each: complete-set context selection, the instructions as one
  constant set with the local time as a note in the latest message, the
  stepped history window, requests by digest, the selection by contract
  digests, text in chunks and typed failure reasons. `context.compiled`
  requires `utc_offset_minutes` and has no capsule; the rebuilt selection has
  no schemas.
- Run status and conversation history still read earlier turns' terminal
  events.

## Exit criteria

1. A current turn replays; the same turn relabeled as version 0, 1, 10 or 12,
   wholly or in its first payload, does not.
2. Every current-contract test still passes.
3. The canonical gate passes.

## Evidence (2026-10-01)

- `only_the_current_turn_contract_replays` replaces the relabel test that
  replayed one turn as versions 1 to 10; the full-form selection forgeries
  and the instruction relabel helpers are gone. 97 turn tests pass.
- Removed: the version-1 positive-overlap and failure-message grammar, the
  legacy and versioned instruction sets, full-form requests, capsules and
  selections, the version-3 history rule and per-version checks in replay.
  The turn module shrank from 9,208 to 8,852 lines and its tests from 7,412
  to 7,192.
- Release daemon memory, unchanged by this slice (same machine, 12 memories,
  instant mock model): 11.6 MB resident at idle (2.0 MB anonymous), 16.5 MB
  after 20 turns and at most 17.5 MB after three sessions ran at once; the
  binary is 19 MB.
- `./scripts/agent-check.sh` passed on the unchanged final tree in 4 min
  20 s (pinned Rust 1.88.0): 590 Rust tests (583 workspace including
  doctests, seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer
  cases and both smokes. Local log `target/task033/gate.log`, SHA-256
  `8bf59bac6f9753d727f2afe4256cc475e04f819419a3a90ee1752f5a1fb89cd9`.
