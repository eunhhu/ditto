# Task 015: Offline context-quality and longer-history workloads

## Observable contract

Measure one deterministic synthetic memory-recall scenario through the actual
CLI, loopback HTTP adapters, kernel and cfg(test) injected driver. Compare fresh
stores containing the same facts with zero versus N unrelated same-session
memories. Save an old fact, correct that exact memory ID, save an irrelevant
same-session fact and a query-matching fact in another session. Kill/reopen the
server before multiple unique requests with the fixed query `cedar timezone`.

The actual model-facing ContextCapsule must contain the corrected memory with
its exact text; every request must exclude the superseded memory, irrelevant
memory, other-session value and all synthetic noise. Reconcile opt-in test-only
JSONL capsule observations with durable model-request identities and payloads
and the unchanged `fixture-calls.txt` counter. A fixture answer is never quality
evidence. This adds no production logging, runtime surface, authority, policy,
wire contract, dependency or architecture change.

The standard-library harness uses Task 014's offline build/artifact resolver,
server lifecycle, CLI, latency, RSS and event-accounting helpers. Bound N to
1–5000 and unique samples to 2–100. Defaults are N=1000 and five queries per
profile; the canonical smoke uses N=4 and two queries per profile. Zero-noise
minimal history is always included. No latency or RAM threshold is imposed.

Record raw request latency and nearest-rank p50/p95, spawn/readiness and recovery,
seed duration, recovered listing duration, server RSS/high-water RSS, durable
events and storage including separately identified fixture logs, model/tool
counts, exact selected capsule node counts/serialized bytes, hardware/build
settings and source/binary SHA-256. Execute and hash exact named Cargo artifacts.
Report explicit correction, stale/irrelevant/noise exclusion and scope-isolation
outcomes. Model-answer quality, live usage/cost, first-useful-progress and
isolated model/tool/Ditto timing remain null/unavailable; external provider
spend is known zero. Unsupported RSS is null, never a fabricated zero.

## Failure model and exit criteria

1. RED assessment regressions and a real small CLI/save/correct/restart/query
   regression precede implementation. Reject missing, duplicate, mismatched or
   unrelated observations, incorrect values and excluded/unknown context.
2. Focused tests, a bounded canonical smoke and `./scripts/agent-check.sh` pass.
   Context observation is opt-in; signal/failure paths reap the server and
   temporary stores. No housekeeping/provider call occurs during seed/recovery.
3. Record the full default workload as `015-quality-history.json`; the evidence
   table agrees with that report and its source/binary hashes remain current.
4. Review the complete diff for authority/status overclaim, network/provider
   use, cleanup, stale measurements and source drift before updating the frontier.

## Limits

This tests existing V1 lexical selection with deliberately unrelated synthetic
text, not semantic retrieval, cross-session recall, general personal-agent or
model-answer quality. Context contains explicit memories, not an automatically
injected transcript. Listing warms the recovered process before measured queries.
RSS excludes CLI children; storage includes live WAL/SHM logical file sizes.
Recovery covers abrupt process loss, not host/power failure. One debug-build
scenario establishes no performance superiority, free inference, zero total
cost or v0.1 readiness. Broader workloads remain separate work.

## Verification state

Complete locally, with the implementation intentionally uncommitted. RED evidence,
eight focused Python regressions, Task 014 compatibility, the full canonical gate
and the default 1,000-memory/five-query comparison passed. All ten observed
capsules contained one corrected node (227 serialized bytes), with every required
exclusion. See [verification and measurements](015-evidence.md) and the
[raw report](015-quality-history.json). Broader personal-agent quality and v0.1
readiness remain open.
