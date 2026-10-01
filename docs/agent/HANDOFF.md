# Verified handoff

Current state only. Per-slice history lives in `tasks/*-evidence.md` and git;
replace superseded facts instead of appending. Update this file only after code
and checks establish a new fact.

## Product intent

- On 2026-09-06 the user confirmed a local-first personal general-purpose agent,
  with Hermes as a positioning reference (not a benchmark). Target problems:
  process memory, context efficiency, memory and scheduled-job reliability,
  sustainable self-improvement, task performance and latency. See
  [product intent](../product.md).
- Zero cost and zero overhead, including implementation effort and future debt,
  are primary premises. Whether external model/API charges must also be zero is
  an open clarification; no zero-cost or superiority result is claimed.

## Repository state

- On 2026-10-01 the user asked for everything to be merged and pushed. main
  now holds Tasks 016.1 through 035 as one linear commit per slice on top of
  `58b2b02` (Task 016, PR #21), fast-forwarded from the stacked `dev/task-*`
  branches, which are deleted locally, and is pushed to `origin`. The
  [Task 017](tasks/017-evaluation-outcomes.md) draft is part of it.
- Later on 2026-09-30 the user redirected the frontier to a daily-driver
  assistant that can stand in for OpenClaw, Hermes, Grok bots, Muse and Dot;
  [NEXT](NEXT.md) orders the slices. No parity claim is made. Slices 018–023
  are done. On 2026-10-01 the user approved memory that Ditto manages on its
  own (Task 024, ADR 0031), reversing ADR 0015's exclusion of model-invoked
  memory writes, and the recommended journal fix (Task 030).
- Still on 2026-09-30 the user asked for the thinnest harness designed around
  concurrency, real-time streaming and context injection timing and scope. The
  design is [realtime-harness](../design/realtime-harness.md) and ADR 0028.
  Phases A–D are implemented (Tasks 025–028) and Phase E in part (Task 029:
  memory search and tool progress; Task 035: one tool lifecycle); parallel
  read-only calls are deferred.
- On 2026-09-30 a codebase review found that run context missed paraphrased or
  inflected questions and admitted unrelated memories, and that replay parsed
  validator wording. The user prioritized fixing both, trimming process
  overhead and splitting the turn loop before Task 017's measured runs; Task
  016.1 addresses the first two.
- Raw measurement reports are no longer tracked. Tasks 014–016 reports remain
  retrievable with `git show 58b2b02:docs/agent/tasks/<report>.json`; their
  evidence files record the digests. The canary rejects tracked
  `docs/**/*.json` and any tracked file above 1 MiB.

## Current system

- **Runtime and storage.** Rust daemon (axum, loopback by default) and CLI. The
  SQLite event spine (schema 7) is append-only by trigger and the sole durable
  authority; schedule and repeat indexes are projected in the same transaction.
  Artifacts are SHA-256 content-addressed with verified reads. The context
  projection (`context-projection.db`, schema 4) is a rebuildable,
  digest-verified cache replayed once at open, then delta-verified; replay and
  deltas read only context nodes, through the kind index. Storage work runs on
  blocking threads: daemon handlers use the blocking pool, and runs, sorts and
  the scheduler journal from blocking threads. Debug builds reject journal
  access on threads that drive async tasks.
- **Ingress.** Typed commands only: record-only input, memory
  save/list/correct/forget,
  run/status/cancel, conversation reset and view, and loopback-only sort,
  schedule and repeat. A run whose text only acknowledges an answer that
  asked nothing is recorded as `acknowledged` without a turn (ADR 0035). The daemon also serves the embedded web app (ADR 0024)
  under a same-origin-only content security policy. While bound to loopback,
  every route refuses requests addressed to a non-loopback host name, which
  blocks DNS rebinding. `ditto telegram` (ADR 0025) is a CLI client that
  relays allowed private Telegram chats and delivers scheduled results; run
  status names the schedule that started a run. The daemon and the offline
  fixture server share one route table. Clients never
  choose actors, kinds or internal metadata; the kernel derives all authority,
  including an attached file's sort permission. SSE subscribes first, replays a
  bounded high-water snapshot in pages and recovers gaps or lag from storage.
- **Context.** Explicit memories promote exact same-session user input; a
  correction supersedes one active memory; listing rechecks source text. Runs
  compile the verified active session/task snapshot with the complete-set
  contract (ADR 0021): the whole current set when it fits the budget (default
  900 estimated tokens, about twelve short memories), otherwise positive
  lexical overlap without function words. With `memory.manage` agent runs
  search the memories their compilation saw, including those it left out
  (ADR 0029), and Ditto remembers, replaces and forgets memories on its own
  (ADR 0031): each written memory is a node
  sourced by a session-scoped `memory.written` event, labeled origin `model`,
  status `inferred`, refused after the turn read a web page or file, for
  credential-shaped facts and past three writes per turn. There is no
  transcript injection, cross-session memory or production semantic
  retrieval.
  The separate V2 joint working-set query is lexical in production with an
  injected embedding seam for tests.
- **Web.** `web.browse` (crate `ditto-web-fetch`, ADR 0036) reads pages
  linked in the user's own message (ADR 0027: exact-URL leases, public
  addresses only with pinned connections, manual redirects, size and time
  bounds, text extraction) and, with `--search-url`, searches on its own
  through a SearXNG-compatible service (ADR 0033: the endpoint is the only
  resource a three-call lease allows, credential-shaped queries are refused,
  at most five bounded results). Its schema offers only what the deployment
  enables; results return as untrusted content and replay uses the journaled
  ones. The instructions tell the model to work on its own and to hand off
  only what needs the user, ending its answer with the question.
