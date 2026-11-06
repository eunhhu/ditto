# Comparison: Hermes Agent, OpenMausBot, Harness², and Ditto

Written 2026-11-06 at the user's request ("hermes나 openmausbot이나 harness2와 비교해서
개선점 찾아보고 명세화해둬"). Earlier product intent already named Hermes as the
positioning reference and listed the problems to solve: memory footprint,
inefficient context management, unreliable memory and schedules, self-improvement
that degrades, task performance, latency ([product.md](../product.md)).

This document says what the three others do, what Ditto does in the same place,
and which gaps became [Task 038](../agent/tasks/038-korean-and-ranked-memory-search.md),
[039](../agent/tasks/039-improvement-compiler-first-slice.md),
[040](../agent/tasks/040-durable-work-items.md) and
[041](../agent/tasks/041-hermes-baseline.md).

## How this was gathered

- **Hermes Agent** — read the working checkout at `~/work/hermes-agent`
  (Python). A read of the code, not a measurement of it.
- **OpenMausBot** — cloned `milind-soni/OpenMausBot` v0.1.143 (Apache-2.0,
  TypeScript/Electron) and read its `docs/*.md` and `server/`.
- **Harness²** — arXiv 2610.14224 and `google-research/harness2`.
- **Ditto** — the numbers below are from calibration 3 (130 attempts,
  DeepSeek V4.1 Flash, release build, turn contract 15) recorded in
  [Task 037](../agent/tasks/037-eval-suite-runner.md).

None of the first three was executed or benchmarked here, so nothing below is a
measured claim about their speed or cost. Where a number appears for them it is
one they publish or one their code fixes (a constant, a limit, a file:line).

## The same mechanism, four ways

| Dimension | Hermes Agent | OpenMausBot | Harness² | Ditto |
| --- | --- | --- | --- | --- |
| Turn bound | No default bound: `max_turns: None`, `max_iterations=sys.maxsize` (`hermes_cli/config_defaults.py:78`, `agent/agent_init.py:2364`). Round caps exist (50 searches, 50 subagents) but not a wall-clock or call budget. | The engine's step cap plus activity/stall windows and per-root budgets (`docs/runtime-contract.md`); API driver loop is `MAX_CHAT_ROUNDS = 64` (`server/drivers/openai-chat.ts:136`). | 200 model turns and 3,600 s per rollout (`harness2/core`). | One turn contract: bounded model calls, tool calls and deadline (ADR 0034); a response's ready calls run in order, max four (ADR 0038). |
| Context assembly | System prompt built once per session in three cache tiers; messages replayed from stored `api_content`; compaction at 50% of the window by an auxiliary model, middle summarized. | The engine session holds the transcript; the harness sends only the delta; compaction is provider-native where possible, else a fold at ~80%. | No compiler: the harness is a tree of eight editable files; the model sees whatever the harness's prompt and skills say. | Compiled per turn from durable nodes: complete set when it fits the budget, otherwise lexical selection; thread window of 8 exchanges / 24 KiB; everything before the latest message is byte-identical for cache reuse (ADR 0028 Phase A). |
| Memory store | `MEMORY.md` / `USER.md`, 2,200 / 1,375 char budgets, curated entries plus raw session rows in SQLite. | `MEMORY.md`, first 200 lines or 24 KB injected every turn, topic files on demand, an archive, and a daily log. | `LongTermMEMORY.md` as one of the eight harness files. | Durable context nodes with origin, epistemic status, scope, provenance and supersession (ADR 0015, 0031, 0032); model inference is labeled as inference, never as a user assertion. |
| Memory search | SQLite FTS5 with bm25, a trigram index and a bundled CJK tokenizer; optional auxiliary-model query rewrite and optional external memory providers. | FTS5 bm25 over notes, query terms OR-ed, punctuation dropped; four notes and four passages before the user message. | Domain-scoped directory index; retrieval by construction, no embeddings. | Lexical overlap over function-word-free tokens, regular plurals folded (ADR 0038). No term-rarity ranking. Tokens split only on non-alphanumeric characters. |
| Tool disclosure | 50 selected tools → 36 schemas in context, ≈54 KB ≈13.2K tokens, plus a `tool_search`/`tool_describe`/`tool_call` bridge and a catalog block capped at 5% of context. | The whole surface: ~40 harness tools plus engine tools plus every mounted MCP server, statically filtered per bot and kept byte-identical for cache reuse. | Tools described in `tool_descriptions/tool_guidance.md`, part of the editable harness. | Capability headers with retrieval hints; only selected cards and schemas are paged into the epoch. An ordinary run has two agent tools plus the artifact tools it was granted. |
| Effects | Tool-level approval policy, sandboxes, and per-tool safety checks. | Engine approval modes; a restart heuristic reads activity rows to decide whether a turn acted outward. | None; research harness. | Orthogonal effect profile, sealed expiring lease bound to one invocation, one-shot execution claim (ADR 0018). |
| Recovery | Replays the transcript, strips dangling `assistant(tool_calls)`, marks the effect `UNKNOWN` and tells the model to inspect before retrying. | Durable `assignments` / `assignment_events` / `inbox` tree; refuses to rerun a turn that already acted outward. | Invalid tree → one retry, then inherit the starting tree. | Journal replay; a claimed attempt is never retried; status reports interrupted ownership instead of guessing. |
| Scheduling | In-process tick with an ownership lock; interrupted attempts become `unknown` only after the owner is proved dead. | Croner routines, pre-model code checks so a quiet tick costs nothing, 12 h catch-up, one retry; the app must be running. | None. | Owned scheduler future, journal-projected index, claim before admission, at-most-once, finite repeats (ADR 0019, 0020). |
| Learning | Background review after every turn (a model call, on by default) plus a skill curator; skills are markdown files. | Skill review ported from Hermes; self-changes apply immediately with an Undo line. | Recursive harness edits: propose → probe → compose, per task, then a domain library of harness trees and lessons. | Improvement compiler is designed (detectors, typed patches, dedup/validation/replay/shadow/canary) and not implemented. |
| Housekeeping model calls | Yes: background review, title generation, compaction, query rewrite. | Yes: memory capture, nightly tidy, skill review. | Yes: three editor sessions per task at `k=1`. | Forbidden by invariant: no model call solely for housekeeping. |
| Delegation | Subagents with fresh context and a summary back; 10 concurrent by default. | Durable assignment tree with `rootId`/`parentId`/`depth`, one wake per result, admission slots. | Subagents as one of the eight harness files. | None. Schedules admit turns; a long task either fits one turn or is not represented. |

