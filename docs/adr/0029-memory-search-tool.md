# ADR 0029: Read-only memory search for agent runs

Status: accepted on 2026-09-30 for Task 029, as part of
[ADR 0028](0028-thin-realtime-harness.md) Phase E. Turn payload version 8.
Amended by [ADR 0031](0031-model-managed-memory.md): from version 10 the search
also reads what Ditto inferred, marked as such. Amended by [ADR 0036](0036-two-agent-tools-one-lifecycle.md): the search is the
`search` action of `memory.manage`.

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
- **Cost.** A turn keeps its compilation and a shared reference to the
  session snapshot it chose from; the searchable memories are gathered only
  at the first search, and replay rebuilds them only for a turn that
  searched, in one pass over the snapshot.

## Consequences

A paraphrased question can reach a memory the budget left out when the model
searches with its own words; the model decides when to search. Recall remains
lexical: a search for "pet" still misses "My dog is called Miso" unless the
model also searches "dog". The model cannot save or change memories.

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
