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

## Repository state (2026-11-13)

- **main** is `756f256` (PR #32), pushed. CI ran `./scripts/agent-check.sh` and
  the MSRV check green on that commit. origin also keeps the merged branch
  `dev/task-037-eval-suite`, which has not been deleted, and the tag
  `archive/codex-scaffold` for the unmerged early scaffold.
- **Branch `dev/review-fixes-2026-11-13`** carries the fixes from a codebase
  review on top of main. It is local and not pushed. Its result is in
  "Review fixes" below.
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
- **Campaign evidence was lost.** The reports cited for campaigns 15 and 16
  were written to `~/.hermes/cache/scratch` and were gone on 2026-11-13, and no
  digest was recorded ([Task 043](tasks/043-requests-per-turn.md),
  "Evidence"). [QUALITY-GATES](QUALITY-GATES.md) "Campaign evidence" now states
  where a campaign must live before it is cited.
- `target/` holds 38 GB on a 235 GB SD card that is 77% full. It also holds
  the `target/task0xx` measurement directories that the earlier evidence names,
  so it has not been cleaned.

## Current system

- **Runtime and storage.**
  - The Rust daemon (axum, loopback by default) and the CLI.
  - The SQLite event spine (schema 8) is append-only by trigger and is the sole
    durable authority. Schedule and repeat indexes are projected in the same
    transaction. Schema 8 replaced the sort-only partial index with
    `events_turn_tools`, over `tool.started`/`tool.output`.
  - Artifacts are SHA-256 content-addressed, with verified reads.
  - The context projection (`context-projection.db`, schema 4) is a
    rebuildable, digest-verified cache, replayed once at open and then
    delta-verified.
  - Storage work runs on blocking threads, and debug builds reject journal
    access on threads that drive async tasks.
- **Profiles.** A daemon serves exactly one profile (ADR 0044).
  - `default` is the data directory itself; any other name lives under
    `<data>/profiles/<name>`.
  - Names are validated (a-z, 0-9, `-`, `_`, at most 32), so `..` is refused by
    construction.
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
  same-origin-only CSP. While it is bound to loopback, every route refuses a
  non-loopback host name, which blocks DNS rebinding. A non-loopback bind needs
  `--allow-unauthenticated-remote`, and then no model provider can be enabled.
  ADR 0045 records why instruction writes follow the memory-write ingress rule
  on such a bind.

  `ditto telegram` (ADR 0025) relays allowed private chats and delivers
  scheduled results. Clients never choose actors, kinds or internal metadata.
  SSE subscribes first, replays a bounded high-water snapshot in pages and
  recovers gaps from storage.
- **Context.**
  - Explicit memories promote exact same-session user input. A correction
    supersedes one active memory, and listing rechecks source text.
  - Runs compile the verified active session/task snapshot with the
    complete-set contract (ADR 0021): the whole current set when it fits the
    budget (default 900 estimated tokens, about twelve short memories),
    otherwise positive lexical overlap without function words.
  - A memory that places an object travels with the memory that names it: the
    pointer closure, up to two nodes, `policy-required: memory pointer`
    (ADR 0029).
  - Ditto remembers, replaces and forgets memories on its own (ADR 0031). They
    are labeled `model`/`inferred`, are refused after a turn read a web page or
    file and for credential-shaped facts, and are limited to three writes per
    turn.
  - There is no transcript injection, cross-session memory or production
    semantic retrieval.
- **Memory search** (`memory.manage` search, ADR 0029).
  - Search scope: the memories the turn's compilation saw, including those it
    left out.
  - Wordings: up to four in one call, unioned and ordered by the best query.
    At most three searches per turn.
  - Matching (from turn contract 20): single Hangul syllables are words; a
    word ending in Hangul also matches through the stem left by the longest of
    25 listed Korean particles when two characters remain; a shared word weighs
    `1/n` by how many searched memories contain it.
  - Limit: the search is lexical, so "pet" does not find "dog", and "키는"
    does not find "키".
- **Standing instructions and skills.**
  - `instructions.recorded` is the profile's one instruction document (at most
    2,000 characters, superseded by the next). A turn pins the document in
    force after the core instructions and records its text (ADR 0045).
  - Skills are `<profile>/skills/<name>/SKILL.md`, with a closed frontmatter
    (`name`, a one-line `description`) and a body of at most 64 KiB. A document
    is read once, at most 68 KiB, regular files only.
  - The catalogue (one line per skill, at most 4,000 characters) leads the
    request and is recorded in the turn. `skill.read` returns one body on
    demand (ADR 0046).
  - A malformed or misnamed skill directory stops the turn at admission by
    name. Hidden entries and plain files are not skills.
- **Web.** `web.browse` (crate `ditto-web-fetch`, ADR 0036):
  - Reads pages linked in the user's message (ADR 0027): exact-URL leases,
    public addresses only with pinned connections, manual redirects, size and
    time bounds.
  - With `--search-url`, searches on its own through a SearXNG-compatible
    service (ADR 0033).
  - Reads the pages its searches return (ADR 0037).
  - With a search service, three reads per turn are shared between the user's
    links and found pages.
  - Results return as untrusted content, and replay uses the journaled ones.
- **Capabilities.**
  - Generated package headers keep full manifests out of startup and search.
  - Live invocation uses the closed Invocation Schema Profile V1 through a
    sealed `LiveExecutionEpoch`.
  - `capability::install` (ADR 0048) writes a package only at the digest the
    caller pins. A different digest for an installed id is a conflict, and
    removal needs the installed digest and a valid id. A headerless package
    counts as installed. It is a library act with no route yet.
  - `device.process.run` is a discovery-only manifest with no runner.
