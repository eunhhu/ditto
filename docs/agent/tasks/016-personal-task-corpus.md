# Task 016: Offline personal-task context corpus

## Contract and frozen oracle

Extend Task 015's standard-library harness, schema **2**, workload
`task016-offline-personal-task-corpus-v1`. The embedded corpus and its complete
SHA-256 definition accompany every report. Expectations are fixed independently
of observed capsules and fixture answers. No Rust, production surface, provider,
policy, authority, dependency or wire change is required; no ADR is needed.

All data is synthetic. Save in this order through the actual CLI:

| Label | Session | Exact text | Operation |
| --- | --- | --- | --- |
| OLD | personal | `cedar timezone is UTC` | Save |
| CURRENT | personal | `cedar timezone is KST` | Save, replacing OLD's returned memory ID |
| PACKING | personal | `harbor packing checklist includes passport` | Save |
| TRAIN | personal | `harbor train departs Friday` | Save |
| FOOD | personal | `supper preference is vegetarian` | Save |
| IRRELEVANT | personal | `bananas ripen tomorrow` | Save |
| OTHER | elsewhere | `cedar timezone is PRIVATE` | Save |
| NOISE[i] | personal | `synthetic unrelated pebble {i:06d}` | Save for each i in 0..N-1 |

Every query runs in `personal`, in the following order in each complete round:

| Case | Exact query | Expected ordered labels | Cardinality |
| --- | --- | --- | ---: |
| corrected_timezone | `cedar timezone` | CURRENT | 1 |
| trip_packing | `harbor packing` | PACKING, TRAIN | 2 |
| trip_train | `harbor train` | TRAIN, PACKING | 2 |
| meal_preference | `supper preference` | FOOD | 1 |
| no_match | `observatory telescope` | empty | 0 |

