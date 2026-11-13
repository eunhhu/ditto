# ADR 0029: Read-only memory search for agent runs

Status: accepted on 2026-09-30 for Task 029, as part of
[ADR 0028](0028-thin-realtime-harness.md) Phase E. Turn payload version 8.
Amended by [ADR 0031](0031-model-managed-memory.md): from version 10 the search
also reads what Ditto inferred, marked as such. Amended by [ADR 0036](0036-two-agent-tools-one-lifecycle.md): the search is the
`search` action of `memory.manage`. Amended for Task 043 (several wordings in
one search) and, on 2026-11-13, for Task 038 (Korean particles, single Hangul
syllables and rarity ranking; turn contract 20).

## Context

Runs compile the whole current memory set only while it fits the context
budget (ADR 0021): about twelve short memories. Past that, selection falls
back to lexical overlap with the question, so a paraphrased question ("What is
my pet's name?") receives no memory about the dog. A daily-driver assistant
accumulates far more than twelve memories. Semantic retrieval is deferred, and
raising the budget is a cost decision for the user. ADR 0021 left a
model-facing memory tool out as larger than its fix; this ADR adds it.

ADR 0015 keeps memory writes on the trusted local-user ingress, not model tool
invocation, and model-invoked saving (Task 024) awaits the user's decision.
Reading the session's own memories raises no such question: the model already
receives them whenever they fit.

## Decision

Agent runs are offered a builtin `memory.search` tool:

- **Contract.** Input `{ "query": string }` of 1 to 200 characters, closed
  schema. The result lists at most 8 memories, `{ id, text }`, with at most
  8 KiB of text, plus `searched`, the number of memories searched. The model
  reads the result labeled as what the user asked Ditto to remember.
- **Search space.** The memories the turn's context compilation saw: the
  nodes it included and those it left out as irrelevant or over budget.
  Invalid, disputed, expired, superseded and other-session nodes are never
  searched. The space is fixed at compilation, so a search is consistent with
  the turn's recorded context. Only the user's own assertions (origin `user`,
  status `asserted`) are searched and counted: the model reads the result as
  the user's words, so an inferred or derived node never appears in it, even
  though the capsule carries such nodes with their status.
- **Ranking.** `ditto_context::lexical_recall`: the complete-set tokenizer
  (function words never count) and the compiler's relevance score, best first,
  then by ID. The context crate owns the one tokenizer; no new one is added.
- **Authority.** A package `capabilities/core/memory-search` with a pinned
  header, a deriver with a local content-read effect and no resource, and a
  no-approval static policy (`StaticPolicy::memory_search`). An invalid query
  returns `invalid_arguments` to the model.
- **Evidence.** `agent.memory.requested` (model) records the arguments and the
  normalized query; `agent.memory.output` (capability) records the result.
  Replay rebuilds the search space from the recorded compilation, taking the
  left-out nodes from their own `context.node.recorded` events at or before
  the provenance cutoff, and recomputes the result exactly.
- **Surface.** From turn payload version 8 the tool is part of every agent
  run's stable tool surface while its package is installed; a missing or
  altered package withdraws it. Legacy artifact turns never see it.
- **Pointer closure.** A memory that places an object is not relevant to a
  request that names the object, so the complete set drops it and the model
  answers half of a two-hop fact. The compiler now recompiles once with up to
  two such nodes marked `policy-required: memory pointer`, chosen because they
  share a word that appears in at most two candidate summaries with an included
  memory. It is a second in-process pass: no model call, no capability page, and
  the capsule still validates because a policy-required node is an existing
  receipt reason with a recomputed score.
- **Budget.** A turn makes at most three searches: the fact, a pointer that
  fact named, and one more wording when a query matched nothing. A fourth and
  later search returns
  `limit_reached` without searching, and the tool stays on the surface so the
  prompt prefix is unchanged. The budget exists because a turn that keeps
  rephrasing a query it already asked spends its whole eight-request budget
  without answering: in the Task 017 suite one `long_use` turn spent seven
  requests on `reading group`, `book club`, `Birch Room`, `get-together venue`,
  `party venue pub restaurant`, `reading` and `gathering place meeting`, then
  failed `bound_exceeded`. Prose that asks the model to stop was measured
  against this and lost: a plain stop rule cut the rephrasing on that case but
  also stopped the pointer hop (recall 17/20 to 15/20), and a pointer-aware
  version raised requests from 5.2 to 6.2. The budget is enforced by the
  harness, not requested in the prompt.
- **Cost.** A turn keeps its compilation and a shared reference to the
  session snapshot it chose from; the searchable memories are gathered only
  at the first search, and replay rebuilds them only for a turn that
  searched, in one pass over the snapshot.

## A search carries several wordings (Task 043)

Recall is lexical, so a request that shares no word with the memory it needs
(`reading group get-together` against `our book club now gathers in Birch
Room`) can only be bridged by the model's own synonyms. Measured on the suite,
letting the model phrase them one search at a time cost 5 requests on that
case and 4 on the abstain case, and forbidding the second search dropped
long_use recall to 0.800 while saving nothing (the first search still failed).

So one search call takes `queries`: up to four wordings for the same fact, sent
together, unioned by memory and ordered by the best query. The model keeps the
synonym work it is good at and spends one round trip instead of five. The
singular `query` still parses, so journals written before the plural form
replay unchanged; `MAX_MEMORY_SEARCHES = 3` stays as the backstop for a turn
that searches again anyway.

Measured on P1, P2, P5, X1 and T1 (45 attempts): requests per attempt 1.64 to
1.69 with recall 0.900 to 0.967 - P2/long_use 5 to 2, P5 4 to 2 - because the
first search now reaches the fact.

## Korean words and rare words (Task 038, turn contract 20)

The search split words only at non-alphanumeric characters and dropped every
one-character token, so a Korean particle made a different word ("냉장고에"
never matched "냉장고") and a one-syllable noun (집, 차, 키) was not a word at
all. Every match also counted the same, so a memory sharing a word every
memory uses tied with the memory sharing the one rare noun, and the tie went
to the ID.

From turn contract 20 the search, and only the search, matches differently:

- **Search words** are the complete-set words plus single Hangul syllables,
  matched exactly. English single letters are still not words.
- **Particle folding.** A word ending in a Hangul syllable also matches as the
  word without the longest particle from a closed list of 25
  (`ditto_context::KOREAN_PARTICLES`: 은/는/이/가/을/를/에/에서/에게/한테/의/도/
  만/와/과/로/으로/부터/까지/처럼/보다/라고/이라는/입니다/해요), when at least two
  characters remain. Two inflections meet at the stem, and a word that itself
  ends in a listed syllable (포도, 고양이) still matches its own inflections.
  A one-syllable remainder is not folded ("집에" stays itself), so "키는" does
  not find "키": that gap is accepted until measurement says otherwise.
- **Rarity.** A shared word contributes `1 / n` when `n` of the searched
  memories contain it, summed in word order so the live search and its replay
  add the same numbers; ties keep the ID order.

The context compiler, its complete-set selection and the pointer closure keep
exact words: their selection is part of the recorded contract, and every
existing compiler test passed unchanged. Replay recomputes the new search,
which is why the change replaces the turn contract (ADR 0034) rather than
living beside it. The set of English memories that match is unchanged, but
their order can change where a rarer word is shared. Since a search returns
at most eight memories (and 8 KiB), which eight are returned can change too. No suite run has measured this yet:
every campaign number recorded so far is from contract 19.

## Consequences

A paraphrased question can reach a memory the budget left out when the model
searches with its own words; the model decides when to search. Recall remains
lexical (with the folding above): a search for "pet" still misses "My dog is called Miso" unless the
model also searches "dog", and a search that carries no matching wording still
returns nothing. The model cannot save or change memories.

Offering the tool costs every agent-run request 333 bytes of tool definition
(kept by a prompt cache, since the surface is stable) and every agent turn
1.6 KB of journal, because `capabilities.selected` records each offered
builtin manifest in full, as it does for the others. Turn start is unchanged
(Task 029 measurements).

## Alternatives

- **Semantic retrieval.** Deferred: it needs an embedding provider and cache.
- **Searching the live projection at call time.** The result could then
  disagree with the turn's recorded context, and replay would need the
  projection's active-set rules; the compilation's candidates avoid both.
- **Letting the model read every memory.** Unbounded context; the budget
  exists for cost and latency.
