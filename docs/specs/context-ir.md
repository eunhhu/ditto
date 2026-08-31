# Context IR

Context IR is a typed temporal graph compiled from events and memory projections.
It is an intermediate representation, not the model prompt and not the durable
source of truth. The IR vocabulary includes turn, session, task, project,
device, and global scopes; durable context version 1 currently admits only
session- and task-scoped records.

## Node types

```text
Goal Constraint Entity Resource Claim Preference Decision Assumption
OpenQuestion Action Evidence Risk Capability
```

## Durable node metadata

An IR node can carry:

```text
origin: user | model | capability | policy | system
epistemic: asserted | inferred | verified | disputed
scope: turn | session | task | project | device | global
lens: personal | task | environment | conversation
confidence
source_event_ids[]
valid_from
valid_until
supersedes[]
```

Context nodes do not store `pinned`, `force_include`, or caller-supplied token
cost. A model-origin node cannot be `asserted`. An IR node outside turn scope
requires event provenance. Disputed or expired nodes are not injected.

The durable version-1 `context.node.recorded` event is system-authored and is
accepted only through the trusted kernel admission boundary. The append-only
event spine is the sole durable authority; its separate SQLite projection is a
deletable, rebuildable cache with a sequence/event-ID checkpoint. Session and
task scope, provenance, validity, and supersession are rechecked from canonical
events before a context snapshot is exposed. Node identity is session-wide
`(session_id, node_id)`, supersession is exact-scope, origin claims require
matching actor evidence, and envelope causation is derived from the cited source
with the greatest durable sequence. Turn, project, device, and global nodes
remain valid IR vocabulary but are rejected by this durable slice.

## Trusted compiler directives

Pinning and policy-required inclusion are ephemeral directives produced by
trusted projections:

```text
ranked
user-pinned
policy-required(reason)
```

They are deliberately not deserializable from model output. Token cost is
calculated locally from content. Invalid ranked context is excluded with a
receipt reason; invalid required context blocks compilation. Required context
may exceed the soft budget but never the absolute compiler ceiling.

## Task Signature

A retrieval query is built from:

```text
normalized request
+ active goal
+ explicit entities and resources
+ unresolved constraints
+ expected effect profile
```

One optional local query embedding is shared by memory and capability retrieval.
When a provider is configured, every active, positively eligible candidate is
embedded exactly once, including exact context matches; exactness still controls
eligibility and its ranking position. Provider absence is explicitly
`lexical_only`.

The V2 query is owned by the retrieval layer and is built once for a joint
working set. Context and capability retrieval consume that same validated query
and its descriptor/vector; neither query, embedding, result, nor context
mutation is durable.

V2 context summaries accept at most 65,000 bytes. The exact embedding document
is `id=...\nkind=...\nsummary=...` and is bounded at 65,287 bytes. Scope-selected
rows count toward the 10,000-candidate ceiling before inactive, supersession, or
lexical filters; candidate 10,001 is a typed error. Result limits accept 1
through 256 and are rejected, never clamped, outside that range. The historical
five-field signature/compiler path remains separate. Its explicit fallible V2
adapter supplies `resources = []` and applies V2 bounds rather than silently
changing legacy behavior.

## Context Capsule and receipt

The compiler emits a compact model-facing capsule and a user-facing receipt. The
default soft budget is 900 estimated tokens and the default absolute ceiling is
1,800, excluding the stable system prefix and provider-native tool schemas.
Capsule items retain only the fields the model boundary needs: identity, kind,
summary, origin, epistemic status, scope, confidence, source-event provenance,
and validity bounds. Durable lens and supersession metadata and the compiler
receipt are not serialized into the capsule. Selection charges the exact
serialized item length through the local token estimator plus fixed envelope
overhead, so provenance or other retained metadata cannot bypass the ceiling.
Deserialized capsules are revalidated at the model request boundary before use.
The compiler also revalidates a serialized compiled result and receipt for
internal consistency against the original signature and exact capsule: IDs are
unique, scores, reasons, and ordering are canonical, and token accounting
satisfies both soft and absolute budgets. Candidate selection itself remains a
trusted live compiler operation; replay does not claim to reconstruct candidates
that were never persisted.

The receipt explains source events, epistemic status, inclusion directive,
score, derived token cost, and exclusion reason. Pinning and policy-required
inclusion remain trusted ephemeral directives. Durable version-1 node admission
has no public client context-mutation route, arbitrary event-append path,
serialized trusted draft, or daemon/CLI command; trusted kernel code assigns the
event authority and clients never mutate history or compiler authority directly.

When a turn persists compiled context, it captures a provenance high-water
sequence. The kernel resolves every included source within the same trusted
session/task snapshot and rechecks validity at model-request admission; later
events cannot retroactively authorize an earlier request.