The current V1 compiler uses distinct lowercase alphanumeric tokens longer than
one character, overlap, authority and confidence. These seeds have equal
authority/confidence and do not match the run signature's added `local content
read` terms. Two-token matches score 4; one-token matches score 3. Both trip
memories are eligible, with strict reversed order independent of generated IDs.
These are deliberately lexical context needs, not a representative sample of
personal-agent performance.

## Real-process workflow

Use fresh zero-noise and N-noise stores. `--history-size` is bounded to 1..5000,
default 1000. `--samples` means **repetitions per query**, bounded to 2..100,
default 5. The canonical smoke is N=4, samples=2: ten runs per profile, twenty
total. The default is twenty-five per profile, fifty total.

Require 7+N successful CLI saves and exactly 2×(7+N) seed events, without
model/tool calls or observer records. Correct using `--replaces`; never write
databases directly. Resolve every seed's exact user-input provenance separately
from the memory-record event returned by save. Kill/reopen the server and
paginate `memory list --limit 100` for both sessions. Require exactly CURRENT,
PACKING, TRAIN, FOOD, IRRELEVANT and noise in `personal`, OTHER in `elsewhere`,
and no OLD. Reopen/listing must append no events or driver records.

Every run uses a canonical client request ID unique across both profiles and must return
`unverified`, with one durable model request, independent call-log entry and
capsule observation. No tool dispatch or `task.completed` is allowed. Reconcile:

`case/repetition → client request ID → input event → task/turn → model.requested → call log → observation`.

Check lengths and uniqueness before pairing; exact queries in durable input and
model conversation; session/task/turn, request index, model ID, event identity,
contiguous sequence and causal chain to the input; and the post-restart boundary.
Reject missing, extra, duplicate, reordered, swapped, malformed or mismatched
records. Parsed capsules must equal durable capsules; their UTF-8 serialized
bytes must match the known ContextCapsule struct field order exactly. This is
not a claim about the journal's map serialization order. Preserve raw observed
`context_json`, model/input events, all post-restart events, independent calls,
expected items and seed provenance after temporary stores disappear.

## Assessment and metrics

An exact item has the returned memory ID, independently frozen summary,
`claim/user/asserted/session`, confidence 1.0, exact original input provenance,
and absent validity bounds. Extra fields and duplicate IDs fail. Every request
passes individually before any aggregate is published. There is no combined
agent-quality score.

Every metric retains numerator, denominator, value, null reason and contributing
IDs/ranks (rank is one-based). Empty denominators have null values with reasons.

| Metric | Definition |
| --- | --- |
| Exact set | All valid expected items and only those items, without duplicates; one opportunity per request |
| Exact order | Exact set plus independently specified order; one per request |
| Nontrivial order | Exact order for expected cardinality >1; zero opportunities otherwise |
| Recall@2 | Distinct valid expected IDs at ranks 1..2 divided by expected cardinality; empty expectation is null |
| Returned-context precision | Distinct valid expected IDs returned divided by actual returned-node count; empty output is null |
| Stale/scope leaks | Forbidden memories exposed by ID or exact forbidden text within any summary; one opportunity each per request |
| Irrelevant leaks | Active same-session non-noise memories outside expectation, 5−cardinality opportunities per request |
| Noise leaks | All N noise memories; zero noise means null rate with zero opportunities |
| Identity reconciliation | Fully reconciled requests / planned requests; raw planned, durable-input, durable-model, call-log, observation, missing, duplicate and unmatched counts |

Leak numerators count distinct forbidden memories, even when repeated in an
output. Diagnostics retain missing/extra/duplicate IDs and malformed/mismatched
nodes. Reconciliation's missing counter sums cardinality deficits across the
four observed collections; duplicate counts sum repeated identities per
collection; unmatched counts sum symmetric differences between corresponding
identity sets. These counters are diagnostic, not a quality score.

For each passing default profile: exact set/order and identity 25/25,
nontrivial order 10/10, micro Recall@2 and returned precision 30/30; stale/scope
0/25, irrelevant 0/95, noise 0/0 (null) or 0/25000.

## Measurements, lifecycle and exit criteria

Reuse Task 014's exact offline Cargo artifact resolver, cleared runtime
environment, proxy-disabled loopback HTTP, CLI, pagination and lifecycle helpers.
Record raw startup/recovery, seed/listing time, each query's latency and separate
per-case/per-profile nearest-rank p50/p95, explicitly labeled pooled corpus
latency, server RSS/high-water RSS, file/WAL/fixture-log sizes, event/call counts,
selected nodes and capsule bytes, hardware and build settings.

1. Assessment/reconciliation adversaries and an independent real-process test
   precede implementation. Record focused RED logs; missing corpus coverage is
   not a fabricated retrieval defect.
2. Preserve opt-in observer/call-count compatibility, argument bounds and `-O`
   rejection. Prove normal/failure/SIGTERM/SIGINT child reaping and store cleanup;
   assessment failure cannot publish success or damage a previous report.
3. Pass focused quality tests, Task 014 tests, bounded smoke, the unchanged
   `./scripts/agent-check.sh`, cached Rust 1.88 offline workspace/all-target check,
   hash/audit checks and diff hygiene. Never download a missing dependency or
   toolchain or substitute mocked success for a blocked process check.
4. Only after gates pass, record the full default
   [raw report](016-personal-task-corpus.json). Publish via a same-directory
   atomic replacement after complete pass and final source/artifact hash checks.
   Independently recompute aggregates and all source/artifact/corpus hashes.
5. Update [evidence](016-evidence.md), QUALITY-GATES, NEXT and HANDOFF with actual
   results. Leave the working tree uncommitted until independent review passes.

## Compatibility and claim boundary

Task 015's frozen report and reproduction commands are **historical**: running
its old commands now uses Task 016 corpus semantics and cannot reproduce its old
sample counts or measurements. The test-only observer, call log, CLI, runtime
and Task 014 measurement contract remain unchanged.

Claims are limited to **five-case synthetic lexical ContextCapsule conformance
after restart**. Model-answer quality, task completion, semantic recall,
tool-task success, live cost, first useful progress, isolated Ditto overhead,
general agent quality and v0.1 readiness remain unavailable/open. Paraphrases,
ambiguity, token pressure, scheduling and learning are outside this corpus.
Listing warms recovery; observation adds file I/O; sequential debug runs on an
uncontrolled host establish no latency superiority. RSS excludes CLI children.
Restart tests process loss, not power loss. External provider spend is zero;
total operating cost is unknown.

## Verification state

Complete locally and ready for repository delivery. The final ten assessment tests,
eight workflow tests, eight Task 014 compatibility tests, twenty-run smoke,
canonical gate (490 Rust tests plus Python/smoke checks), cached Rust 1.88
workspace/all-target check and fifty-run default report passed. Independent
literal-oracle, raw aggregate, identity/provenance and source/artifact/corpus
hash audits passed. All eight authorized files passed final scope/link/canary
and diff checks; measured processes and temporary stores were removed.
See [exact commands, measurements and log digests](016-evidence.md).
