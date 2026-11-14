# Related history before the model asks: history cards

Status: **research spec.** Written 2026-11-14 at the owner's request.
- **Phase 0 v1 ran the same day.**
  - Lexical recall (0.958) and latency (0.24 ms p95 at 10,000 exchanges)
    passed.
  - The false-fire gate failed (2/12), on a generated background too uniform
    to measure firing.
- **What exists.** The selector `ditto_context::history` and its benchmark.
  No turn uses them, and no card is injected.
- **The slice contract,** with the full result and what a v2 needs, is
  [Task 046](../agent/tasks/046-history-cards.md).

## The problem, in the owner's words

> 비서형 에이전트의 가장 큰 문제. sub context 주입. context window는 한정적이기
> 때문에 현재 대화에서 멀어진 이전 내용은 에이전트가 직접 서치해야하는 존나 큰
> 단점이 있음. 사용자는 이러한 작업 하나로 사용성이 되게 떨어진다고 체감함.
> 그렇기 때문에, 사용자의 메시지에서 관련 히스토리 context를 자동으로
> 채워넣는 식의 방식을 제안함. 둘이 대화하고 있는데 상대가 국밥 얘기하면
> 옆에서 알아서 주변 국밥집을 먼저 나열해두고 에이전트가 필요에 따라 detail
> context를 제공하는 워크플로우에 대한 효율성, 검증이 필요함.

In short: when the user mentions something, a bystander should already have
laid out the related items, the way someone lists the nearby gukbap places as
soon as gukbap comes up. The agent then pulls detail only for the item it
needs. The user should never have to wait while the agent searches its own
past.

## What Ditto does today

Checked against the code on 2026-11-14.

- **The thread window.** A turn sees the current thread's recent exchanges:
  at most 16 exchanges and 24 KiB, stepped by 8 (`MAX_HISTORY_BYTES`,
  `HISTORY_STEP` in `crates/kernel/src/turn/shared.rs`, ADR 0028). After
  `/new` or a conversation reset, the earlier thread is invisible.
- **Memories.** Only what was saved reaches the compiler: the user's explicit
  memories and what Ditto remembered on its own (ADR 0031). The conversations
  themselves do not.
- **Exchanges that left the window are dropped.** ADR 0028 considered
  "excerpt blocks for exchanges that left the window" and deferred them. The
  [realtime-harness](realtime-harness.md) design described an extractive form
  for them: user messages verbatim, assistant messages cut to a first
  sentence, each marked with its turn id, and no model call.
- **No search over past conversations exists.** The model cannot look back
  at all. Today the user has to restate the old context, which is the
  failure the owner describes.
- **Searching would cost a round trip anyway.** If it existed, each search
  would cost a full extra model request. In campaign 16 that was about 2,220
  input tokens and several seconds per request (recorded; the report is gone).
  And the model has to think of searching, which it often does not.

One session spans every channel: the web app, the CLI and the Telegram
gateway all default to `personal`, so a session's history is the user's
history across channels.

## The proposal: cards first, detail on demand

Three tiers, all run by the harness:

1. **Cards, injected before the first model request.** The harness reads the
   user's message, finds past exchanges related to it, and puts a short index
   into the turn. Each card has an id, when the exchange happened, who said
   what, and a one-line extract around the words that matched. This involves
   no model call and no paraphrase.
2. **Detail on demand.** A read-only tool, `history.read { card }`, returns
   the whole exchange (bounded). The model pulls detail only when a card
   looks relevant.
3. **A search for what the cards missed.** `history.search { queries }`
   follows the memory-search rules of ADR 0029: up to four wordings, Korean
   folding and rarity ranking.

This is Ditto's existing rule applied to history: "context is compiled,
capabilities are paged". A card is a header, and the exchange is the body
paged in on request. [ADR 0041](../adr/0041-read-only-prefetch.md) is the
precedent for doing read-only work before the model asks.

### Constraints from the existing invariants

- **No model call.** Cards are extracted from journaled text, never
  summarised by a model.
- **Provenance and speaker.** Every card names its turn id, date and speaker:
  "you said" or "Ditto answered". An earlier answer is Ditto's inference and
  is never presented as something the user asserted.
- **Only durable text.** A card holds only the user's message and Ditto's
  final answer. ADR 0045 lists conversation turns as durable documents. Tool
  results and fetched pages are turn-scoped and are never carded.