- **Capabilities.** Generated package headers keep full manifests out of
  startup and search; a selected manifest is paged after digest and projection
  checks. Live invocation uses the closed Invocation Schema Profile V1 through a
  sealed `LiveExecutionEpoch`; replayable `ExecutionEpochEvidence` carries no
  authority. `device.process.run` is a discovery-only manifest with no runner.
- **Policy.** Sealed canonical invocations carry harness-derived effect,
  resource and placement. One affine ticket per epoch creates one expiring
  ledger. `artifact.read` and memory searches use static no-approval permits;
  `artifact.sort`, web reads and web searches each consume one-shot
  `ExecutionClaim`s under their own exact-resource leases, and memory writes
  share one three-call lease without resources.
- **Model and turns.** `ditto-model` owns the provider-neutral request/stream
  contract. `ditto-model-openai` holds two drivers on one request lifecycle:
  the closed `gpt-5.6` Responses profile (ephemeral storage) and an
  OpenAI-compatible `/chat/completions` driver for an operator-configured
  local or hosted server (ADR 0023; HTTPS unless loopback, no redirects,
  optional key only from `DITTO_MODEL_API_KEY`). Keys are redacted and
  transport-only. The daemon defaults to a disabled provider. The kernel turn
  loop compiles context, pages its tools from one table of builtins (an
  ordinary agent run gets `web.browse` while reading or searching is enabled
  and `memory.manage`; an attachment adds `artifact.read` and the permitted
  `artifact.sort`; a legacy artifact turn gets `artifact.read`), runs at most
  eight model requests, journals versioned transitions before publication and
  replays without provider, artifact or network I/O. Agent tool calls share
  one `tool.requested`/`tool.started`/`tool.output` lifecycle (ADR 0036).
  There is one turn contract, version 13 (ADR 0034): replay rejects turns
  recorded under any other version, while status and thread history still
  read their answers.
  The contract carries typed failure reasons (ADR 0021), the thread's recent
  exchanges as native messages in a stepped window (ADRs 0022, 0028), the
  assistant instructions with the local time and the session's runs still in
  flight as notes in the latest message (ADRs 0026, 0028, 0035), each fact
  recorded once (text in chunks, each
  request as its SHA-256, the selection by contract digests; ADRs 0028,
  0030), and the two agent tools of ADR 0036, with the instruction to work
  on its own and hand off only what needs the user.

  A run reuses its session's verified context while no context node
  has been committed since (sessions with task-scoped or windowed nodes are
  recompiled), keeps each session's thread between turns, and commits
  `context.compiled` and `capabilities.selected` with its first model request.
  Builtin tool contracts are validated once per process. `ditto chat` is an interactive client with `/new` and
  `/remember`; `ditto new` starts a thread. Answers stay `unverified`; model runs never emit
  `task.completed`. The loop
  is split into stage functions (context, capability selection, request
  dispatch, stream admission, tool execution, finish); cancellation/deadline
  checkpoint messages are one shared `Checkpoint` table used by runtime and
  replay; the split itself changed no behavior (485 workspace tests and the five
  built-CLI scenarios passed before Task 016.1).
