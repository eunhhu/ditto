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

- Base: main `58b2b02` (Task 016, PR #21). Branch
  `dev/task-016-1-personal-recall` holds the preserved
  [Task 017](tasks/017-evaluation-outcomes.md) draft, raw-report and handoff
  trimming, the turn-loop split and Task 016.1; `dev/task-018-conversation-threads`
  stacks Task 018 on it, and `dev/task-019-openai-compatible`,
  `dev/task-020-web-app`, `dev/task-021-telegram`,
  `dev/task-022-assistant-instructions` and `dev/task-023-web-fetch` stack
  Tasks 019–023. Nothing is pushed.
- Later on 2026-09-30 the user redirected the frontier to a daily-driver
  assistant that can stand in for OpenClaw, Hermes, Grok bots, Muse and Dot;
  [NEXT](NEXT.md) orders the slices. No parity claim is made. Slices 018–023
  are done. Model-invoked memory saving (024) awaits the user's decision
  because it reverses ADR 0015's exclusion of model tool invocation.
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
  digest-verified cache replayed once at open, then delta-verified.
- **Ingress.** Typed commands only: record-only input, memory save/list/correct,
  run/status/cancel, conversation reset and view, and loopback-only sort,
  schedule and repeat. The daemon also serves the embedded web app (ADR 0024)
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
  lexical overlap without function words. There is no transcript injection,
  cross-session memory or production semantic retrieval.
  The separate V2 joint working-set query is lexical in production with an
  injected embedding seam for tests.
- **Web links.** `ditto-web-fetch` (ADR 0027) reads pages linked in the user's
  own message: exact-URL leases, public addresses only with pinned
  connections, manual redirects, size and time bounds, text extraction and
  journaled `agent.fetch.*` evidence replayed without network I/O.
- **Capabilities.** Generated package headers keep full manifests out of
  startup and search; a selected manifest is paged after digest and projection
  checks. Live invocation uses the closed Invocation Schema Profile V1 through a
  sealed `LiveExecutionEpoch`; replayable `ExecutionEpochEvidence` carries no
  authority. `device.process.run` is a discovery-only manifest with no runner.
- **Policy.** Sealed canonical invocations carry harness-derived effect,
  resource and placement. One affine ticket per epoch creates one expiring
  ledger. `artifact.read` uses a static no-approval permit; `artifact.sort`
  consumes a one-shot `ExecutionClaim` under an exact-resource, one-call lease.
- **Model and turns.** `ditto-model` owns the provider-neutral request/stream
  contract. `ditto-model-openai` holds two drivers on one request lifecycle:
  the closed `gpt-5.6` Responses profile (ephemeral storage) and an
  OpenAI-compatible `/chat/completions` driver for an operator-configured
  local or hosted server (ADR 0023; HTTPS unless loopback, no redirects,
  optional key only from `DITTO_MODEL_API_KEY`). Keys are redacted and
  transport-only. The daemon defaults to a disabled provider. The kernel turn loop compiles context, pages
  `artifact.read` (plus `artifact.sort` only for a permitted attachment), runs
  at most eight model requests, journals versioned transitions before
  publication and replays without provider or artifact I/O. New turns write
  payload version 3: typed `TurnFailureReason`s for validator-derived failures
  (since version 2) and, for agent runs, the current conversation thread's
  newest finished exchanges (at most eight, 24 KiB) as native messages, with
  their turn IDs recorded and recomputed on replay (ADR 0022). Older versions
  replay under their original rules; a turn never mixes versions. Version 4
  (ADR 0026) replaces the harness-facing system instructions with
  personal-assistant instructions and the local time of acceptance, fixed by a
  recorded UTC offset. `ditto chat` is an interactive client with `/new` and
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
  retry or housekeeping model call. Runs, sorts and scheduled dispatch share
  one execution slot; other immediate requests are rejected as busy.
- **Measurement.** Task 014 recorded an offline RAM/latency/accounting
  baseline; Tasks 015–016 recorded five literal synthetic queries at zero and
  1,000 unrelated memories; corpus schema 3 (Task 016.1) derives expected
  capsules from the complete-set rule. None of these measures answer quality,
  semantic recall at scale, tool-task success, live cost or v0.1 readiness.

## Latest verified slice: Task 023

- [Contract and evidence](tasks/023-web-fetch.md). The crate tests cover
  address classes, extraction, URL grants, compile-path derivation, redirects,
  bounds, timeouts and cancellation. The production policy refused six
  private targets without a connection. Kernel tests fetched a granted link
  once and replayed it without network I/O, rejecting four forgeries. They
  also denied an unlisted URL without contact, enforced the call budget,
  offered no tool without links or when disabled, and blocked a private link.
  A real fetch of `https://example.com/` through the built daemon returned
  "Example Domain"; a loopback link returned `blocked_address`.
- The canonical gate passed on the final tree (1 min 49 s with warm caches,
  pinned Rust 1.88.0): 544 Rust tests (537 workspace including doctests,
  seven built-CLI scenarios), 8 baseline and 21 quality Python tests, 12 web
  renderer cases and both smokes; the tree hash was identical before and
  after. An earlier run hit the corpus harness's 20 s CLI bound under a load
  average of 8.7 on the shared machine and passed on rerun.

## Previous slice: Task 022

- [Contract and evidence](tasks/022-assistant-instructions.md): assistant
  instructions and local time, turn payload version 4 (ADR 0026). Its gate
  passed on its final tree (532 Rust tests).

## Known gaps

- After a reload the web app restores only finished exchanges of the current
  thread: failed or cancelled turns are not restored, and a run still in
  flight at load appears when it finishes. Phone access to the web app still
  needs an authenticated gateway; Telegram is the phone path for now.
- The Telegram gateway keeps pending replies in memory: a gateway restart
  during an answer loses that reply unless Telegram redelivers the update.
  Only text messages are handled.
- `web.fetch` decodes bodies as UTF-8 and runs no JavaScript, so pages in
  legacy charsets or rendered by scripts yield garbled or little text. Web
  search waits for per-call approval.

- Answer quality and tool-use reliability with local or hosted
  OpenAI-compatible models are unmeasured; tools need a model with function
  calling.

- Sessions whose current memories exceed the context budget fall back to
  lexical overlap, so a pure paraphrase can miss. Semantic retrieval (deferred)
  or a larger budget (a cost decision for the user) would close it.
- Version-1 turns still replay through the frozen failure-message grammar;
  only historical traces depend on it.
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
- SQLite calls are synchronous inside async handlers, and the run slot holds a
  `std::sync::Mutex` across storage writes; measure before adding a
  high-concurrency gateway.
- Context graph edges are validated but unused in ranking.
- The context-projection authority workflow is one large module; split it
  without weakening its single-gate, single-rebuild or atomic-checkpoint
  semantics.
- Three lexical tokenizers differ (context V1, capability search, retrieval
  V2).
- Headerless capability packages still pay one bounded startup body read.
- The injected embedding interface is synchronous and may make up to 513
  serial calls; a production worker needs rerank pools, batching and caching.
- Legacy excluded-context receipts have no independent encoded payload ceiling;
  add one before accepting them as a new durable wire input.
- Generic model-task completion admission is a high-water check followed by
  append, not an atomic verifier/admission transaction.
- The only process profile is bounded sort; broader programs, host-crash
  containment and new verifiers need separate contracts.
