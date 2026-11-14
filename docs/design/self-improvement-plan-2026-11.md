# Glass-box self-improvement: where Ditto stands and what to build

Written 2026-11-14 at the owner's request ("개선법 문서로 기록하고 commit push해둬"),
from an analysis done on 2026-11-13 against main `9b4944b` (turn contract 20).

**This is a plan, not an implementation.** Nothing in "What to build" exists in
code. [NEXT](../agent/NEXT.md) owns the order.

On 2026-11-14 the owner answered the five decisions listed at the end. They are
recorded in [ADR 0049](../adr/0049-learned-behavior-decisions.md), and the
order below is the one NEXT now follows. Decision 4 moves a credential-free
read-only connector up to right after step 3.

Steps 1–3 landed together on 2026-11-14 as
[Task 045](../agent/tasks/045-learned-retrieval-keys.md)
([ADR 0050](../adr/0050-learned-retrieval-keys.md)).
- It has detectors over existing events, a learned layer with one kind (the
  retrieval key), and the compiler's use of active keys.
- A ledger with undo makes it inspectable.
- No learning gain has been measured yet.

## The north star

The owner restated on 2026-11-13 what Ditto is for:

> ditto는 인간의 사용성과 체감성능, 알잘딱을 세계 최고로 사용하기 위한 비서형,
> 24/7, 에이전트 하네스야. 가장 크게 기술적으로 성과를 띄워야할 건,
> 자가학습/자가개선이야. post training을 따로 시킨급으로 맥락과 연혁 참조와
> context switching, injecting, forgeting등이 효율적이고 성능집약적으로
> 작동해야함. 그걸 블랙박스 없이 전부 인간이 조회/제어가 가능해야하고.

In this document's terms:

- **Product.** A secretary-type agent harness that is always on, and best in
  the world on usability, perceived speed and doing the right thing unasked.
- **Headline technical result.** Self-learning and self-improvement.
- **Quality bar.** History reference, context switching, injection and
  forgetting should work as if the model had been post-trained, at low cost.
- **Hard constraint.** No black box: a person can inspect and control all of
  it.

[Product intent](../product.md) already lists sustainable self-improvement and
inspectable memory among its outcomes. The new part is the ranking:
self-improvement is the headline, and inspectability is a requirement.

## Summary

Ditto has a base for inspection that this review found in no other system
compared here: an append-only journal, offline replay of every turn, and
context receipts. For three of those systems no inspection mechanism was found
in public sources at all, so that comparison is limited to what the sources
show. Ditto has none of the learning. Three things block a learning loop:

- **No fuel.**
  - The host this was checked on has no Ditto data directory at the default
    locations (`.ditto` in the repository or the home directory) and no running
    daemon, so no real-use journal was found to learn from.
  - The evaluation suite is 17 synthetic cases.
  - The journal already records several reactions (listed under step 1), but
    nothing reads them as signals, and a user cannot mark an answer as wrong.
- **Nowhere to write a lesson.**
  - The model's core instructions are six Rust constants: 3,049 characters in
    `INSTRUCTIONS`, `crates/kernel/src/turn/shared.rs`. Changing one means a
    rebuild and a new turn contract.
  - The only instruction-like texts that change at run time are the standing
    instructions and the skills, and the user writes both. Memories change
    too, and the model writes them, but they hold facts, not ways of working.
- **Little procedure to learn.**
  - [Task 042](../agent/tasks/042-harness-only-self-improvement.md) cites
    Harness²: its gains came from procedure, such as tool order and when to
    stop.
  - Ditto's model tools are web reading, memory, skill reading and the
    attachment read and sort. Task 042's first family is already recorded as
    saturated for wording edits.

So the order below starts with signals, then a place to write lessons, then one
learning loop that needs no model call.

## Where Ditto stands on each axis

Checked in code on 2026-11-13 unless a row says "recorded". Recorded numbers
come from campaign 16 and earlier measurements, whose raw reports can no longer
be inspected ([HANDOFF](../agent/HANDOFF.md), "Repository state").

