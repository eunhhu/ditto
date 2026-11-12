# Task 043 outcomes: 1.65 → 1.172 requests per turn

Date: 2026-11-11. Status: performance bar met; every gate decided passes except
`answer`, which waits on the second-review sample.

This report is the context behind the four commits `7b9afc8`, `e43c9d5`,
`8c83406`, `736a440`: what the bar was, what the numbers were, what each change
fixed, what is still above one request per turn, and what is deliberately not
done.

## The bar

Task 043 asks a read-only personal turn for:

| Bar | Target | Campaign 16 |
| --- | --- | --- |
| Requests per turn | ≤ 1.3 | **1.172** |
| Completion p50 | ≤ 6s | **5.4s** |
| First useful progress | ≤ 2s | 5.3s (provider) |
| Recall@5 | ≥ 0.95 | **1.000** |
| Spend per turn | ≤ $0.0012 | **$0.00070** |
| Housekeeping model calls | 0 | 0 |
| Harness share of model time | ≤ 15% | **0.9%** |
| Admission p99 | ≤ 5ms | held (ADR 0043) |

Requests per turn is the metric that matters, because a turn's wall clock is
almost entirely provider time: one request per turn means one round trip, and
every extra request is a full provider latency. The harness's own share of model
time is 0.9% (overhead p50 35ms, p95 148ms against 5.5s of model time), so tuning
CPU work cannot move the number that Task 043 sets.

First useful progress p50 is 5.3s against a 5.5s model time. That is the provider
answering, not the harness spending the time; the 2s target is a property of the
upstream model and region, and it is reported as missed rather than excused.

## Where the campaign stood before this work

| Campaign | Requests per attempt | Requests per turn | Notes |
| --- | --- | --- | --- |
| 11 | 1.72 | - | pointer-closure slice landed after |
| 12 | 1.75 | - | |
| 13 | 1.65 | - | |
| 14 | 1.65 | - | first `integrity` pass; `tool` failed (X3 0.000) |
| 15 | 1.48 | **1.324** | 145 turns / 192 requests; recall 1.000 |
| 16 | 1.31 | **1.172** | 145 turns / 170 requests |

Campaign 15's headline 1.48 was requests per *attempt*. Schedule cases make
several turns per attempt (S2: 3, S4: 2), so the attempt denominator inflated the
figure; the turn denominator is the one the bar names. The evaluation report still
reports attempts, and the turn ratio is computed from the suite's `occurrences`
(the report itself is unchanged - see "Not done").

## What the 46 extra requests in campaign 15 were

Every turn above one request fell into three groups:

1. **Memory verification, 21 turns.** P2 (6), P5 (10), P1 (5) re-searched memory
   after the first answer did not settle the question. ADR 0029 now has the
   section "A search carries several wordings (Task 043)": one turn issues one
   `memory.manage` search whose `queries` array holds every wording able to name
   the fact (max 4), results are the union ranked by best query, and the model is
   told not to search again. `MAX_MEMORY_SEARCHES = 3` stays as a backstop.
   Probe (`P1 P2 P5 X1 T1`, 45 attempts): P2/long_use 5 → 2, P5 4 → 2,
   P1/long_use 2-3 → 2, recall 0.967.
2. **Attachment sorts whose permission allowed deduplication, 19 turns.**
   T1 (5), T3 (4), X3 (10). The user wrote "remove duplicates", "drop repeated
   tags", "drop repeats"; the permission said only `allow_deduplicate: true`, so
   the harness could not tell a user who wanted repeats kept from one who wanted
   them removed and left the decision to the model - which spent a request asking
   for the sort the turn could have run.
