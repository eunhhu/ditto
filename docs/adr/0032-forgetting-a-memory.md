# ADR 0032: Forgetting a memory directly

Status: accepted on 2026-10-01 for Task 031. Amends
[ADR 0015](0015-explicit-user-memory.md) (the memory commands).

## Context

Since [ADR 0031](0031-model-managed-memory.md) Ditto writes memories on its
own. The user could correct any memory, but could remove one only by asking
Ditto in a conversation. Someone who manages what Ditto remembers needs to
remove any memory directly: in the web app, the CLI and Telegram.

## Decision

- **Command.** `POST /v1/commands/memory/forget` takes
  `{ "session_id", "memory_id" }` and rejects unknown fields. Like
  `/v1/commands/memory` it belongs to the trusted local-user ingress. The
  memory must be an active memory of the session, the user's own or one Ditto
  inferred; otherwise the command is a conflict (HTTP 409), and a malformed
  ID is a bad request (HTTP 400).
- **Records.** Under the context admission gate the kernel appends a
  task-free `memory.forgotten` event (user, `{ event_version: 1, memory_id }`)
  and a disputed node it sources: summary `forgotten`, origin `user`,
  superseding the memory. It is the form Ditto's `memory.forget` records,
  attested by the user instead of the model. A disputed node is never active,
  so neither reaches context, search or listings; the append-only journal
  keeps both.
- **Response.** The forgotten memory's ID and the node event's identity, with
  outcome `recorded` (HTTP 200) or `committed_but_projection_unavailable`
  (HTTP 202). Forgetting it again is a conflict: it is no longer active.
- **Clients.** The web memory list offers Forget with a second, in-place
  confirmation. `ditto memory forget ID [--session]` prints the response.
  Telegram's `/forget <words>` forgets the one memory whose text contains the
  words, ignoring case; when several match it lists them, and when none does
  it says so.

## Alternatives

- **Deleting the memory's events.** The journal is append-only.
- **Telegram by list position.** Ditto may write between showing the list and
  the command, so a number could name another memory; requiring exactly one
  memory to match the words cannot.
- **Answering a repeated forget with the earlier records.** It needs a lookup
  from a memory to the node that superseded it; a conflict already tells a
  client the memory is gone.