| Axis | In code | Missing |
| --- | --- | --- |
| Self-improvement | `scripts/probe_variant.py`, an offline script that applies a variant to source files, runs cases and reverts. Two probes ran; both ended `KEEP_BASE`. | All run-time learning. Tasks 039 and 042 are specs. The `improvement.*` event kinds are constants in `crates/protocol` with no emitter. |
| History reference | The current thread's recent exchanges (at most 16, 24 KiB) and the session's memories. | Any search over past conversations. After a conversation reset, the earlier thread is invisible to the model unless something was saved as a memory. |
| Context switching | Profiles (separate data roots) and a manual thread reset. | Switching by topic or project. `ContextScope` has `project`, `device` and `global`, but the kernel writes only `session`. |
| Injection | Per-turn compilation; a byte-stable prefix (recorded median reuse 96.4%); the pointer closure; attachment and sort prefetch. | A match when the request and the memory use different words: the capsule misses and the turn pays one search (recorded: 1.40 requests per turn in the memory family). Tool output is not folded: a web page enters at up to 24,000 characters and a skill body at up to 64 KiB. A running turn cannot be steered. |
| Forgetting | Explicit forget, and supersession by a correction. | Expiry (nodes have `valid_until`, but no path sets it), usage accounting, any retirement policy, history and restore, and erasure from the journal. |
| Inspection and control | The journal; replay; receipts with inclusion and exclusion reasons; origin labels on memories. | See "The inspection gap" below. |
| Always-on secretary | Recorded: 1.172 requests per turn, 35 ms median harness overhead, 17 MB peak RSS. One-shot and finite repeating schedules. Telegram. | Tools that act (calendar, mail, files, shell). Event sources that wake a task. Durable long tasks. |

### The inspection gap

The web app's "Why?" view shows the memories a turn used with their reasons,
the thread history it replayed, the tool calls and the request count. It does
not show:

- which memories were excluded (it shows a count per reason);
- the standing instructions and skill catalogue in force for that turn;
- the request as sent (the journal holds its digest; replay can rebuild it);
- tokens, cost and latency.

The web app has no screen for instructions, skills or profiles; those are CLI
only.

## How the others learn

**None of these systems was run.** The tables compare mechanisms, not speed or
quality. Each row names how good its source is.
[Task 041](../agent/tasks/041-hermes-baseline.md) is the unbuilt measurement
against Hermes.

Hermes, OpenMausBot and Harness² are covered in more depth in
[comparison-2026-11](comparison-2026-11.md), which this section extends.

### Assistants

| System | Where it runs | How it learns | Inspection and control | Source |
| --- | --- | --- | --- | --- |
| OpenAI dots (2026-09-29) | A cloud VM and browser per assistant; 4,000+ app connections; works on goals between conversations. | Reported to learn from feedback; no mechanism found. | Significant actions need approval by default; a read-only mode. | News reports |
| Grok Bot (2026-08) | A computer per bot; scheduled and email-triggered automations; $120 a month. | Custom skills the user creates. Memory details not found. | Not found. | Third-party articles |
| Manus 2.0 | A cloud computer; event automations; scheduled tasks keep memory, files and project context. | No documented self-improvement found. | Not found. | Vendor blog, reviews |
| OpenMausBot | A local harness that runs Claude or Codex as persistent bots. | A skill review ported from Hermes; changes apply at once. | A memory journal with one-click undo and expiry dates. | comparison-2026-11 (code read 2026-11-06) |
| Hermes Agent | Local; about 50 tools. | A background review after every turn (a model call); skills it creates and edits; a session search over SQLite FTS5 with a CJK tokenizer. | Markdown files the user can edit. | comparison-2026-11, the local checkout, official docs |
| OpenClaw | Local; chat-app gateways. | A heartbeat every 30 minutes that can tidy memory; no structured improvement workflow. | Markdown files the user can edit. | Docs, third-party articles |
| Ditto | Local. | Nothing. | Journal, replay, receipts. | Code |

What follows from it:

- **The three cloud products** (dots, Grok Bot, Manus) get their reach from a
  computer and app connections, and their learning cannot be seen from
  outside. Ditto can lead on transparency. It cannot be used as a secretary
  without some of that reach.
- **Hermes, OpenMausBot and OpenClaw** let the model rewrite its own notes and
  skills. No evidence gate checks an edit, each review costs a model call, and
  the files grow. That is the "self-improvement that becomes less efficient
  over time" the owner named on 2026-09-06, so it is not copied.
- **Worth taking:** Hermes's search over past sessions, and OpenMausBot's undo
  journal.

### Work harnesses

