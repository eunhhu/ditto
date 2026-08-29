# Context IR

Context IR is a typed temporal graph compiled from events and memory projections. It is an intermediate representation, not the model prompt and not the sole durable source of truth.

## Node types

```text
Goal Constraint Entity Resource Claim Preference Decision Assumption
OpenQuestion Action Evidence Risk Capability
```

## Required metadata

Every node records:

```text
origin: user | model | capability | policy | system
epistemic: asserted | inferred | verified | disputed
scope: turn | session | task | project | device | global
confidence
source_event_ids[]
valid_from
valid_until
supersedes[]
token_cost
pinned
force_include
```

User assertions and model inferences are never conflated. Disputed or expired nodes are not injected. Pinned constraints, active leases, contradictions, blockers, destructive-action risks, and completion evidence may be hard-included regardless of semantic score.

## Task Signature

A retrieval query is built from:

```text
normalized request
+ active goal
+ explicit entities and resources
+ unresolved constraints
+ expected effect class
```

One optional local query embedding is shared by memory and capability retrieval. Exact references and aliases may skip embedding entirely.

## Context Capsule

The compiler emits a compact model-facing capsule and a user-facing receipt. The default capsule budget is 900 estimated tokens, excluding a stable system prefix and provider-native tool schemas.

The receipt explains node source, epistemic status, inclusion reason, score, token cost, and supersession. Clients may pin, dispute, delete, or rescope nodes by emitting new events; they do not mutate history.
