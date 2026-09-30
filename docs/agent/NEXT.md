# Implementation frontier

Work in order unless the user explicitly changes priorities. Complete one task
file's exit criteria before moving the marker.

## Active

Task 017 is the drafted [personal-task outcomes and evaluation
specification](tasks/017-evaluation-outcomes.md). It defines representative
semantic recall, answer-quality, tool/schedule, safety, resource and cost/latency
evaluation needs after Task 016's bounded lexical corpus. This is a specification
only: the broader suite, calibrated budgets, live model-answer assessment,
semantic retrieval, tool-task success and v0.1 readiness are not verified.
General approval fulfillment remains deferred.

On 2026-09-30 the user prioritized a prerequisite before Task 017's measured
runs: production run context misses paraphrased questions and admits unrelated
memories (see the handoff's known gaps). That fix is Task 016.1; its contract
lands with its implementation.

## Completed

One line per slice; each task file and its evidence hold the details.

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

## Later

- calendar recurrence, notification delivery and scheduled effect grants only
  after their product need and policy contracts are defined;
- batched, compact rerank pools and descriptor/hash caching before a production
  embedding worker;
- device registry, SSH placement, and expanded gateway integrations after the
  local workflow is useful;
- evidence-gated improvement compiler.