| Harness | What is learned | How it is edited | Evidence and rollback | Source |
| --- | --- | --- | --- | --- |
| Prime Agent (Prime Intellect, arXiv 2608.23552) | Prompts, sub-agents, skills and memory, kept as state. The base system prompt is immutable. | `/refine` reads a trajectory and applies the smallest create, update or delete that helps. | Snapshots for rollback. ARC-AGI-3 from 30% to 95.5%, self-reported. | Abstract, README |
| Harness² (Google, arXiv 2610.14224) | Eight harness files. | Propose, probe, compose per task; lessons collect into a persistent improvement harness. | An edit without execution evidence lowered the score (73.4 to 63.2). Up to +11.5 and +11.8 points, self-reported. | Abstract, comparison-2026-11 |
| Claude Code | `CLAUDE.md` (the person's), auto memory (notes the model writes), skills, sub-agents with their own context. | The model writes memory files. | Files a person can edit. No validation step. | Third-party guides; observed in the session that wrote this |
| Codex | `AGENTS.md`, memories built from session summaries, skills, automations. | Background summarising and consolidation, per third-party accounts. | `memories` and `skills` directories seen under the local Codex home; internals not confirmed. | Official concepts page, local |
| Ditto | Nothing. | An offline script edits source files and reverts them. | Replay-based validation is designed only. | Code |

What follows from it:

- **From Prime Agent:** an immutable base, mutable state on top, the smallest
  edit, and snapshots. ADRs 0045 and 0046 already record in each turn the
  document text the turn was given, so Ditto can have the same shape without
  breaking replay.
- **From Harness²:** no edit without execution evidence, and `KEEP_BASE` as a
  normal outcome. Tasks 039 and 042 already say this.
- **Claude Code and Codex** learn at the level of notes and summaries, which
  leaves room to do better. Their sub-agents protect the main context; the
  [context feedback note](feedback-context-architecture.md) argues for the
  same.

## What to build, in dependency order

The promotion states, the learning layers and the immutable surface are the
ones [improvement-patches](../specs/improvement-patches.md) already defines.
This plan adds what feeds them and where their output lives.

### 1. Real use, and the signals it leaves

Run Ditto as the daily assistant. Most of the signals a learning loop needs are
then already in the journal, and reading them needs no new event and no model
call:

| Reaction | Already journaled as |
| --- | --- |
| "ok" and other acknowledgments | `input.received` whose run metadata has `acknowledged: true` (ADR 0035) |
| A cancelled or failed turn, with its code | `turn.failed` |
| A memory corrected or forgotten | a superseding `context.node.recorded`, or `memory.forgotten` |
| A new conversation | `conversation.reset` |
| A search after the capsule missed | `context.compiled` followed by a `memory.manage` search and its `tool.output` |
| A refused or limit-reached tool call | `tool.output` |

So the work is deterministic detectors over these events, each naming the turn
its evidence is about. Two signals are new:

- **Explicit negative feedback.** A thumbs-down, or a short closed list of
  phrases such as "that's wrong", recorded against the answer it follows. The
  closed-list approach is the one acknowledgments already use.
- **A re-ask.** The same question asked again soon after an answer. Detecting
  it is a comparison of two journaled inputs.

Without real use, the detectors of
[Task 039](../agent/tasks/039-improvement-compiler-first-slice.md) have nothing
to read. One of its four detector classes, a repeated approval, cannot fire at
all today, because approval fulfillment is still deferred.

No task file covers this yet.

### 2. A learned layer: where a lesson lives

Learned behavior becomes profile documents in the journal, recorded in each
turn that received them, exactly as standing instructions and the skill
catalogue are (ADRs 0045, 0046). The core instruction constants stay immutable.

| Kind | What it holds | Layer in improvement-patches |
| --- | --- | --- |
| Retrieval key | Extra words by which one memory is found | A retrieval patch, targeting a memory |
| Lesson | A failure pattern seen in this installation | Fragment, expiring |
| Procedure | A skill the model drafted | Recipe, versioned |
| Preference | How this person wants things done | Claim |

Every item carries the turns that are its evidence, a promotion state, an
expiry and a use count. Rollback is supersession by the previous version. The
layer gets a fixed size in the prompt, and what stays in is decided by measured
use, not by age.

This is the open half of [Task 044](../agent/tasks/044-platform-surface.md) P2
("a model write that lands on the next turn"), widened from instructions to
four kinds.

### 3. The first loop: retrieval keys from the model's own searches

When the capsule misses a memory and the model then finds it by searching, the
journal already holds the request's words, the wordings the model searched
with, and the memory that was found. A deterministic detector can read that
triple.

1. **Candidate.** The same request words lead to the same memory in two
   distinct turns (Task 039's admission rule).
2. **Validation.** Recompile the evidence turns' context with the key in place
   and check that the capsule now includes the memory. This is a recompilation
   from journaled events (the request text and the context nodes up to that
   turn's provenance cutoff), not a turn replay: replay rejects every turn
   recorded under an earlier contract (ADR 0034), and this change is itself a
   new contract.
3. **Promotion.** Three independent cases, or the owner's approval, as
   improvement-patches requires for a permanent promotion.
4. **Effect.** The compiler includes the memory on the first request, so the
   turn skips the search.

Why this one first:

- **No model call anywhere.** The model already wrote the wording during
  normal use.
- **The target already exists.** [Task 043](../agent/tasks/043-second-generation-bar.md)
  asks for Recall@5 of 0.95 with no search; the memory family stood at 1.40
  requests per turn in campaign 16.
- **It settles a disagreement between two documents.** comparison-2026-11 says
  not to add embeddings, and Task 043's outcomes say the next lever is a
  retrieval bridge. Learned lexical keys are a bridge without a vector
  service.

It changes how the compiler selects, so it needs an ADR and a new turn
contract. That bump is paid once. After it, a newly learned key changes no
contract, because each turn records the keys it was given, as it records its
standing instructions.

### 4. Past conversations, and workspaces

- **A history search.** A read-only tool over the journal's finished exchanges,
  lexical, reusing the Korean folding of ADR 0029. It returns short excerpts
  with their turn ids, and replay recomputes it. No summarising model call.
- **Workspaces.** Start writing the `project` scope: a memory or a thread can
  belong to a workspace, and a turn compiles the session's memories plus the
  active workspace's. The switch is a user command or a channel mapping; the
  model may suggest one as a hand-off.

No task file covers this yet.

### 5. Forgetting as a policy

- **Usage accounting.** Per memory: how often it was injected, and whether the
  turn used it.
- **Expiry and a cold tier.** A memory may carry `until`. A memory unused for
  long enough becomes cold: still searchable, no longer compiled.
- **History and restore.** The rest of
  [Task 038](../agent/tasks/038-korean-and-ranked-memory-search.md).
- **Erasure.** Today "forget" removes a memory from use and view, and its text
  stays in the append-only journal. Removing the text needs an ADR, because it
  changes what the journal promises (ADR 0002).

### 6. A glass-box console

- **Per turn:** the request as sent, rebuilt by replay; the instructions,
  skills and learned items in force; the excluded memories by name; tokens,
  cost and latency.
- **The learning ledger:** every candidate and promotion with its evidence
  turns, what changed, the measured effect, and an undo.
- **"What if":** re-run a recorded turn without one memory or one learned
  item. This is the counterfactual replay Task 042 designs for probes, offered
  to the user.
- **Screens** for instructions, skills and profiles, which are CLI only today.

No task file covers this yet.

### 7. Tools that act, and events that wake

- **Read-only connectors first** (calendar, mail, files) as capability packages
  under the lease model. They feed context without changing anything outside.
  They still need credentials, and likely a lease per source. The credential
  boundary is one of the changes that requires an ADR first.
- **Then effects,** with approval delivered as a hand-off in the answer.
- **Event sources.** A new mail or a calendar change wakes a task. The
  invariant forbids a periodic LLM heartbeat; it allows an event to wake work.
- **Durable long tasks.** [Task 040](../agent/tasks/040-durable-work-items.md).

This is also what gives the learning loop procedure to learn. It overlaps
Task 044 P4 (install route, effect review) and the "Later" list in NEXT.

### Measuring it

A gain is claimed only against Task 042's bar: held-out cases, at least 8
points or Recall@5 of 0.95, no gate regressing, and at most 5% more prompt
tokens.

The suite's `long_use` profile checks that long use does not make Ditto worse.
It does not check that use makes Ditto better. That needs a **learning-curve
profile**: let Ditto gain experience over a set of episodes, then test on cases
it has not seen, and compare with a fresh install. The measures are corrections
per hundred turns, requests per turn and tokens per turn (which should stay
flat).

As of this date no learning gain has been measured on Ditto, and contract 20
itself is unmeasured.

## Decisions for the owner

**Answered on 2026-11-14** ([ADR 0049](../adr/0049-learned-behavior-decisions.md)):

| # | Answer | As applied |
| --- | --- | --- |
| 1 | 알아서 | The proposal: a change that widens nothing and can be undone applies on its own. Rollback is manual for now, plus automatic retirement on a forgotten target or on expiry. |
| 2 | 그렇게 하자. | Claims are per family, against Task 042's bar. |
| 3 | 회고 방식에 많지 않은 비용이 든다면 허가할게. | Retrospection is allowed when a detector wakes it: at most 3 requests and 8,000 input tokens per run, 2 runs per profile per day. The ceiling is ADR 0049's number, not the owner's. |
| 4 | 추천대로 | No recommendation had been written, so ADR 0049 makes one: a credential-free read-only connector (granted local files) comes right after step 3, then the credential ADR and read-only calendar and mail. |
| 5 | 턴계약은 1.0때 고정. 그 전까진 better way가 있다면 무조건 그 방식으로 연구하고 적용도 해보고 하는 식 | One contract until 1.0, then frozen. Until then the better design wins, still ADR-first and still under Task 042's evidence bar. |

The questions as they were put:

1. **Approval or hand-off for a learned change.** Task 039 requires an approval
   event before any patch applies. The owner's standing preference is hand-off
   rather than an approval layer.
   - Proposal: a change that widens no effect and can be undone (a retrieval
     key, a lesson) applies on its own, appears in the ledger, and rolls back
     automatically if its measure falls.
   - A change that widens what Ditto may do still needs approval.
2. **What "post-training level" may claim.** Nothing shows that a harness alone
   equals weight training in general. The published gains are self-reported
   and tied to their benchmarks. The claim that can be supported is per
   family, against Task 042's bar.
3. **Two invariants that shape the loop.**
   - "No model call solely for housekeeping" rules out reflection after every
     turn. Learning becomes offline work with a budget, woken by a detector.
   - "No permanent improvement from one successful trajectory" means one
     correction can create an expiring candidate, not a lasting change.

   Both protect cost and guard against the slow decay the owner named. Keeping
   them is the proposal; it is the owner's call.
4. **How far forward to pull step 7.** Learning is the headline, and a narrow
   tool surface caps what there is to learn.
5. **When the turn contract freezes.** Until the first release there is one
   contract, and replay rejects turns recorded under an earlier one
   (ADR 0034).
   - Detectors read events, so earlier turns stay usable as evidence after a
     bump.
   - Replay-based checks do not survive a bump. That includes step 6's "what
     if" and Task 042's counterfactual replay: they work only on turns
     recorded under the current contract.

   Once real use is the fuel, each bump strands that history for replay.
   Freezing the contract, or keeping a bounded number of earlier ones
   replayable, is a decision this plan needs before step 6.

## Sources

Gathered 2026-11-13 by web search, except where a row above names the code or
comparison-2026-11.

- OpenAI dots: <https://techcrunch.com/2026/09/29/openai-launches-dots-its-bubbly-agentic-avatar/>,
  <https://www.axios.com/2026/09/30/openai-dots-ai-agent-safety>
- Grok Bot: <https://venturebeat.com/ai/spacexais-grok-bot-turns-agents-into-persistent-digital-coworkers-that-can-operate-your-apps-for-120-per-month/>
- Manus: <https://www.timesofai.com/news/scheduled-tasks-2-0/>
- Hermes Agent: <https://hermes-agent.nousresearch.com/docs>
- OpenClaw: <https://clawdocs.org/architecture/memory-system>
- OpenMausBot: <https://www.scriptbyai.com/openmausbot-grok-bot-alternative/>
- Prime Agent: <https://arxiv.org/abs/2608.23552>,
  <https://github.com/PrimeIntellect-ai/prime-agent>
- Harness²: <https://arxiv.org/abs/2610.14224>,
  <https://github.com/google-research/harness2>
- Claude Code: <https://code.claude.com/docs/en/context-window>
- Codex: <https://developers.openai.com/codex/concepts/customization>
