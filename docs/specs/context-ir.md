# Context IR

Context IR is a typed temporal graph compiled from events and memory projections.
It is an intermediate representation, not the model prompt and not the durable
source of truth.

## Node types

```text
Goal Constraint Entity Resource Claim Preference Decision Assumption
OpenQuestion Action Evidence Risk Capability
```

## Durable node metadata

Every node records:

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

Durable nodes do not store `pinned`, `force_include`, or caller-supplied token
cost. A model-origin node cannot be `asserted`. Non-turn nodes require event
provenance. Disputed or expired nodes are not injected.

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
Exact references and aliases may skip embedding entirely.

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

The receipt explains source events, epistemic status, inclusion directive,
score, derived token cost, and exclusion reason. Clients pin, dispute, delete,
or rescope nodes by emitting commands that become events; they never mutate
history or set compiler authority directly.
