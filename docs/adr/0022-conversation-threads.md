# ADR 0022: Conversation threads

Status: accepted for Task 018. Extends ADR 0016 (explicit runs) and ADR 0021
(turn payload version 2) with turn payload version 3.

## Context

Every run was a single turn: the model saw memories and the current request,
never the previous exchange, so follow-ups such as "What is his name?" failed.
Assistants in this category keep a conversation. Ditto must add that without
turning transcripts into unbounded prompts, unverifiable state or a second
source of truth.

## Decision

A session's **current thread** is every finished agent-run turn after the
latest `conversation.reset` event in that session. A new agent run replays the
thread's newest exchanges as native conversation messages (user text, then the
unverified final answer) before its own request:

- only turns that emitted `turn.finished` and whose input carries agent-run
  metadata count; failed, interrupted and legacy injected turns never do;
- at most the 32 newest finished turns are examined, at most 8 exchanges are
  kept, each message is truncated to 4 KiB with an explicit marker, and older
  exchanges are dropped once 24 KiB would be exceeded;
- memories are unaffected by the thread; context compilation is unchanged.

`POST /v1/commands/conversation/reset` (CLI `ditto new`, `/new` in `ditto chat`)
appends a user-authored `conversation.reset` event with `{ "version": 1 }`. It
deletes nothing. Event-store schema 7 adds one partial index over reset and
finished-turn events, so selection is two indexed reads plus one indexed input
lookup per examined turn.

Turn payload version 3 records the selected turn IDs, oldest first, as
`history_turn_ids` in `context.compiled`. Runtime and replay share one pure
selection function; replay recomputes the thread from the session snapshot,
requires the recorded IDs to match and rebuilds the exact request conversation.
If history cannot be read, the run fails before model I/O with the typed reason
`conversation_history_unavailable`. Versions 1 and 2 carry no history.

## Rejected alternatives

- Putting history in the context capsule mixes transcripts with memory ranking
  and budgets, and models handle native messages better.
- A time window adds a second implicit boundary; explicit resets are clearer.
- Model-written summaries of old turns would add a housekeeping model call.
- A thread identifier on every run would change the run command contract;
  one thread per session with an explicit reset is enough for now.

## Consequences, compatibility and rollback

Answers can use recent context while request size stays bounded (at most
24 KiB of history text). Scheduled runs in the same session join the thread.
Older binaries cannot open a schema-7 store or replay version-3 turns; the
index can be dropped without losing data. Rollback stops writing version 3 and
ignores reset events for new runs while keeping version-3 replay.