## Where Ditto is already ahead

- **Authority is a lease, not a prompt.** None of the three binds an effect to a
  sealed, expiring, invocation-bound permit derived from a capability-declared
  profile. OpenMausBot's outward-action check and Hermes' `effect_disposition`
  are honest heuristics; Ditto's is a contract.
- **Capability paging is real, not aspirational.** 13.2K tokens of tool schemas
  (Hermes) or a 40-tool flat surface (OpenMausBot) against two agent tools and a
  selected card.
- **Memory has provenance.** The others store plain text; Ditto refuses to
  present model inference as a user assertion and scopes supersession.
- **Schedules survive.** Claim-before-admit and at-most-once from a journal beat
  a keep-awake hold and an in-process tick.
- **Measured.** Calibration 3: 128/130 verified turns, Recall@5 45/45 clean and
  43/45 with 1,000 distractor memories, $0.233 per 130 attempts at list price,
  p50 2.8 s for a memory question. The other three publish no comparable
  figure for the same workload.

## Gaps, in the order they matter

1. **The learning loop does not exist yet.** This is the user's stated fear
   ("self-improvement that becomes less efficient over time"), and it is the one
   place where another project has measured evidence: Harness² reports +11.5
   and +11.8 points from one recursive step with a frozen model, and — the part
   that matters for Ditto — editing *without* execution evidence drops the score
   (73.4 → 63.2). Hermes and OpenMausBot ship review loops, but both pay a model
   call per turn, which Ditto's invariants forbid. So the slice must be offline,
   evidence-gated and free at runtime. → Task 039.
2. **Memory retrieval is the weakest of the four for Korean and for ranking.**
   Hermes has a CJK tokenizer and bm25; OpenMausBot ORs terms through bm25;
   Ditto splits only on non-alphanumeric characters, so a Korean word with a
   particle never matches the bare word, and nothing ranks a rare matching word
   above a common one. The user writes Korean. → Task 038.
3. **A correction cannot be reviewed or undone.** OpenMausBot keeps a journal
   outside the workspace with one-click undo, expiry dates and hash-checked
   concurrent edits. Ditto supersedes and forgets, but the previous text is not
   listed and cannot be restored. Small surface, real user trust. → folded into
   Task 038 as its second half, because both touch the correction record.
4. **A long delegated task has no durable form.** OpenMausBot's assignment tree
   is the strongest engineering in the three: a restart policy that refuses to
   rerun a turn that already acted outward, read from activity rows rather than
   model text. Ditto's one-turn contract deliberately excludes this until the
   first release. → Task 040, after the release, ADR first.
5. **No measured Hermes baseline.** "Replace Hermes" is the goal and every
   Ditto number is absolute. Same suite, same model, same machine, blind grading
   is the only honest comparison — and the memory family is the part that is
   comparable without harness-specific tools. → Task 041.

## What not to copy

- **Per-turn review, memory capture, tidy, titles, query rewrite.** Every one is
  a model call for housekeeping, which Ditto forbids; the same jobs must run as
  deterministic detectors over journaled evidence, or as an explicitly approved
  offline task with its own budget.
- **Sending the whole tool surface, or 13K tokens of schemas.** The capability
  pager exists to avoid exactly this.
- **4,000 rollouts inside a user request, and auto-adoption of a revision.**
  Incompatible with one bounded turn and with the approval boundary; the
  journaled proposal is the Ditto form of the same idea.
- **Embeddings or a vector service.** All four systems on this page are lexical,
  and Ditto's misses so far were a prefix/particle problem and a two-hop reasoning
  problem, not a semantic-similarity problem. Adding a vector path would be a new
  mandatory service for no demonstrated gain.
- **A growing skills index in the prompt.** Hermes injects every skill name and
  description; Ditto's equivalent is capability headers, already bounded.
