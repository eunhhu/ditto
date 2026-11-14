# Verified handoff

Current state only. Per-slice history lives in `tasks/*-evidence.md`, the task
files and git. Replace superseded facts instead of appending. Update this file
only after code and checks establish a new fact.

## Product intent

- On 2026-09-06 the user confirmed the product: a local-first personal
  general-purpose agent, with Hermes as a positioning reference (not a
  benchmark). Target problems: process memory, context efficiency, memory and
  scheduled-job reliability, sustainable self-improvement, task performance and
  latency. See [product intent](../product.md).
- Zero cost and zero overhead, including implementation effort and future debt,
  are primary premises. Whether external model/API charges must also be zero is
  an open clarification; no zero-cost or superiority result is claimed.
- On 2026-09-30 the user redirected the frontier toward a daily-driver
  assistant that can stand in for OpenClaw, Hermes, Grok bots, Muse and Dot.
  On 2026-11-06 the user asked to finish Ditto quickly so it can replace the
  current assistant, and for a comparison with Hermes, OpenMausBot and
  Harness² ([comparison-2026-11](../design/comparison-2026-11.md)). No parity
  claim is made.
- On 2026-11-13 the user named the north star: a secretary-type, always-on
  harness whose headline result is self-improvement, with everything
  inspectable and controllable ([product intent](../product.md), "North
  star").
  - [The plan](../design/self-improvement-plan-2026-11.md) records where the
    code stands against it and the build order NEXT now follows.
  - On 2026-11-14 the user answered its five decisions; ADR 0049 records
    them.
    - It amends the housekeeping invariant in `AGENTS.md`, allowing a
      bounded, detector-woken retrospection.
    - It amends ADR 0034: one contract until 1.0.
  - None of that plan is implemented yet.

## Repository state (2026-11-14)

- **main** is `9b4944b`: PR #33, the review fixes, merged by the user on
  2026-11-13. CI ran `./scripts/agent-check.sh` and the MSRV check green on
  that commit.
- **origin** also keeps the merged branch `dev/review-fixes-2026-11-13`, which
  has not been deleted, and the tag `archive/codex-scaffold` for the unmerged
  early scaffold. The merged `dev/task-037-eval-suite` was deleted on
  2026-11-13.
- **Tasks 038–041** are specs. Only 038's search half is implemented.
  - [Task 042](tasks/042-harness-only-self-improvement.md): only
    `scripts/probe_variant.py` and its tests exist.
  - [Task 044](tasks/044-platform-surface.md): P1–P3 are done and P4 has its
    library slice.
- **Raw measurement reports are not tracked.** The reports of Tasks 014–016
  remain retrievable with
  `git show 58b2b02:docs/agent/tasks/<report>.json`, and their evidence files
  record the digests. The canary rejects tracked `docs/**/*.json` and any
  tracked file above 1 MiB.
- **No campaign report can be re-inspected.**
  - Only campaigns 15 and 16 have recorded paths, under
    `~/.hermes/cache/scratch`, and those were gone on 2026-11-13
    ([Task 043](tasks/043-requests-per-turn.md), "Evidence").
  - Campaigns 1–14 and the probes never had a location recorded.
  - A search of this host (the home directory to depth 9, `/tmp`, `target/`
    and `.ditto/`) found no directory named `campaign*` or `probe-*`.
  - No campaign report's SHA-256 is recorded in the repository.

  Every campaign number in the task files is therefore a documented summary,
  not checkable evidence. [QUALITY-GATES](QUALITY-GATES.md) "Campaign evidence"
  now states where a campaign must live before it is cited.
- On 2026-11-13 `target/debug` (37 GB of build cache) was removed at the
  user's request. `target/` is now 848 MB, and the SD card went from 77% to
  60% full. `target/release` and the measurement directories the earlier
  evidence names (`target/task0xx`, `target/review-2026-11-13`,
  `target/web-e2e`) were kept. Do not run `cargo clean` there.

## Current system

- **Runtime and storage.**
  - The Rust daemon (axum, loopback by default) and the CLI.
  - The SQLite event spine (schema 8) is append-only by trigger and the sole
    durable authority. Schedule and repeat indexes are projected in the same
    transaction. Schema 8 replaced the sort-only partial index with
    `events_turn_tools`, over `tool.started`/`tool.output`.
  - Artifacts are SHA-256 content-addressed, with verified reads.
  - The context projection (`context-projection.db`, schema 4) is a
    rebuildable, digest-verified cache, replayed once at open and then
    delta-verified. Replay and deltas read only context nodes, through the
    kind index.
  - Storage work runs on blocking threads: daemon handlers use the blocking
    pool, and runs, sorts and the scheduler journal from blocking threads.
    Debug builds reject journal access on threads that drive async tasks.
- **Profiles.** A daemon serves exactly one profile (ADR 0044).
  - `default` is the data root itself; any other name lives under
    `<data>/profiles/<name>`.
  - Names are validated (a-z, 0-9, `-`, `_`, at most 32), so `..` is refused
    by construction.
  - CLI: `ditto profile list|path`; daemon: `ditto-daemon --profile`
    (`DITTO_PROFILE`).
- **Ingress.** Typed commands only:
  - record-only input;
  - memory save/list/correct/forget;
  - run/status/cancel;
  - conversation reset and view;
  - the standing instructions (`GET/PUT /v1/instructions`);
  - loopback-only sort, schedule and repeat.

  A run whose text only acknowledges an answer that asked nothing is recorded
  as `acknowledged`, without a turn (ADR 0035).

  The daemon also serves the embedded web app (ADR 0024) under a
  same-origin-only content security policy. While it is bound to loopback,
  every route refuses a request addressed to a non-loopback host name, which
  blocks DNS rebinding. A non-loopback bind needs
  `--allow-unauthenticated-remote`, and then no model provider can be enabled.
  ADR 0045 records why instruction writes follow the memory-write ingress rule
  there.

  `ditto telegram` (ADR 0025) is a CLI client that relays allowed private
  Telegram chats and delivers scheduled results. Run status names the schedule
  that started a run. The daemon and the offline fixture server share one
  route table.

  Clients never choose actors, kinds or internal metadata; the kernel derives
  all authority, including an attached file's sort permission. SSE subscribes
  first, replays a bounded high-water snapshot in pages, and recovers gaps or
  lag from storage.
- **Context.**
  - Explicit memories promote exact same-session user input. A correction
    supersedes one active memory, and listing rechecks source text.
  - Runs compile the verified active session/task snapshot with the
    complete-set contract (ADR 0021): the whole current set when it fits the
    budget (default 900 estimated tokens, about twelve short memories),
    otherwise positive lexical overlap without function words.
  - A memory that places an object travels with the memory that names it:
    the pointer closure, up to two nodes, `policy-required: memory pointer`
    (ADR 0029).
  - With `memory.manage`, agent runs search the memories their compilation
    saw, including those it left out (ADR 0029). Ditto remembers, replaces and
    forgets memories on its own (ADR 0031). Each written memory is a node
    sourced by a session-scoped `memory.written` event and labeled origin
    `model`, status `inferred`. Writes are refused after the turn read a web
    page or file, for credential-shaped facts, and past three per turn.
  - There is no transcript injection, cross-session memory or production
    semantic retrieval. The separate V2 joint working-set query is lexical in
    production, with an injected embedding seam for tests.
- **Memory search** (the `search` action of `memory.manage`).
  - One call carries up to four wordings, unioned and ordered by the best
    query. A turn may search at most three times.
  - Matching (from turn contract 20):
    - single Hangul syllables count as words;
    - a word ending in Hangul also matches through the stem left by the
      longest of 25 listed Korean particles, when two characters remain;
    - a shared word weighs `1/n` by how many searched memories contain it.

    The compiler's own selection keeps exact words.
- **Standing instructions and skills.**
  - `instructions.recorded` is the profile's one instruction document: at most
    2,000 characters, and superseded by the next one. A turn pins the document
    in force after the core instructions and records its text (ADR 0045).
  - Skills are `<profile>/skills/<name>/SKILL.md`: a closed frontmatter
    (`name`, a one-line `description`) and a body of at most 64 KiB. A
    document is read once, at most 68 KiB, and only if it is a regular file.
  - The catalogue (one line per skill, at most 4,000 characters) leads the
    request and is recorded in the turn. `skill.read` returns one body on
    demand (ADR 0046).
  - A malformed or misnamed skill directory stops the turn at admission by
    name. Hidden entries and plain files are not skills.
- **Web.** `web.browse` (crate `ditto-web-fetch`, ADR 0036):
  - It reads pages linked in the user's own message (ADR 0027) under
    exact-URL leases: public addresses only, with pinned connections, manual
    redirects, size and time bounds, and text extraction.
  - With `--search-url`, it searches on its own through a SearXNG-compatible
    service (ADR 0033). The endpoint is the only resource a three-call lease
    allows, credential-shaped queries are refused, and a search returns at
    most five bounded results.
  - It reads the pages its searches return (ADR 0037). From each search's
    output on, that search's result URLs join the turn's read grant and lease.
    With a search service, three reads per turn are shared by the user's
    links and found pages.
  - Its schema offers only what the deployment enables. Results return as
    untrusted content, and replay uses the journaled ones.
  - The instructions tell the model to work on its own and to hand off only
    what needs the user, ending its answer with the question.
- **Capabilities.**
  - Generated package headers keep full manifests out of startup and search.
    A selected manifest is paged after digest and projection checks.
  - Live invocation uses the closed Invocation Schema Profile V1 through a
    sealed `LiveExecutionEpoch`. Replayable `ExecutionEpochEvidence` carries
    no authority.
  - `capability::install` (ADR 0048) writes a package only at the digest the
    caller pins:
    - a different digest for an installed id is a conflict;
    - removal needs the installed digest and a valid capability id;
    - a headerless package counts as installed.

    It is a library act, with no route yet.
  - `device.process.run` is a discovery-only manifest with no runner.
- **Policy.**
  - Sealed canonical invocations carry harness-derived effect, resource and
    placement. One affine ticket per epoch creates one expiring ledger.
  - `artifact.read`, memory searches and `skill.read` use static no-approval
    permits.
  - `artifact.sort`, web reads and web searches each consume one-shot
    `ExecutionClaim`s under their own exact-resource leases. Memory writes
    share one three-call lease without resources.
  - The kernel adds exact resources to a registered lease, keeping its budget
    and expiry, only for the pages a search returned.
- **Model and turns.**
  - `ditto-model` owns the provider-neutral request/stream contract.
    `ditto-model-openai` holds two drivers on one request lifecycle:
    - the closed `gpt-5.6` Responses profile (ephemeral storage);
    - an OpenAI-compatible `/chat/completions` driver for an
      operator-configured local or hosted server (ADR 0023): HTTPS unless
      loopback, no redirects, and an optional key only from
      `DITTO_MODEL_API_KEY`.

    Keys are redacted and transport-only. The daemon defaults to a disabled
    provider.
  - The kernel turn loop compiles context and pages its tools from one table
    of builtins:
    - an ordinary agent run gets `web.browse` while reading or searching is
      enabled, and `memory.manage`;
    - `skill.read` is added while the profile carries a skill;
    - an attachment adds `artifact.read` and the permitted `artifact.sort`;
    - a legacy artifact turn gets `artifact.read`.

    It may run an attachment read, and a sort the permission fixes, before
    the first request (ADRs 0041, 0047). It runs at most eight model requests
    and at most four ready calls per response, one after another (ADR 0038).
    It journals versioned transitions before publication and replays without
    provider, artifact or network I/O. Agent tool calls share one
    `tool.requested`/`tool.started`/`tool.output` lifecycle (ADR 0036).
  - There is one turn contract, **version 20**, until 1.0 (ADRs 0034, 0049). Replay rejects
    turns recorded under any other version, while status and thread history
    still read their answers. The contract carries:
    - typed failure reasons (ADR 0021);
    - the thread's recent exchanges as native messages, in a stepped window
      (ADRs 0022, 0028);
    - the assistant instructions, the profile's standing instructions and
      skill catalogue (ADRs 0045, 0046);
    - the local time and the session's runs still in flight, as notes in the
      latest message (ADRs 0026, 0028, 0035);
    - each fact recorded once: text in chunks, each request as its SHA-256,
      the selection by contract digests (ADRs 0028, 0030).
  - A run reuses its session's verified context while no context node has
    been committed since (sessions with task-scoped or windowed nodes are
    recompiled). It keeps each session's thread between turns, and commits
    `context.compiled` and `capabilities.selected` with its first model
    request. Builtin tool contracts are validated once per process.
  - `ditto chat` is an interactive client with `/new` and `/remember`;
    `ditto new` starts a thread. Answers stay `unverified`, and model runs
    never emit `task.completed`.
  - The loop is split into stage functions: context, capability selection,
    request dispatch, stream admission, tool execution and finish.
    Cancellation and deadline checkpoint messages are one shared `Checkpoint`
    table, used by runtime and replay.
- **Local work and schedules.**
  - The closed `/usr/bin/sort` profile: 64 KiB / 4,096 lines, five seconds, a
    cleared environment, private scratch and process-group cleanup. It has an
    independent verifier and a sort-specific completion.
  - One-shot schedules (ADR 0019) and finite repeats (ADR 0020) share a
    bounded index of 100 future intents and one event/timer-driven scheduler.
    Claims are at most once, missed and interrupted states are visible, and
    there is no automatic retry or housekeeping model call.
  - Four runs and sorts, including scheduled dispatch, are active at once in
    total, from one session or several (ADR 0035). Other immediate requests
    are rejected as busy, and scheduled work waits for a slot.
- **Measurement.**
  - Task 014 recorded an offline RAM/latency/accounting baseline. Tasks 015–016
    recorded five literal synthetic queries at zero and 1,000 unrelated
    memories.
  - Corpus schemas: 3 (Task 016.1) derives expected capsules from the
    complete-set rule; 4 (Task 025) from its version-6 ID-ordered
    presentation; 5 (Task 027) binds each driver observation to its journaled
    request digest. None of these offline measurements covers answer quality,
    semantic recall at scale, tool-task success, live cost or v0.1 readiness.
  - The live measurements come from the Task 017 suite and runner
    ([Task 037](tasks/037-eval-suite-runner.md), budgets v1–v3, ADRs 0042 and
    0043).

## Measured state

- **Campaign 12** (budgets v3; Task 017, "Campaign 12") is the only recorded
  run on which **all eight gates passed**, including `answer`:
  - 1.75 model requests per turn;
  - turn p50 4.0 s;
  - spend $0.1326;
  - daemon admission p95 1.0 ms.

  For `answer`, the 29 rule-flagged items were read, one retrieval miss was
  recorded (P1/long_use#4), and the fixed 22-item 20% sample was reviewed
  separately and agreed, as that file records.
- **Campaign 16** (contract 19, 130 attempts, 145 turns):
  - 1.172 requests per turn;
  - Recall@5 1.000 (90/90);
  - spend $0.1013, which is $0.00070 per turn;
  - completion p50 5.4 s, with harness overhead p50 35 ms (0.9% of model
    time);
  - every gate passed except `answer`, which waits on the owner's 20% second
    review.

  Those figures are recorded in
  [Task 043](tasks/043-requests-per-turn.md), but the report is gone and
  cannot be re-checked.
- **Contract 20 is unmeasured.** No campaign has run it, and it changes how
  memory search ranks.
- **Task 042 probes** on the memory two-hop case: base 10/10 answers with
  Recall@5 18/20, and both variants were `KEEP_BASE`. That family is saturated
  for wording edits.
- **Harness baseline** (2026-09-30, Raspberry Pi 5, release build, instant
  loopback model, `scripts/measure-harness.py`, before and after ADR 0028):
  - work before dispatch: 5–10 ms → 1 ms;
  - journal: 206 or 1,006 events per turn → 8 or 9, and 81–107 → 13.4 or 4.3
    journal bytes per answer byte;
  - median prompt prefix reuse: 50.6% → 96.4%;
  - concurrency: one of three simultaneous sessions accepted → all three, and
    four runs in one session.
- **Last browser end-to-end run** (Task 036): 34/34 passed, with RSS 11.6 MB
  idle and at most 17.2 MB with four runs at once. The review session of
  2026-11-13 did not re-run it, and the gate does not run it either.

## Review fixes (2026-11-13, PR #33, merged)

A review of main found the problems below, and each commit fixes one of them:

- **Skills (ADR 0046 amended).**
  - The problem: a directory under `skills/` whose name the format refuses was
    silently left out of the catalogue, and each `SKILL.md` was read twice and
    unbounded.
  - The fix: it is now reported, hidden entries and files are ignored, and a
    document is read once, at most 68 KiB, and only if it is a regular file.
  - Tests: `ditto-profile` 13 passed (3 new).
- **Pinned install (ADR 0048 amended).**
  - The problem: `uninstall` joined an unchecked id onto the root and removed
    it recursively, and a headerless package was overwritten without a
    conflict and could not be uninstalled.
  - The fix: an id outside the grammar is `InvalidId`, a directory naming
    another id is `Mismatched`, and installed state is read the way the
    catalogue reads it.
  - Tests: the install tests, 9 passed (3 new). One of them was later
    strengthened to check `install` too.
- **Turn contract 20 (ADR 0029 amended, Task 038's search half).**
  - The change: Korean particle folding, single Hangul syllables, rarity
    ranking, and 24 stray spaces removed from the prefetched sort's note.
  - The compiler's tokenizer, selection and pointer closure are untouched, and
    every existing context and kernel test passed without editing an expected
    value.
  - Tests: `ditto-context` 62 (4 new); `ditto-kernel` lib 46,
    `read_only_turn` 116 (1 new, a Korean search that replays),
    `durable_context_projection` 15, `sort_runs` 5, `user_memory` 8;
    `test-personal-quality.py` 22 OK.
- **Documents.** NEXT, the event protocol (versions 15–20,
  `instructions.recorded`, prefetch origin), the architecture version history,
  the README (profiles, instructions, skills), the ADR 0041 status, ADR 0045's
  ingress decision, the Task 044 file and the campaign-evidence rule.
- **Full gate:** see "Last full gate" below.

## Last full gate

CI ran the same script green on the PR's last commit `3e789fe` and on the
merge commit `9b4944b`. The local run below is the one with a kept log.

`./scripts/agent-check.sh` **exited 0** on `1652636` (2026-11-13, Raspberry
Pi 5, 9 min 53 s wall clock). The host load average was about 4–5 from other
work when it started. The log is kept at
`target/review-2026-11-13/agent-check.log` (SHA-256
`2e0bcfd3ba179772e12c6f3b624b1f46b3d3545cf0937c36f0221af146ac3871`).

| Part of the gate | Result |
| --- | --- |
| canary, fmt, clippy `-D warnings` | passed |
| workspace tests | 642 passed, 8 ignored |
| built-CLI scenarios | the seven ignored ones, run one by one: 7 passed |
| `test-personal-baseline.py` | 8 OK |
| `test-personal-quality.py` | 22 OK |
| `test-eval-task017.py` | 43 OK |
| `test-probe-variant.py` | 9 OK |
| web renderer checks | 12 cases passed |
| offline baseline and five-case corpus | passed |

Changes after that commit:

- `90c1b9d` changed only documents. The canary passed on it.
- The next commit strengthens one install test, so that it also checks that
  `install` refuses a directory holding another id.
  - `cargo test -p ditto-capability --lib install`: 10 passed (the 9 install
    tests plus one catalogue test the filter matches).
  - clippy on `ditto-capability` passed.
  - The full gate was not re-run for that test-only change.

Not run in this session:

- `node scripts/web-e2e.js`, the browser run that the gate does not include;
- any live provider campaign.

## Known gaps

- **Web app reload.** After a reload the web app restores only finished
  exchanges of the current thread, in the order they finished. Failed or
  cancelled turns, acknowledgments and the lines said before a tool call are
  not restored, and a run still in flight at load appears when it finishes.
- **Phone access.** The web app needs an authenticated gateway for phone
  access; Telegram is the phone path for now.
- **Telegram.** The gateway keeps pending replies in memory, so a gateway
  restart during an answer loses that reply unless Telegram redelivers the
  update. Only text messages are handled. A fifth message while four answers
  run waits up to three minutes for a slot.
- **Acknowledgments.** They are a closed list checked against the last line of
  the latest answer. An offer phrased without a question mark, followed by
  "ok", is taken as an acknowledgment; the instructions avoid this by asking
  the model to end hand-offs with a question.
- **Page decoding.** `web.browse` decodes pages as UTF-8 and runs no
  JavaScript, so pages in legacy charsets or rendered by scripts yield garbled
  or little text.
- **Web search.**
  - It needs a SearXNG-compatible service the operator runs or trusts, and
    that service sees each query.
  - Ditto reads at most three pages per answer, from the user's links and its
    searches' results, and does not follow links inside a read page.
  - A page that injects instructions could steer a later query and choose
    which returned result Ditto opens. ADR 0037 accepts this narrow channel.
- **Model quality.** Answer quality and tool-use reliability with local or
  hosted OpenAI-compatible models are unmeasured, and tools need a model with
  function calling.
- **Lexical recall.**
  - Sessions whose current memories exceed the context budget fall back to
    lexical overlap, so a pure paraphrase can miss.
  - The model can search the left-out memories in its own words (Task 029),
    but that search is lexical too and runs only when the model chooses.
  - Korean folding stops at one-syllable stems ("키는" does not find "키").
  - P1 and P2 still pay one search for a fact the capsule left out.

  Semantic retrieval or a retrieval bridge (deferred, Task 043) or a larger
  budget (a cost decision for the user) would close it.
- **Inferred memories.**
  - They can be wrong, and the credential check is a heuristic.
  - Writes are refused after a web page or file is read in the same turn, but
    injected text that reached Ditto's own earlier answers in the thread is
    not caught.
  - Forgetting removes a memory from use and view, not from the append-only
    journal.
  - The web app shows its notice of a write only live; after a reload the
    memory list still marks Ditto's memories.
- **Scheduler.** It sleeps on a monotonic timer and has no resume wake-up, so
  a suspended laptop may delay or miss a start window. This has not been
  reproduced.
- **Evaluation runner.** It does not report requests per turn, and schedule
  gate aggregation counts attempts rather than turns (Task 043).
- **Skills.** `skill.read` has run only against the scripted-model
  integration suite, with no live provider turn.

## Intentionally deferred

- Calendar recurrence, scheduled effect grants and additional effectful tool
  profiles.
- Native non-OpenAI wire formats, reasoning replay, remote cancel and explicit
  prompt-cache breakpoints.
- Additional capability derivers, durable/cross-process authorization,
  approval fulfillment and the capability worker protocol; the install route,
  its event and lease, and the effect review (Task 044 P4).
- Device registry, general process profiles, SSH transport and secrets.
- A production embedding worker/provider and persisted embedding cache.
- Additional completion verifiers and the improvement compiler (Task 039).
- Authenticated remote gateway (phone or remote access to the web app).

## Known engineering debt

- **Artifact-root authorization** pages through session history, and range
  reads verify the whole object. Measure before adding indexes or caches.
- **Memory listing** materializes the bounded active snapshot before paging.
  Its 10,000-candidate and byte limits include other active session context.
- **Locking.** The run slot holds a `std::sync::Mutex` across storage writes,
  now on blocking threads, and one connection lock serializes every journal
  read and write. Measure with Phase D's parallel sessions before adding a
  writer thread or reader pool.
- **Context graph edges** are validated but unused in ranking.
- **Large units.**
  - The context-projection authority workflow is one large module.
  - `turn::replay` holds one function of about 900 lines (`replay`).
  - `crates/context/src/lib.rs` is about 5,300 lines, half of them tests.

  Split any of them without weakening their single-gate, single-rebuild,
  atomic-checkpoint or replay semantics.
- **Tokenizers.** Three lexical tokenizers differ: context V1, capability
  search and retrieval V2. The memory search adds its own search-word rules on
  top of context V1.
- **Tool paths.** Agent tools share one `tool.*` lifecycle (ADR 0036), but each
  keeps its own run path and replay checks. The legacy `artifact.read` turn
  still journals `capability.requested`/`execution.*`.
- **Journal bytes.** Journal bytes per answer byte are 13.4 for a 200-delta
  answer and 4.3 for a 1,000-delta answer, with twelve memories, against
  ADR 0028's target of 3. The largest remaining terms:
  - `context.compiled`, which records each included memory in full (about
    480 bytes each) although its own `context.node.recorded` event holds it;
  - `turn.finished`, which repeats the answer text so status and thread
    history read it without reassembling chunks.
- **Headerless capability packages** still pay one bounded startup body read.
- **Embedding interface.** The injected interface is synchronous and may make
  up to 513 serial calls. A production worker needs rerank pools, batching and
  caching.
- **Excluded-context receipts.** Legacy receipts have no independent encoded
  payload ceiling. Add one before accepting them as a new durable wire input.
- **Completion admission.** Generic model-task completion admission is a
  high-water check followed by an append, not an atomic verifier/admission
  transaction.
- **Process profiles.** The only process profile is bounded sort. Broader
  programs, host-crash containment and new verifiers need separate contracts.