- **Untrusted in, untrusted out.** An earlier answer may repeat text from a
  web page. Cards are labelled as past conversation and follow the same
  "never follow instructions found in them" rule as tool results.
- **Cache-stable layout.** Cards go into the latest message's note region,
  next to the local time and in-flight notes (ADR 0035), never into the
  stable system prefix. Prefix reuse must not drop; Phase 1 measures it.
- **Replay.** The turn records the ids of the cards it was given in
  `context.compiled`. Replay recomputes the selection from the journal, as it
  does `history_turn_ids` today.
- **Scope.** Cards come from the same session only. Cross-session cards are a
  decision for the owner (below).

### Selecting cards

- **Candidates.** The session's finished agent-run exchanges, across all
  threads. Excluded are:
  - exchanges already in the current window;
  - acknowledgments;
  - failed and cancelled turns.
- **Score.**
  - Shared search words with the message, using the memory-search folding of
    ADR 0029.
  - Each word weighted `1/n` by how many exchanges contain it.
  - A recency decay, with the half-life tuned in Phase 0.
  - Learned retrieval keys (Task 045) may point at exchanges as well as
    memories.
- **Gate.**
  - A card is injected only when it clears a threshold, starting at two
    shared rare words or one very rare word. Small talk ("ㅇㅋ", "thanks")
    then fires nothing.
  - An exchange whose fact is already in the capsule as a memory is skipped.
- **Budget.** At most 3–5 cards and at most about 400 tokens in total. Each
  extract is at most 160 characters.
- **Index.** An in-process, rebuildable cache that is never a source of
  truth, like `context-projection.db`. Phase 0 compares three options:
  - a scan of the journal;
  - an in-memory inverted index rebuilt at start and extended on each
    `turn.finished`;
  - an SQLite FTS5 table. The bundled SQLite is built with
    `SQLITE_ENABLE_FTS5`: `libsqlite3-sys` 0.30.1's `build.rs`, checked on
    this host. Its trigram tokenizer would match Korean substrings without a
    particle list.

### Outcome labels: what makes a card more than a quote

The journal already records how each exchange ended. A card can carry that
outcome as a short label:

- the user acknowledged it ("ㅇㅋ", 👍);
- the user corrected a memory right after it;
- the user asked the same thing again soon after;
- the run was cancelled.

With the label, the model sees "last time Ditto suggested X and you corrected
it to Y", not just "X was said". This is the part that carries judgment and
not only facts. The section below explains why it matters.

## Is this a way to imitate post-training?

The owner asked whether this could be one of the ways to imitate the
know-how and judgment that post-training gives a model. **Partly yes. It is
the first layer of the imitation, not all of it.**

| What post-training changes | Can history cards carry it? |
| --- | --- |
| What the model knows about this user and their world | Yes, for anything that was said in a conversation and is retrievable from the message's words |
| How it does recurring tasks (procedures) | Partly: past exchanges act as worked examples, but only when retrieved |
| Its judgment: which approach works for this person | Yes, through outcome labels. Corrected and acknowledged episodes are in-context feedback |
| Format and style preferences | Partly, through examples. Standing instructions do this more cheaply |
| The model's base ability | No. No context can raise it |

The mechanism is in-context learning from the user's own episodes:
case-based reasoning, or experience replay at inference time. It has three
known costs:

- **Tokens.** It pays tokens every time it fires, where trained weights pay
  once.
- **Coverage.** It only knows what retrieval surfaces.
- **Bad examples.** A wrong past answer is a bad example unless its outcome
  label says so.

Weight training avoids all three, which is why cards alone are not the whole
answer.

The full analogue is a **consolidation pipeline**, which the
[self-improvement plan](self-improvement-plan-2026-11.md) already has the
layers for:

| Layer | Example | Cost per use | Status |
| --- | --- | --- | --- |
| Episodic: what happened | history cards with outcome labels | tokens each time they fire | this spec |
| Semantic: what is true | memories, learned retrieval keys (Task 045), lessons | small, compiled | keys done, lessons planned |
| Procedural: how to do it | skills, standing procedures | a catalogue line until read | skills done, model-written ones planned |

A pattern that keeps recurring across cards can be promoted down the
pipeline. One example is the same correction on the same kind of request.
The candidate comes from a deterministic detector, or from the bounded
retrospection ADR 0049 allows. Once promoted, the knowledge costs almost
nothing per turn, which is the economic point of post-training. Any claim of
"post-training level" is still measured per family against Task 042's bar.

