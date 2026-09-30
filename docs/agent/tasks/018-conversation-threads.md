# Task 018: Conversation threads

Contract and evidence in one file. Decision:
[ADR 0022](../../adr/0022-conversation-threads.md).

## Problem

Every run was a single turn, so follow-up questions lost their referent. The
2026-09-30 direction is a daily-driver personal assistant, which needs
conversation continuity in every channel (CLI, web, messaging).

## Contract

- A session's current thread is its finished agent-run turns after the latest
  `conversation.reset`. New agent runs replay at most eight of the newest
  exchanges (32 turns examined, 4 KiB per message, 24 KiB total) as native
  messages before the request. Failed, interrupted and legacy turns never join.
- `POST /v1/commands/conversation/reset`, CLI `ditto new` and `/new` in the new
  interactive `ditto chat` start a thread without deleting anything.
- Turn payload version 3 records `history_turn_ids`; replay recomputes the
  thread from the snapshot with the shared rule and rebuilds the conversation.
  Unreadable history fails the run before model I/O with
  `conversation_history_unavailable`.
- Event-store schema 7 adds one partial index; memories and context
  compilation are unchanged. The daemon and the offline fixture server now
  share one route table.

## Exit criteria

1. A follow-up receives the previous exchange; a reset, a failed turn and
   another session do not leak into the thread.
2. Bounds, truncation and ordering are deterministic and replayed.
3. Replay rejects altered history IDs, a conversation missing its history and
   history under an older payload version.
4. The corpus harness keeps measuring capsules on fresh threads; the canonical
   gate passes.

## Evidence (2026-09-30)

- Kernel: two new turn tests (thread replay until reset, with failed and
  other-session exclusions and four forgeries; eight-exchange, truncation and
  byte bounds) and two history-rule unit tests. Event store: schema 6 to 7
  migration without rewrite, index-only query plan, reset/sequence/session
  bounds and exact turn-input lookup.
- Real processes: `ditto chat` against the fixture server answered three lines;
  the model received `[user]`, then
  `[user, assistant, user]`, then after `/new` only `[user]`. Terminal escape
  sequences in the answer were printed escaped.
- The corpus harness (21 tests) starts each measured query with `ditto new`;
  its capsules are unchanged.
- `./scripts/agent-check.sh` passed on the final tree in 8 min 34 s on the
  pinned Rust 1.88.0 toolchain: 505 Rust tests (500 workspace including
  doctests, five built-CLI scenarios), 8 + 21 Python tests and both smokes.
  Local log `target/task018/gate.log`, SHA-256
  `646731390d38b05c9c2c591c1bceedc1a9d70aab200a30d24ecea7721b83009a`.