- **Policy.**
  - Sealed canonical invocations carry harness-derived effect, resource and
    placement.
  - `artifact.read`, memory searches and `skill.read` use static no-approval
    permits.
  - `artifact.sort`, web reads and web searches each consume one-shot
    `ExecutionClaim`s under their own exact-resource leases. Memory writes
    share one three-call lease.
- **Model and turns.**
  - `ditto-model` owns the provider-neutral request/stream contract.
    `ditto-model-openai` holds the closed `gpt-5.6` Responses profile and an
    OpenAI-compatible `/chat/completions` driver (ADR 0023). The daemon
    defaults to a disabled provider.
  - There is one turn contract, **version 20** (ADR 0034). Replay rejects turns
    recorded under any other version, while status and thread history still
    read their answers.
  - A turn compiles context and pages its tools from one table of builtins:
    - `web.browse` while reading or searching is enabled;
    - `memory.manage`;
    - `skill.read` when the profile carries a skill;
    - `artifact.read`, plus the permitted `artifact.sort`, with an attachment.
  - It may prefetch an attachment and a sort the permission fixes before the
    first request (ADRs 0041, 0047).
  - It runs at most eight model requests and at most four ready calls per
    response, in order (ADR 0038).
  - It journals versioned transitions before publication and replays without
    provider, artifact or network I/O. Agent tool calls share one
    `tool.requested`/`tool.started`/`tool.output` lifecycle (ADR 0036).
  - Answers stay `unverified`, and model runs never emit `task.completed`.
- **Local work and schedules.**
  - The closed `/usr/bin/sort` profile (64 KiB / 4,096 lines, five seconds)
    has an independent verifier.
  - One-shot schedules (ADR 0019) and finite repeats (ADR 0020) share a
    bounded index of 100 future intents and one event/timer-driven scheduler,
    with at-most-once claims and no automatic retry or housekeeping model call.
  - Four runs and sorts are active at once in total (ADR 0035).
- **Measurement.**
  - Task 014 recorded an offline RAM/latency/accounting baseline. Tasks 015–016
    recorded five literal synthetic queries at zero and 1,000 unrelated
    memories.
  - [Task 037](tasks/037-eval-suite-runner.md) froze suite v1 and the
    `eval_task017.py` runner, with budgets v1–v3 (ADRs 0042, 0043).

## Measured state

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
  cannot be re-checked. No campaign has measured contract 20, which changes
  memory search ranking.
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
  idle and at most 17.2 MB with four runs at once. It has not been re-run since.

## Review fixes (2026-11-13, branch `dev/review-fixes-2026-11-13`)

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
  - Tests: the install tests, 9 passed (3 new).
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
- **Full gate:** see "Gate on the review branch" below.

## Gate on the review branch

Not yet run.

## Known gaps

- After a reload the web app restores only finished exchanges of the current
  thread. Phone access to the web app still needs an authenticated gateway, so
  Telegram is the phone path for now.
- The Telegram gateway keeps pending replies in memory, and handles only text
  messages. A fifth message while four answers run waits up to three minutes.
- Acknowledgments are a closed list checked against the last line of the latest
  answer. An offer phrased without a question mark, followed by "ok", is taken
  as an acknowledgment.
- `web.browse` decodes pages as UTF-8 and runs no JavaScript. A page that
  injects instructions could steer a later query and choose which returned
  result Ditto opens, a narrow channel that ADR 0037 accepts.
- Answer quality and tool-use reliability with local or hosted
  OpenAI-compatible models are unmeasured.
- Memory recall is lexical on both paths. Past the context budget, a pure
  paraphrase is reached only if the model searches with matching words. Korean
  folding stops at one-syllable stems. P1 and P2 still pay one search for a
  fact the capsule left out; closing that needs a retrieval bridge (Task 043).
- The evaluation runner does not report requests per turn, and schedule-gate
  aggregation counts attempts rather than turns (Task 043).
- Memories Ditto infers can be wrong, and its credential check is a heuristic.
  Forgetting removes a memory from use and view, not from the append-only
  journal.
- The scheduler sleeps on a monotonic timer and has no resume wake-up.
- `skill.read` has run only against the scripted-model integration suite, with
  no live provider turn.

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

- **Large units.**
  - `turn::replay` holds one function of about 900 lines (`replay`).
  - `crates/context/src/lib.rs` is 5,300 lines, half of them tests.
  - The context-projection authority workflow is one large module.

  Split any of them without weakening their single-gate and replay semantics.
- **Shared lifecycle, separate paths.** Agent tools share the `tool.*`
  lifecycle, but each keeps its own run path and replay checks, and the legacy
  `artifact.read` turn still journals `capability.requested`/`execution.*`.
- **Three lexical tokenizers** differ: context V1, capability search and
  retrieval V2. The memory search has its own search-word rules on top of
  context V1.
- **Journal size.** Journal bytes per answer byte are 13.4 (200-delta answer)
  and 4.3 (1,000-delta answer), against ADR 0028's target of 3.
  `context.compiled` records each included memory in full.
- **Artifact and memory paging.**
  - Artifact-root authorization pages through session history, and range reads
    verify the whole object.
  - Memory listing materializes the bounded active snapshot before paging.
- **Locking.** The run slot holds a `std::sync::Mutex` across storage writes,
  and one connection lock serializes every journal read and write.
- **Small items.**
  - Headerless capability packages still pay one bounded startup body read.
  - The injected embedding interface is synchronous and may make up to 513
    serial calls.
  - Legacy excluded-context receipts have no independent encoded payload
    ceiling.
  - Generic model-task completion admission is a high-water check followed by
    an append, not one atomic transaction.
