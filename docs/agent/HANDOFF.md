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

- On 2026-10-01 the user asked for everything to be merged and pushed, and on
  2026-10-22 merged Task 036 as well. main now holds Tasks 016.1 through 036
  as one linear commit per slice on top of `58b2b02` (Task 016, PR #21),
  fast-forwarded from the stacked `dev/task-*` branches, which are deleted
  locally, and is pushed to `origin`, which keeps only `main` and the tag
  `archive/codex-scaffold` for the unmerged early scaffold. The
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
- On 2026-11-06 the user asked for a comparison with Hermes, OpenMausBot and
  Harness² and for the improvements to be specified. The mechanism comparison is
  [design/comparison-2026-11.md](../design/comparison-2026-11.md); Tasks 038,
  039, 040 and 041 are drafted specs and **no part of them is implemented**.
  The same day's measured facts: calibration 3 over 130 attempts recorded
  128/130 verified turns, Recall@5 45/45 clean and 43/45 with 1,000 distractor
  memories, and `eval/task017/budgets-v1.json` is frozen from it (Task 037).
- [Task 042](tasks/042-harness-only-self-improvement.md) owns harness-only
  self-improvement at post-training scale. Implemented so far: only
  `scripts/probe_variant.py` with `scripts/test-probe-variant.py` (apply a
  variant, run its cases, compare against a base run, revert byte-exactly). Two
  candidate variants are written down in `eval/variants/` and **neither has been
  probed yet**.
- The eval runner now keeps a store's daemon log under `<out>/failures/` when a
  store fails, because campaign 1 lost the only log of its own failure.
- Campaign 1 (130 attempts, 123 verified) is **not a valid readiness run**: an
  `agent-check.sh` build ran during it, which loaded the CPU that the latency
  gate measures, and one store's daemon failed to start inside its 60 s window.
  Its report is kept in scratch as evidence for that diagnosis; the readiness
  number has to come from a clean run.
- Campaign 2 (130 attempts) is **also invalid, for a worse reason**: it measured
  a binary that predated the tree. `capabilities.selected` shows only
  `web.browse` in every memory attempt, so the model answered "I don't have a
  memory search available in this session" and Recall@5 collapsed to 0.500 while
  the answers looked fine. A clean rebuild restored `memory.manage` selection
  and memory calls in all five attempts of the case. The runner now builds the
  daemon itself (`--no-build` exists for tests) and exits when the binary's
  mtime is older than the newest Rust or capability source.
- Campaign 3 is therefore the first readiness candidate: built by the runner,
  with `--agent-check-exit 0` and the frozen budget.
- First harness probes (Task 042) on the memory two-hop case: base 10/10 answers
  with Recall@5 18/20; the pointer-as-stopping-rule variant 10/10 and 17/20; the
  retrieval-hint variant 10/10 and 15/20 (safety gate flagged unused body
  loads). Both are `KEEP_BASE`. The case's answers were never wrong — only its
  recall — so the family is saturated for wording edits.
- Case-level gates now need ten repetitions to apply their 0.80/0.90 rate;
  below that they block only under 0.40 (`case_threshold` in
  `scripts/eval_task017.py`), because `X1`/`long_use` scored 3/5 and then 5/5 on
  consecutive runs of the same binary.


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
  at most five bounded results), and reads the pages its searches return
  (ADR 0037: from each search's output on, its results' exact URLs join the
  turn's read grant and lease; with a search service three reads per turn
  are shared by the user's links and found pages). Its schema offers only
  what the deployment enables; results return as untrusted content and
  replay uses the journaled ones. The instructions tell the model to work
  on its own and to hand off only what needs the user, ending its answer
  with the question.
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
  share one three-call lease without resources. The kernel adds exact
  resources to a registered lease, keeping its budget and expiry, only for
  the pages a search returned.
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
  There is one turn contract, version 15 (ADR 0034; 0038 runs the ready
  calls of a response in order): replay rejects turns
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

## Latest verified slice: a profile carries skills as documents (ADR 0046, Task 044 P3, first half)

- `crates/profile::skill` owns the format: `<profile>/skills/<name>/SKILL.md` with
  a closed `---` frontmatter holding `name` and a one-line `description` (1-200
  characters), then the body (at most 64 KiB). Names are validated (a-z, 0-9, `-`,
  `_`, at most 64, first character alphanumeric), so `../etc` is refused by name
  rather than by a path check. A malformed skill is reported with its path and
  reason, never skipped, and the declared name must match its directory.
- `ditto skills list|show --profile --data-dir` reads the catalogue on this host
  without a daemon. Live: a `photo-triage` skill listed as one line plus its
  description, `show` returned the body, and `show ../etc` exited 1 with
  `it must start with a lowercase letter or digit`.
- `DittoKernel::skill_headers()` and `skill_body(name)` answer both questions from
  the served profile's data root, reading the directory through each call: an
  installation changes a few times a year, and a cache would only add a way to be
  stale. A malformed skill makes the catalogue unreadable rather than silently
  shorter.
- Tests: `ditto-profile` 10 passed (6 new), `ditto-cli` 12 + 11 passed, kernel
  `skills` 3 passed.
- **Not done:** the header is not yet in the request prefix and there is no
  `skill.read` capability. That is the rest of P3; the format is fixed first
  because it is what a model-written skill would have to obey.
- **Design note (ADR 0046):** installing a skill grants no effect, so reading one
  needs no lease; what a skill does is still whatever capability the model then
  invokes.

## Latest verified slice: standing instructions are journaled (ADR 0045, Task 044 P2)

- `instructions.recorded` is a durable document: `DittoKernel::set_instructions`
  journals the text with the event id it supersedes, `instructions_in_force(seq)`
  reads the last one at or before a sequence. 2000 characters is the limit, and an
  overlong text is refused by name rather than truncated.
- The turn pins it after the six core instructions as one segment, recorded in the
  turn's own `context.compiled` (`standing_instructions`, payload version 17), so
  replay reproduces the request and a later edit cannot change what an old turn
  was told. Verified: `instructions::the_standing_instructions_lead_the_request_and_replay_keeps_the_old_text`.
- Blank instructions pin nothing - an empty profile adds no segment and costs no
  prefix space.
- Routes: `GET /v1/instructions` (in force, or `null`), `PUT /v1/instructions`
  (`{"text": ...}`, 201 with the document, 400 over the limit). CLI:
  `ditto instructions show|set --text|--file|clear`. Live check against
  `127.0.0.1:8901`: set returned seq 2, show echoed the text, clear returned
  `no standing instructions`.
- Tests: `read_only_turn` 109 passed; daemon `instructions` route test 1 passed.

## Latest verified slice: a profile is a directory (ADR 0044, Task 044 P1)

- `crates/profile` owns the boundary: `validate` (a-z, 0-9, `-`, `_`, at most 32
  characters, first character alphanumeric), `root` (`default` is the data root
  itself, every other name is `<data>/profiles/<name>`), `list`, `create`.
- `ditto-daemon --profile <name>` (env `DITTO_PROFILE`) resolves the data root
  before the kernel is built and logs `serving one profile` with both values.
  Verified live: `DITTO_PROFILE=minchae DITTO_DATA_DIR=/tmp/dprof2` listened on
  8899 and wrote to `/tmp/dprof2/profiles/minchae`.
- `ditto profile list` and `ditto profile path <name>` read the data root on this
  host (JSON by default, `--human` for a table). `../etc` is refused by the name
  rule, not by a path check.
- Isolation is by construction and tested: a file written under one root does not
  exist under the other (`two_profiles_live_in_two_directories`).
- Platform design and slice order: [design/platform-surface.md](../design/platform-surface.md).
  Durable documents, session state and turn state are separated by ADR 0045;
  verified by construction - the compiler's candidates come only from
  `context.node.recorded` events, so a tool result cannot enter tomorrow's capsule.

## Latest work: Task 038, two hops and a lighter tool call

Two changes, both measured on the Task 017 suite.

**A memory that places an object travels with the memory that names it.** The
capsule was selected by word overlap with the request, so "the saffron tin sits
above the fridge" shared nothing with "where is my spare key" and was dropped;
P3 answered "inside the saffron tin" in half its long-use attempts with one
model call and no search. `ContextCompiler::compile_with_closure` now recompiles
once in process, marking up to two such nodes `policy-required: memory pointer`
(ADR 0029). A pointer needs a word that at most two candidate summaries share,
so a topic word does not drag in unrelated memories. No model call and no new
receipt reason.

Measured (`probe-closure`, P1-P5, 50 attempts, $0.09): P3 long-use recall
5/10 -> 9/9, long-use recall over valid attempts 0.889 -> 1.000. Ten attempts
failed with an empty provider response (one call, no token usage, ~8 s), which
is upstream and not the change; the ratios above exclude them.

**A sort returns the lines it sorted** (ADR 0040, turn contract 16). Artifact
turns spent four requests where three carried work: the model read the sorted
artifact back only because the sort result named a reference. The verified
result now carries up to eight lines, 120 characters each, and the model
answers from it.

`read_only_turn` is 103 tests; `ditto-kernel` suite green.

**Campaign 6 recorded the two changes on a bad provider day.** 130 attempts,
112 observed, 18 lost to empty responses (14%). Semantic passed at recall 1.000
on both profiles, artifact turns fell from 3.50 to 2.78 model requests, and all
turns from 2.66 to 2.34. The tool and cost_latency gates failed on the same 18
attempts (9 artifact attempts never reached a tool call; two genuine misses,
T3/clean#4 and X4/long_use#5), so readiness is still `fail` pending a repeat on
a healthy provider. Do not read campaign 6 as a harness regression.

**Campaign 7 lost 28 attempts the same way** (21.5%, same signature: one
request, no token usage, no failure reason), so the evaluation runner now
re-asks exactly that signature up to two more times and records
`transport_retries`. Two campaigns whose loss rate moves from 0 to 14 to 21.5
percent on identical code measure the provider, not Ditto, and a gate verdict
must not depend on it.

**Campaign 9 is the readable one.** 129 of 130 observed after the retry, spend
$0.1826, and seven of eight gates decided: integrity, safety, semantic, tool,
schedule and resource pass; `answer` needs the second review; `cost_latency`
fails at p95 29.9 s clean and 45.5 s long_use against the frozen 27.5 s. Input
tokens per turn fell 4343 -> 3560 and requests 2.66 -> 2.34 since campaign 5,
while the median turn went 8.1 s -> 13.4 s, so the ceiling is measuring the
provider's day rather than the harness. Do not widen the ceiling to make the
gate pass; re-measure it deliberately or run in a faster window.

## State: campaign 12 passes all eight gates

130 of 130 attempts observed under `budgets-v3`; 1.75 model requests per turn
(prompt 2.06, tool 1.60, mixed 1.45, schedule 1.75), turn p50 4.0 s, $0.1326 for
the run, daemon admission p95 1.0 ms. The answer gate cleared after the reading
of 29 rule-flagged items and the agreed 20% second-review sample. Remaining
distance to the Task 043 bar (1.3 requests per turn) is the memory family at
1.90 and the tool family at 1.60.

## Latest verified slice: admission is the daemon's span (ADR 0043)

`budgets-v3` gates `daemon_admission_ms` (journal: claimed occurrence to admitted
turn, 0-1 ms measured) and keeps the client's command round trip as a recorded
number, because that round trip is the store's fsync tail on this host (p50 0.74 ms
on tmpfs, 2.35 ms on the SD card). No ceiling changed; a manifest check refuses a
v3 file that relaxes admission. `scripts/test-eval-task017.py` covers it with 41
tests.

## Latest verified slice: Task 017, the cost_latency split (ADR 0042)

`budgets-v2.json` (schema version 2) gates the `cost_latency` family on
`harness_ms` - the turn's completion less the sum of its model request spans,
recorded in the existing `resources.ditto_overhead_ms` field - together with the
model round trips and the input tokens, while the absolute turn time and the
provider's own span are recorded with a same-day baseline and do not gate.
`budgets-v1.json` keeps its meaning and stays readable; every ceiling in v2
equals its v1 value, so nothing was loosened. `scripts/test-eval-task017.py`
covers the split (35 tests). Not yet exercised by a full campaign: the next run
is the first that grades under v2.

The granted `artifact.sort` now runs at the start of the turn (ADR 0041
amendment): it is performed from the grant's own arguments when the grant does
not allow deduplication, its output enters the compilation in canonical receipt
order, and the tool events that produce it are journaled before that compilation
so replay derives the same context. The accepted consequences are that a
cancelled turn may already carry a published sort, and that the model cannot
re-sort because the one-shot lease is spent. Three `model_sort` tests were
rewritten to that contract; the cancellation test now asserts the race-free
invariants (no published output, at most one recorded call, at most one model
request) instead of a fixed event count.

## Latest work: Task 042, a harness-side search budget

- A turn makes at most three `memory.search` calls; a fourth returns
  `limit_reached` without searching ([ADR 0029](../adr/0029-memory-search-tool.md)).
  The tool stays on the surface, so the prompt prefix and the stable-surface
  invariant are unchanged, and replay recomputes both the search and the
  refusal. The turn-scoped counter lives in `TurnScope.memory_searches`, and
  `tool_replay` carries `memory_search_count` so journals recorded before the
  budget still replay.
- Why: one `P2`/`long_use` turn in campaign 4 spent seven reworded searches
  (`reading group`, `book club`, `Birch Room`, `get-together venue`, `party
  venue pub restaurant`, `reading`, `gathering place meeting`) after it had
  already read the fact, and failed `bound_exceeded` with no answer.
- Measured against the alternative: prompt edits asking the model to stop cut
  that loop but also stopped a legitimate pointer hop (`P3` recall 17/20 to
  15/20), and a pointer-aware wording raised requests from 5.2 to 6.2. Two
  searches lost a fact a third query would have found (recall 5/5 to 4/5);
  three restored it at 4.8 requests and a 10.7 s median against 5.2 and 11.4 s.
  Five attempts per profile, so this is recorded as a bound, not a gain; the
  loop did not reproduce in any later round.
- Kernel tests: 101 in `read_only_turn`, `agent-check.sh` exit 0.

## Latest verified slice: Task 036

- [Contract and evidence](tasks/036-reading-found-pages.md) (ADR 0037), turn
  contract 14. `web.browse` reads the pages the turn's searches returned as
  well as the user's links, under one read lease whose exact resources grow
  with each search's results; no event or payload changes.
- Its gate passed on its final code (595 Rust tests; only documents changed
  afterwards, and the canary passed again); the browser end-to-end check
  passed 34 of 34, and resident memory stayed at 11.6 MB idle and at most
  17.2 MB with four runs at once.

## Previous slice: Task 035

- [Contract and evidence](tasks/035-two-agent-tools.md) (ADR 0036): two
  agent tools, `memory.manage` and `web.browse`, on one `tool.*` lifecycle;
  a minimal request shrank from 4,538 to 3,571 bytes. Its gate passed on its
  final tree (592 Rust tests).

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
  that service sees each query. Ditto reads at most three pages per answer,
  from the user's links and its searches' results; links inside a read page
  are not followed. A page that injects instructions could steer a later
  query and choose which returned result Ditto opens, a narrow channel ADR
  0037 accepts.

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
