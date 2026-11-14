# Implementation frontier

Work in order unless the user explicitly changes priorities. Complete one task
file's exit criteria before moving the marker.

Checked against the code on 2026-11-13: main `9b4944b` (PR #33, turn contract
20; see [HANDOFF](HANDOFF.md)).

## Active

1. **Measure contract 20 and close the `answer` gate.** This is a
   [Task 017](tasks/017-evaluation-outcomes.md) campaign 17 over suite v1, under
   `budgets-v3`, with `--agent-check-exit 0`. Three things make it necessary:
   - campaign 16's files are gone ([Task 043](tasks/043-requests-per-turn.md),
     "Evidence");
   - contract 20 changes how memory search ranks;
   - the `answer` gate needs the owner's 20% second review on a campaign whose
     files still exist.

   Store the run as [QUALITY-GATES](QUALITY-GATES.md) "Campaign evidence"
   requires. It is a paid run (campaign 16 spent $0.1013), so it starts when the
   owner says so. The 2026-11-13 review proposed putting it first; the owner has
   not confirmed that order yet.
2. [038](tasks/038-korean-and-ranked-memory-search.md) Korean memory search.
   The search half is implemented in turn contract 20. What remains is
   `eval/task017/suite-v2.json` (P6, P7, X5, P8), its paid calibration, Korean
   Recall@5 ≥ 0.90 over 80 attempts, and the correction record (`ditto memory
   history`, `restore`, `until`).
3. [039](tasks/039-improvement-compiler-first-slice.md) The improvement
   compiler's first evidence-gated slice. Spec only.
4. [041](tasks/041-hermes-baseline.md) A measured Hermes baseline on the same
   memory workload. Spec only.
5. [040](tasks/040-durable-work-items.md) A durable work item for a long
   delegated task. After the first release, ADR first.

Items 2–5 keep the order recorded with the
[comparison](../design/comparison-2026-11.md) on 2026-11-06.

## Proposed order from the north star (not confirmed)

On 2026-11-13 the user named self-improvement as the headline result, with
everything inspectable and controllable ([product](../product.md), "North
star"). [The plan](../design/self-improvement-plan-2026-11.md) proposes this
dependency order. It is a proposal: the Active list above stays in force until
the user confirms an order and answers the plan's five decisions.

1. Real use and journaled feedback signals. No task file yet.
2. A learned layer of journaled profile documents. This widens
   [044](tasks/044-platform-surface.md) P2.
3. The first learning loop, retrieval keys from the model's own searches. It
   needs no model call, and it is the retrieval bridge
   [043](tasks/043-requests-per-turn.md) left open. It overlaps the
   retrieval-miss detector of
   [039](tasks/039-improvement-compiler-first-slice.md).
4. A search over past conversations, and workspaces. No task file yet.
5. Forgetting as a policy: usage accounting, expiry, the rest of
   [038](tasks/038-korean-and-ranked-memory-search.md), and erasure (ADR
   first).
6. A glass-box console with a learning ledger. No task file yet.
7. Tools that act and events that wake, with
   [040](tasks/040-durable-work-items.md) and 044 P4.

## Parallel tracks

Each has its own task file and open exit criteria:

- [044](tasks/044-platform-surface.md) The platform surface.
  - Landed: P1 profiles, P2's pinned instructions and P3 skills (ADRs
    0044–0046, turn contracts 17–19), and P4's library slice, the pinned
    install (ADR 0048).
  - Open: P2's model-written half, P4's event, lease, route and effect
    review, P5 provider conformance and P6 the native shell.
- [042](tasks/042-harness-only-self-improvement.md) Harness-only
  self-improvement. `scripts/probe_variant.py` exists. The first two probes
  were `KEEP_BASE`, and the memory two-hop family is saturated for wording
  edits.
- [043](tasks/043-second-generation-bar.md) The second-generation bar, and its
  [outcomes](tasks/043-requests-per-turn.md): 1.65 → 1.172 requests per turn
  on campaign 16 (ADR 0029 amendment, ADR 0047), which can no longer be
  inspected. Open:
  - the runner does not report requests per turn;
  - schedule-gate aggregation counts attempts, not turns;
  - first useful progress is 5.3 s against a 2 s target;
  - the retrieval bridge for capsule misses (P1, P2).
- ADR 0028's deferred parts: parallel read-only calls and the journal target
  of three bytes per answer byte.

## Completed

One line per slice; each task file and its evidence hold the details.

Tasks 038, 042, 043 and 044 are not listed here. Each has landed slices, but
its exit criteria are still open, so it appears above under Active or
Parallel tracks.

- Between the Task 017 campaigns:
  - the two-hop pointer closure (ADR 0029);
  - the sort's result lines (ADR 0040, turn contract 16), which HANDOFF and
    ADR 0040 filed under "Task 038" before that spec's own work began;
  - the read-only prefetch (ADR 0041).
- [037](tasks/037-eval-suite-runner.md) Suite v1, its runner and frozen
  budgets v1–v3 (ADRs 0038, 0042, 0043; turn contract 15).
- [036](tasks/036-reading-found-pages.md) Reading the pages a search found,
  turn contract 14 (ADR 0037).
- [035](tasks/035-two-agent-tools.md) Two agent tools and one tool
  lifecycle, turn contract 13 (ADR 0036).
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

## Numbering notes

- Task 043 has two files: the bar ([043-second-generation-bar](tasks/043-second-generation-bar.md))
  and its outcomes ([043-requests-per-turn](tasks/043-requests-per-turn.md)).
- ADR number 0039 was never used.

## Later

- calendar recurrence, notification delivery and scheduled effect grants only
  after their product need and policy contracts are defined;
- batched, compact rerank pools and descriptor/hash caching before a production
  embedding worker;
- device registry, SSH placement, and expanded gateway integrations after the
  local workflow is useful;
- evidence-gated improvement compiler (Task 039).
