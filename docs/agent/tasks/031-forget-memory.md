# Task 031: Forgetting a memory directly

Contract and evidence in one file. Decision:
[ADR 0032](../../adr/0032-forgetting-a-memory.md). On 2026-10-01 the user chose
memory management first ("memory management먼저 ㄱㄱ").

## Problem

Ditto writes memories on its own (Task 024). The user could correct any
memory, but removing one took a conversation with Ditto: neither the web
memory list, the CLI nor Telegram could forget a memory.

## Contract

- `POST /v1/commands/memory/forget` `{ session_id, memory_id }`, closed: an
  active memory of the session, the user's own or Ditto's, is forgotten once;
  another is a conflict (HTTP 409), a malformed ID a bad request (HTTP 400).
- The kernel appends a task-free `memory.forgotten` event (user) and the
  disputed node it sources, which supersedes the memory: the form Ditto's
  `memory.forget` records, attested by the user. The memory leaves context,
  search and listings; the journal keeps it.
- Clients: the web memory list's Forget, confirmed by a second click within
  four seconds, in place; `ditto memory forget ID [--session]`; Telegram
  `/forget <words>`, which forgets only when exactly one memory holds the
  words (ignoring case), lists the matches when several do and says so when
  none does.

## Exit criteria

1. A forgotten memory leaves the list and later context, also after the
   projection is rebuilt; another session's, an unknown or a malformed ID
   forgets nothing and appends nothing; forgetting twice is a conflict.
2. The HTTP command rejects unknown fields and malformed IDs and reports a
   repeated forget as a conflict.
3. The web app, the CLI and Telegram forget a memory end to end.
4. The canonical gate passes.

## Evidence (2026-10-01)

- Kernel test `forgetting_takes_one_active_memory_out_of_use_and_survives_rebuild`:
  another session's memory and an unknown ID are conflicts, a malformed ID is
  invalid, none appends an event; forgetting records a user `memory.forgotten`
  event without a task and a node it causes with origin `user`, status
  `disputed`, superseding the memory; the list keeps the other memories and
  the other session's; a second forget is a conflict; a rebuilt projection
  agrees.
- HTTP test: an extra `actor` field is HTTP 422, `memory-1` HTTP 400, the
  first forget HTTP 200 `recorded`, the second HTTP 409, and the list is
  empty after.
- Built-CLI chat scenario: `ditto memory forget` on the memory saved with
  `/remember` succeeds and prints its ID; a second run fails with "not an
  active memory".
- Built-CLI Telegram scenario: `/forget GREEN tea` replies "Forgot: I like
  green tea" and the list is empty. The fake Telegram API now confirms
  updates below the requested offset, as Telegram does; it had redelivered
  every update to the restarted gateway, which saved `/remember` twice.
- Telegram unit tests: `/forget` parsing and case-insensitive matching of
  one, several and no memories.
- Browser end-to-end (`scripts/web-e2e.js`, debug build, headless Chromium):
  31 of 31 checks, including "a memory is forgotten from the list after a
  second click" (the first click shows "Forget it?").
- `./scripts/agent-check.sh` passed on the unchanged final tree in 4 min
  35 s (pinned Rust 1.88.0): 584 Rust tests (577 workspace including doctests,
  seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer cases and
  both smokes. Local log `target/task031/gate.log`, SHA-256
  `5b75d90839b69f9f8c16a6b30154bab439cdd5b15d9e7dc84eb60d66c78655b2`.
