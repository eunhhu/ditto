# Implementation frontier

Work in order unless the user explicitly changes priorities. Complete one task
file's exit criteria before moving the marker.

## Active

On 2026-10-01 the user chose the next two slices: the recommended journal fix,
and memory that Ditto manages on its own, which reverses ADR 0015's exclusion
of model tool invocation for memory writes. In this order:

1. [030](tasks/030-selection-by-reference.md) Capability selection recorded
   by reference, turn payload version 9 - complete (`capabilities.selected`
   4,910 → 952 bytes; a short agent turn journals 4,879 bytes instead of
   8,837).
2. [024](tasks/024-model-managed-memory.md) Model-managed memory, turn
   payload version 10 - complete (Ditto remembers, replaces and forgets
   memories on its own, labeled as its inference, refused after reading a
   page or file; ADR 0031).

The user then chose memory management first:

3. [031](tasks/031-forget-memory.md) Forgetting a memory directly from the
   web memory list, the CLI or Telegram - complete (ADR 0032).

The user then asked for consent to be a hand-off and for Ditto to work on its
own ("알아서 잘 딱"):

4. [032](tasks/032-web-search.md) Web search on its own through a configured
   SearXNG-compatible service, turn payload version 11 - complete (ADR 0033;
   no per-query approval, consent handed off in the answer).

The user then asked for the code to be consolidated and as light and thin as
possible, then for chat to be a bridge to concurrent work, and for the
assistant to message by state and spend nothing on acknowledgments:

5. [033](tasks/033-one-turn-contract.md) One turn contract until the first
   release - complete (ADR 0034).
6. [034](tasks/034-chat-as-a-bridge.md) The chat as a bridge, turn contract
   12 - complete (ADR 0035; four answers at once even in one session, each
   run aware of the session's work in flight, the line before a tool call as
   its own message, and a 👍 instead of a model call for an acknowledgment).
7. 035 One builtin tool lifecycle, with the memory and web tools merged.

The thin-harness phases of ADR 0028 (Tasks 025–029) are complete except the
parts deferred there: one builtin tool lifecycle, parallel read-only calls,
and the journal target of three bytes per answer byte.

Still open from the earlier frontier:

- Reading the pages a search returned (today only links the user sends are
  fetched).
- The drafted [Task 017](tasks/017-evaluation-outcomes.md) evaluation remains
  the measurement gate before any claim of parity with other assistants.

## Completed

One line per slice; each task file and its evidence hold the details.

- [034](tasks/034-chat-as-a-bridge.md) The chat as a bridge, turn contract
  12 (ADR 0035).
- [033](tasks/033-one-turn-contract.md) One turn contract until the first
  release (ADR 0034).
- [032](tasks/032-web-search.md) Web search on its own, turn payload
  version 11 (ADR 0033).
- [031](tasks/031-forget-memory.md) Forgetting a memory from the web app,
  CLI or Telegram (ADR 0032).
- [024](tasks/024-model-managed-memory.md) Memory that Ditto manages, turn
  payload version 10 (ADR 0031).
- [030](tasks/030-selection-by-reference.md) Capability selection recorded
  by reference, turn payload version 9 (ADR 0030).
- [029](tasks/029-memory-search.md) Read-only memory search and tool
  progress, turn payload version 8 (ADR 0029; ADR 0028 Phase E in part).
- [028](tasks/028-session-parallel-runs.md) Session-parallel runs (ADR 0028
  Phase D).
- [027](tasks/027-one-journal-plane.md) One journal plane, each fact once,
  turn payload version 7 (ADR 0028 Phase C).
- [026](tasks/026-thin-turn-start.md) Thin turn start and storage off async
  threads (ADR 0028 Phase B).
- [025](tasks/025-cache-stable-layout.md) Cache-stable prompt layout, turn
  payload version 6 (ADR 0028 Phase A).
- [023](tasks/023-web-fetch.md) `web.fetch` for links in the user's message,
  turn payload version 5 (ADR 0027).
- [022](tasks/022-assistant-instructions.md) Assistant instructions, local
  time and `/remember` in `ditto chat`, turn payload version 4 (ADR 0026).
- [021](tasks/021-telegram-gateway.md) Telegram gateway with streamed drafts,
  stop and scheduled result delivery (ADR 0025).
- [020](tasks/020-local-web-app.md) Local web app, conversation view and
  loopback host guard (ADR 0024).
- [019](tasks/019-openai-compatible-provider.md) OpenAI-compatible chat
  provider for local and hosted models (ADR 0023).
- [018](tasks/018-conversation-threads.md) Conversation threads, `ditto chat`
  and `ditto new`, turn payload version 3 (ADR 0022).
- [016.1](tasks/016-1-personal-recall.md) Personal-scale recall (complete-set
  context) and typed turn failures, turn payload version 2 (ADR 0021).
- [016](tasks/016-personal-task-corpus.md) Offline personal-task context corpus:
  five literal synthetic queries after restart ([evidence](tasks/016-evidence.md)).
- [015](tasks/015-quality-history-workloads.md) Offline context-quality and
  longer-history workloads ([evidence](tasks/015-evidence.md)).
- [014](tasks/014-status-baselines.md) Human task inspection and offline
  personal-agent baseline ([evidence](tasks/014-evidence.md)).
- [013](tasks/013-recurring-schedules.md) Bounded recurring schedules
  ([evidence](tasks/013-evidence.md)).
- [012](tasks/012-scheduled-runs.md) One-shot scheduled requests and restart
  recovery ([evidence](tasks/012-evidence.md)).
- [011](tasks/011-model-sort.md) Model-directed, explicitly permitted local work
  ([evidence](tasks/011-evidence.md)).
- [010](tasks/010-local-process.md) Bounded local process execution
  ([evidence](tasks/010-evidence.md)).
- [009](tasks/009-agent-run.md) Explicit personal-agent request path
  ([evidence](tasks/009-evidence.md)).
- [008](tasks/008-user-memory.md) Explicit user memory
  ([evidence](tasks/008-evidence.md)).
- [007](tasks/007-capability-package-headers.md) Bounded capability package
  headers ([evidence](tasks/007-evidence.md)).
- [006](tasks/006-compact-session-index.md) Compact source-verified session index
  ([evidence](tasks/006-evidence.md)).
- [005.1](tasks/005-1-live-epoch-schema-authority.md) Live epoch, closed schema
  profile and bounded authority lifetime ([evidence](tasks/005-1-evidence.md)).
- [005](tasks/005-canonical-capability-invocation.md) Canonical capability
  invocation and effect/resource authority ([evidence](tasks/005-evidence.md)).
- [004.2](tasks/004-2-repair-budget-validity-precision.md) Repair budget and
  validity precision ([evidence](tasks/004-2-evidence.md)).
- [004.1](tasks/004-1-retrieval-resource-envelope.md) Retrieval resource envelope
  and lifecycle ([evidence](tasks/004-1-evidence.md)).
- [004](tasks/004-durable-context-projection.md) Durable context projection and
  shared retrieval query.
- [003](tasks/003-read-only-tool-loop.md) Read-only tool continuation loop.
- [002](tasks/002-first-provider.md) First frontier provider (closed `gpt-5.6`
  Responses adapter).
- [001](tasks/001-model-ir.md) Provider-neutral model IR.

## Later

- calendar recurrence, notification delivery and scheduled effect grants only
  after their product need and policy contracts are defined;
- batched, compact rerank pools and descriptor/hash caching before a production
  embedding worker;
- device registry, SSH placement, and expanded gateway integrations after the
  local workflow is useful;
- evidence-gated improvement compiler.