- **Local work and schedules.** The closed `/usr/bin/sort` profile (64 KiB /
  4,096 lines, five seconds, cleared environment, private scratch,
  process-group cleanup) has an independent verifier and a sort-specific
  completion. One-shot schedules (ADR 0019) and finite repeats (ADR 0020) share
  a bounded index of 100 future intents, one event/timer-driven scheduler,
  at-most-once claims, visible missed/interrupted states and no automatic
  retry or housekeeping model call. Four runs and sorts, including scheduled
  dispatch, are active at once in total, from one session or several
  (ADR 0035); other immediate requests are rejected as busy and scheduled
  work waits for a slot.
- **Measurement.** Task 014 recorded an offline RAM/latency/accounting
  baseline; Tasks 015–016 recorded five literal synthetic queries at zero and
  1,000 unrelated memories; corpus schema 3 (Task 016.1) derives expected
  capsules from the complete-set rule, schema 4 (Task 025) from its
  version-6 ID-ordered presentation, and schema 5 (Task 027) binds each
  driver observation to its journaled request digest. None of these measures answer quality,
  semantic recall at scale, tool-task success, live cost or v0.1 readiness.

## Latest verified slice: Task 035

- [Contract and evidence](tasks/035-two-agent-tools.md) (ADR 0036), turn
  contract 13. An ordinary run offers two tools, `memory.manage` and
  `web.browse`, instead of five or six; `artifact.read` comes only with an
  attachment; every agent tool call journals one `tool.*` lifecycle, and
  fourteen per-tool modules became five. A minimal request shrank from 4,538
  to 3,571 bytes and the turn module from 8,989 to 7,731 lines.
- Its gate passed on its final tree (592 Rust tests).

## Previous slice: Task 034

- [Contract and evidence](tasks/034-chat-as-a-bridge.md) (ADR 0035): four
  answers at once even in one session, the in-flight note, free
  acknowledgments and per-state messages. Its gate passed on its final tree
  (593 Rust tests).

## Known gaps

- Measured harness baseline (2026-09-30, Raspberry Pi 5, release build,
  instant loopback model, `scripts/measure-harness.py`):
  - 5–10 ms of work before dispatch (1 ms after Task 026);
  - 74–80 µs (55–60 µs after Task 026) and one SQLite transaction per
    streamed delta, 206 or 1,006 events per turn, 81–107 journal bytes per
    answer byte (after Task 027: 8 or 9 events and 16.3 or 4.9 bytes; after
    Task 030: 13.4 or 4.3 bytes, the tools of Task 029 included);
  - median prompt prefix reuse between turns of 50.6 % (96.4 % after
    Task 025);
  - one of three simultaneous sessions accepted, the others HTTP 429 (all
    three after Task 028, and four runs in one session after Task 034).

  ADR 0028's phases target each of these.

- After a reload the web app restores only finished exchanges of the current
  thread, in the order they finished: failed or cancelled turns,
  acknowledgments and the lines said before a tool call are not restored,
  and a run still in flight at load appears when it finishes. Phone access to the web app still
  needs an authenticated gateway; Telegram is the phone path for now.