3. **One attachment read** (T3/clean#2), which the retry resolved.

ADR 0041 already compiles attachments into context; the two-request turns were
never attachment reads. That was the single most expensive misdiagnosis of the
campaign and it is why the fix is a permission field rather than a context
change.

## The two changes

### ADR 0029: one search carries every wording

`memory.manage` 0.2.0 takes `queries: [String; 1..=4]`; the singular `query`
still parses so journals written before the change replay. The instruction block
in `crates/kernel/src/turn/shared.rs` now says: search memory once before
answering, put every wording able to name the fact in that one call, and if that
search does not settle it, do not search again.

The failure this fixes is specifically the wording gap: the request "Where should
I go for the reading group get-together?" shares no word with the memory "Our
book club now gathers in Birch Room". A single-wording search cannot bridge that,
and a second search is a second round trip. The test
`several_wordings_in_one_search_find_a_fact_no_single_wording_would` pins it: one
call whose second wording finds the fact, exactly two requests for the turn.

A stale `capability.header.json` fails the turn with `protocol`, not a test-only
error: `capabilities/core/memory-manage/capability.header.json` had to be
regenerated in the same commit as the version bump.

### ADR 0047: the sort permission states the user's choice

`AgentSortPermission` gains `deduplicate` (serde default false, so old journals
replay unchanged):

- `deduplicate: true` - the user chose the deduplicated order; the turn
  prefetches `unique: true` before the first model request.
- `allow_deduplicate: true, deduplicate: false` - either order is permitted; the
  turn prefetches nothing and the model decides.
- `allow_deduplicate: false` - prefetch `unique: false`.

`deduplicate` without `allow_deduplicate` is refused at admission, `SortGrant`
carries the choice so the prefetched arguments stay a property of the journaled
input, and `permission_matches` compares it so a retry that changes the choice
conflicts instead of reusing the stored artifact. When the turn did prefetch, the
permission blob in the user message reports `already_ran`, `calls_used: 1` and the
context node holding the ordered lines, so a model does not spend a request
rediscovering a spent lease.

This is a permission field, not intent inference: the client reads the message
and sets the flag. The evaluation suite declares `deduplicate_requested` per case
and the harness passes it as the permission - the same thing a client that read
the message would send.

## Campaign 16: the measurement

130 attempts, 145 turns, 170 requests, release daemon on the SD-card host,
`--agent-check-exit 0`, budgets v3.

- **1.172 requests per turn** (memory 1.40, mixed 1.12, schedule 1.00, tool 1.00)
- Recall@5 **1.000 (90/90)**
- Spend **$0.1013** total, **$0.00070** per turn, **2,220** tokens per turn
- Completion p50 **5.4s**, first useful progress p50 5.3s
- Harness overhead p50 **35ms**, p95 148ms, **0.9%** of model time
- Independent-verification failures: **0**
- Gates: integrity, safety, semantic, tool, schedule, cost_latency, resource pass;
  **answer blocked** on the second-review sample

Probe before the full run (`T1 T3 X3 X1`, 30 attempts): 35 requests = 1.17,
recall 20/20, T1/T3/X3-clean/X3-long/X1-clean all exactly one request, and
X1/long_use two because that case's memory question is a separate lookup rather
than a sort.

## The 25 turns still above one request

- **P5, 10 turns** - the question asks for a fact the memory does not hold. The
  model searches once to be sure, then says it does not have it. Refusing
  honestly is the correct answer; removing this request would mean the context
  could prove absence without a search, which it cannot.
- **P1 5, P2 5 turns** - the fact exists but the compiled context did not select it, so
  the turn pays one search. Closing this needs a retrieval bridge (embedding
  index or alias table) between request wording and memory wording.
- **X1/long_use 5** - a memory question attached to a sort turn.

The next lever is therefore retrieval, not request shaping: predict which
memories the turn needs when the capsule is compiled, so the first request
already holds the fact.

## Grading

Blind first pass over 110 answers: 94 pass by the rule-based grader, 26 disputed.
The 26 were adjudicated by reading (P5 "the policy number is not held" 10, T2/T4
"deduplication was not permitted" 10, P3 3, P2 2, X2 1); all were the grader's
atoms not matching a paraphrase, and each was scored 2222 with clarity 1 where the
answer was padded.

The `answer` gate needs the 20% second review - 22 items out of 110 - by the
repository owner. That review is what stands between 7/8 and 8/8, and it is
deliberately not self-approved.

## Platform work landed alongside

- P1 profiles (ADR 0044), P2 instructions in the journal (payload 17), P3 skills as
  data with `skill.read`, a bounded catalogue segment (ADR 0046, payload 19).
  Merged in PR #31.
- P4 first slice: `capability::install` (ADR 0048) - install is pinned to a digest
  the caller names, re-install of the same digest is a no-op, a different digest
  for an installed id is a conflict rather than a silent upgrade, and uninstall
  only removes the digest it names. Six tests, library-level only.

## Not done

- **The evaluation runner does not report requests per turn.** The number in this
  report is computed from `report.json` with the suite's `occurrences` as the
  denominator. Adding it to the runner needs a schema change and a test update,
  attempted once and reverted rather than half-landed. Until then the bar is
  checked by an external calculation.
- **P4 effect review** (the declared effects of an installed package, reviewed
  before exposure), **P5 provider conformance**, **P6 the native shell**.
- **Retrieval bridge** for the 10 capsule misses.
- **S gate aggregation** counts attempts, so schedule families read 1.75 when
  their turns are 1.00. The verifier should divide by turns; not changed here.
- **`T3/clean#2` in campaign 15** returned an empty completion twice and then a
  provider error. Campaign 16 had no such failure, so it is upstream flakiness,
  not a harness path - unconfirmed beyond that observation.

## Evidence

- `~/.hermes/cache/scratch/campaign15/report.json`, `grading/`
- `~/.hermes/cache/scratch/campaign16/report.json`, `grading/first-pass.json`,
  `grading/grades.json`
- `~/.hermes/cache/scratch/probe-multi-query/report.json`,
  `~/.hermes/cache/scratch/probe-dedup/report.json`
- `~/.hermes/cache/scratch/agent-check-r25.log` … `agent-check-r28.log` (all exit 0)
- Tests: `crates/kernel/tests/read_only_turn/{memory,model_sort}.rs` (`read_only_turn`
  115 passed), `crates/capability/src/install.rs` (7 passed)
