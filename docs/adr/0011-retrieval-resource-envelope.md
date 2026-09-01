# ADR 0011: Retrieval resource envelope and verified projection lifecycle

## Status

Accepted.

## Context

ADR 0010 bounds each V2 candidate and caps a scan at 10,000, but those limits
compose into an unsafe request: a lexical working set can retain hundreds of
MiB of duplicate documents, and an injected provider can receive 20,001
synchronous calls. The source-verified snapshot path also replays one session
from sequence zero on every retrieval while holding the admission gate. These
behaviors are correct on small fixtures but make request cost grow with durable
history.

The same review found three authority-adjacent gaps: verified and derived cache
snapshots share one Rust type, working-set scope and runtime-search inputs reach
the provider before deterministic validation, and new context identities may
contain non-canonical whitespace/control/Unicode forms. Local SQLite privacy is
also left to the process umask, while Task 004 audit evidence is described in a
tracked handoff but not itself reproducible from the repository.

## Decision

### One fixed cumulative work envelope

Every joint V2 retrieval owns one stateful `RetrievalWorkBudget`, shared by
query construction, context projection/ranking, and capability ranking. Version
1 has these hard maxima:

```text
candidate bytes          64 MiB
constructed documents    64 MiB
lexical work bytes        64 MiB
provider calls               513
provider input bytes      32 MiB
```

All counters use checked addition and fail with a typed budget-dimension error
before the over-budget allocation, tokenization, or provider call. The query
embedding consumes one call and its canonical query bytes. Each document is
constructed, charged, scored/embedded, and dropped before the next candidate.
Context and capability ranking retain at most their requested top-K roots, not
all eligible documents. Provider failure or budget exhaustion still fails the
whole working set without fallback or a partial result.

The existing per-value and 10,000 active-candidate maxima remain. A request may
therefore fail before candidate 10,000 when its cumulative byte/work envelope
is exhausted; this is the intended bounded behavior.

### Lifecycle candidates, not immutable history

ADR 0010's rule that counted all scope-selected context history and every
installed manifest before lifecycle filtering is superseded.

- Context candidate count is taken after exact scope, supersession, disputed,
  not-yet-valid, and expiry filtering. Immutable historical nodes remain in the
  event spine and projection but do not consume the active retrieval namespace.
- Capability manifests gain a default-active lifecycle with `active`,
  `retired`, and `quarantined` states. Only active manifests consume V2 scan
  capacity or appear as roots/complements/cards.
- Hard runtime filters and positive lexical eligibility still precede embedding
  work and never become authority through similarity.

Projection schema version 2 stores the lifecycle fields needed to filter active
rows in SQLite before materialization. A schema-1 cache is discarded and
rebuilt; canonical events are unchanged.

### Full replay once, delta verification thereafter

The event spine remains the sole durable authority. A kernel open performs one
bounded-page rebuild of `context-projection.db` from canonical events and marks
that projection generation source-verified only in process memory. Normal
working-set reads then:

1. validate the remembered canonical checkpoint anchor and SQLite data version;
2. validate and apply only events after that checkpoint through the captured
   high-water;
3. stream the active requested scope from the verified generation; and
4. return a typed `VerifiedContextSnapshot`.

A missing in-process proof, anchor mismatch, external cache write, schema
change, SQLite failure, or explicit audit causes one full rebuild/recheck. Full
session replay is therefore an open/recovery/audit operation, not normal query
work. The compact in-memory proof is never serialized and cannot become a
second durable source of truth.

`DerivedContextSnapshot` is reserved for cache inspection and exposes no
candidate-consuming conversion. Only `VerifiedContextSnapshot` can enter the
kernel ranking path. The historical inspection helpers remain public only for
projection diagnostics and tests, with the weaker type.

### Canonical bounded ingress

Working-set session/task scope is constructed through bounded newtypes before a
provider can run. New trusted context admissions reject surrounding whitespace,
control or Unicode-format characters, non-NFC strings, and node IDs that differ
from the retrieval layer's canonical exact-identity form. Existing version-1
events remain replayable; the stricter rule applies to new kernel admissions and
does not rewrite history.

`SearchContext` validates fixed collection and component bounds, uniqueness,
canonical Unicode, and runtime completeness before query embedding. A preferred
placement is valid only when it is in the available-placement set, and the
ranking bonus requires that same membership.

### Operational evidence and local privacy

The event and projection stores reject symlink/non-regular database targets,
require current-user ownership on Unix, set data directories to `0700`, and set
database/WAL/SHM files to `0600` when present. These checks add no remote or
credential service.

`scripts/agent-canary.sh` becomes part of the canonical repository gate. A
tracked Task 004 evidence manifest records reviewed commits, exact commands,
verdicts, tool versions, and hashes of retained local evidence without
committing `.omo` or `.surf` contents.

## Rejected alternatives

- Lowering the 10,000 count alone does not bound composed bytes or provider
  work and preserves history-dependent failure.
- Trusting the projection checkpoint without an in-process source proof makes a
  cache an authority.
- Replaying the full session on every request preserves correctness but makes
  latency proportional to repository age.
- Persisting embeddings or adding a vector service would widen mandatory
  infrastructure and credential boundaries; both remain deferred.
- Silently canonicalizing durable IDs would make two caller identities alias.
  New ingress rejects non-canonical forms instead.
- Wall-clock, p95, and process-wide RSS thresholds are host- and scheduler-
  dependent correctness gates. CI instead exercises the maximum-size lazy
  generator against fixed checked work counters and exposes replay/delta/cache
  metrics. Non-gating performance measurements may be added once a stable
  benchmark host exists; they cannot replace the deterministic envelope.

## Compatibility and migration

Legacy context compilation and raw-string capability search remain unchanged.
Task 003 event/replay behavior is unchanged. Capability manifests without a
`lifecycle` field deserialize as active. Projection schema 1 is rebuildable and
is replaced automatically by schema 2. Working-set construction gains typed
scope and typed work-budget failures; this Task 004 API has no stable external
wire route.

## Measurable consequences and rollback

- Tests must prove exact N/N+1 cumulative counters, one query embedding, bounded
  document-call/input totals, and top-K retention under a 10,000-candidate
  generator.
- A repeated working-set query at an unchanged high-water must not increment the
  full-replay counter; a delta must advance from the prior checkpoint.
- More than 10,000 superseded/expired/disputed nodes or retired/quarantined
  manifests must not stop an otherwise bounded active namespace.
- Invalid scope/SearchContext must fail before provider calls.
- Derived snapshots must fail to compile as a kernel candidate source.
- The canonical gate includes canaries, focused resource tests, and a tracked
  evidence manifest.

Rollback is removal of the 004.1 API/code while retaining canonical events and
rebuilding the derived schema-1 projection. No event migration or source rewrite
is required.