- The Telegram gateway keeps pending replies in memory: a gateway restart
  during an answer loses that reply unless Telegram redelivers the update.
  Only text messages are handled. A fifth message while four answers run
  waits up to three minutes for a slot.
- Acknowledgments are a closed list checked against the last line of the
  latest answer: an offer phrased without a question mark followed by "ok"
  is taken as an acknowledgment, which the instructions avoid by asking the
  model to end hand-offs with a question.
- `web.browse` decodes pages as UTF-8 and runs no JavaScript, so pages in
  legacy charsets or rendered by scripts yield garbled or little text.
- Web search needs a SearXNG-compatible service the operator runs or trusts;
  that service sees each query. Ditto reads results' snippets but cannot open
  a result page unless the user sends its link.

- Answer quality and tool-use reliability with local or hosted
  OpenAI-compatible models are unmeasured; tools need a model with function
  calling.

- Sessions whose current memories exceed the context budget fall back to
  lexical overlap, so a pure paraphrase can miss. The model can now search the
  left-out memories in its own words (Task 029), but that search is lexical
  too and runs only when the model chooses. Semantic retrieval (deferred) or a
  larger budget (a cost decision for the user) would close it.
- Memories Ditto infers can be wrong, and its credential check is a
  heuristic. Writes are refused after a web page or file is read in the same
  turn, but injected text that reached Ditto's own earlier answers in the
  thread is not caught. Forgetting removes a memory from use and view, not
  from the append-only journal. The web app's notice of a write shows only
  live; after a reload the memory list still marks Ditto's memories.
- The scheduler sleeps on a monotonic timer and has no resume wake-up, so a
  suspended laptop may delay or miss a start window. Not reproduced.

## Intentionally deferred

- Calendar recurrence, scheduled effect grants and additional effectful tool
  profiles.
- Native non-OpenAI wire formats, reasoning replay, remote cancel and explicit
  prompt-cache breakpoints.
- Additional capability derivers, durable/cross-process authorization, approval
  fulfillment and the capability worker protocol.
- Device registry, general process profiles, SSH transport and secrets.
- A production embedding worker/provider and persisted embedding cache.
- Additional completion verifiers and the improvement compiler.
- Authenticated remote gateway (phone or remote access to the web app).

## Known engineering debt

- Artifact-root authorization pages through session history and range reads
  verify the whole object; measure before adding indexes or caches.
- Memory listing materializes the bounded active snapshot before paging; its
  10,000-candidate and byte limits include other active session context.
- The run slot holds a `std::sync::Mutex` across storage writes, now on
  blocking threads, and one connection lock serializes every journal read and
  write; measure with Phase D's parallel sessions before adding a writer
  thread or reader pool.
- Context graph edges are validated but unused in ranking.
- The context-projection authority workflow is one large module; split it
  without weakening its single-gate, single-rebuild or atomic-checkpoint
  semantics.
- Three lexical tokenizers differ (context V1, capability search, retrieval
  V2); the memory search reuses context V1.
- Each builtin tool keeps its own journal events, run path and replay checks;
  ADR 0028 defers one shared lifecycle until another effectful tool is added.
- Journal bytes per answer byte are 13.4 (200-delta answer) and 4.3
  (1,000-delta answer) with twelve memories, against ADR 0028's target of 3.
  The largest remaining terms are `context.compiled`, which records each
  included memory in full (about 480 bytes each) although its own
  `context.node.recorded` event holds it, and `turn.finished`, which repeats
  the answer text so status and thread history read it without reassembling
  chunks.
- Headerless capability packages still pay one bounded startup body read.
- The injected embedding interface is synchronous and may make up to 513
  serial calls; a production worker needs rerank pools, batching and caching.
- Legacy excluded-context receipts have no independent encoded payload ceiling;
  add one before accepting them as a new durable wire input.
- Generic model-task completion admission is a high-water check followed by
  append, not an atomic verifier/admission transaction.
- The only process profile is bounded sort; broader programs, host-crash
  containment and new verifiers need separate contracts.