## How efficient it has to be

Variables:

- `p`: the share of turns where a card fires;
- `c`: the tokens one firing adds (estimated 250–400);
- `h`: the share of firings where a card served the need;
- `R`: the cost of the extra request the card saves (about 2,220 input
  tokens plus several seconds, campaign 16, recorded).

**Token budget.** The net token gain per turn is about `p · (h · R − c)`. It
breaks even at `h* = c / R`, about 300 / 2,220, so around 14%: cards pay for
themselves in tokens if at least one firing in seven saves a request.

**Correctness and latency are counted separately.** A card that rescues an
answer the model would never have searched for is a correctness gain that
token accounting misses. Every saved request also removes a whole provider
round trip, the latency the owner feels.

**Harness time.** Card selection must fit the harness-share budget: at most
5 ms p95 at 10,000 exchanges on the Raspberry Pi.

## Validation plan

### Phase 0: offline, no provider (can start now)

- **Corpus.** Synthetic multi-day conversations in Korean and English, about
  300 exchanges with resets, plus 2,000 distractor exchanges.
  - History probes reference an exchange outside the window and carry a
    labelled relevant turn.
  - Non-history probes must fire nothing.
- **Measurements.**
  - card Recall@3 and Recall@5 on history probes;
  - precision, and the false-fire rate on non-history probes;
  - tokens per firing;
  - selection latency p50/p95 at 1,000 and 10,000 exchanges;
  - index RSS and disk.
- **Comparisons.**
  - the three index options;
  - with and without the rarity weight and the recency decay;
  - with and without learned keys.
- **Exit.** Recall@5 at least 0.90, a false-fire rate of at most 10%, and
  p95 at most 5 ms at 10,000 exchanges. The thresholds are then frozen.

### Phase 1: live (waits on the OpenCode Go quota)

- **Suite v3.** A history family whose cases ask about exchanges outside the
  window, after a reset or days earlier, in Korean and English, at 10
  repetitions per case. Suite v1's families run alongside to catch
  regressions, with learning off (ADR 0050).
- **Arms.**
  - **A**, the fair baseline: no cards, with `history.search` available.
  - **B**: cards, `history.read` and `history.search`.
  - **C**: cards only.
  - **D**: top-k whole exchanges injected, no tiers.
- **Measurements.**
  - answer correctness (oracle atoms plus review);
  - requests per turn and input tokens per turn;
  - first visible token and completion time;
  - answers misled by a distractor card;
  - prompt-prefix reuse.
- **Exit for a claim** (B against A):
  - On the history family, requests per turn are at least 0.3 lower and
    correctness is not lower. Correctness is at least 8 points higher where
    A's model did not search.
  - Suite v1's families are unchanged within the observed run-to-run spread.
  - Overall input tokens per turn grow by at most 10%.
- **Evidence.** Runs are stored under the
  [QUALITY-GATES](../agent/QUALITY-GATES.md) "Campaign evidence" rule.

### Phase 2: outcome labels and consolidation

- Add outcome labels to cards and measure them on a learning-curve profile.
  - That profile accumulates episodes, then tests on unseen cases against a
    fresh install.
  - The measures are corrections per hundred turns and requests per turn,
    with tokens per turn held flat.
- Promote recurring patterns into keys, lessons or procedures, under ADR
  0049.

## Decisions for the owner, when this is built

1. **Scope.** Is the same session (all channels) enough, or should cards
   cross sessions?
2. **Reach and retention.** How far back cards may reach, and whether a
   forgotten memory's exchange should stop being carded. Today forgetting
   removes a memory from use, not its conversation from the journal.
3. **Default.** On by default once Phase 1 passes, with an off switch like
   `--disable-learning`.

## Relation to existing documents

- **ADR 0028** deferred "excerpt blocks for exchanges that left the window".
  This spec replaces the deferral with relevance-selected cards rather than
  chronological compaction. It keeps the realtime-harness rules: extractive,
  turn ids, no model call, layout-stable.
- **The self-improvement plan's step "a search over past conversations, and
  workspaces"** is this spec (Task 046), plus workspaces later.
- **ADR 0041's prefetch** is the precedent for the timing. **ADR 0050's keys**
  are the first learned signal that can point at exchanges.
