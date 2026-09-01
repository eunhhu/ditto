# Task 004.2: Repair budget and validity precision

## Status

Completed under the Task 004.2 amendment to
[ADR 0011](../../adr/0011-retrieval-resource-envelope.md). The replayable audit
record is [Task 004.2 verification evidence](004-2-evidence.md).

## Objective

Close two correctness gaps found after Task 004.1 without widening into the
effectful Task 005 architecture: cache repair must not reset request work, and
durable validity must have one exact interpretation in admission, SQLite, and
source replay.

## Required vertical slice

1. Carry one `RetrievalWorkBudget` monotonically through every verified-snapshot
   attempt, including the permitted cache rebuild/recheck.
2. Reject sub-millisecond `valid_from` or `valid_until` on new trusted durable
   context admission before append or publication.
3. Rebuild projection schema 3 with exact sub-millisecond lifecycle columns so
   previously recorded version-1 events retain their original inclusive-start,
   exclusive-end semantics.
4. Add adversarial tests for combined retry work, both invalid fields, exact
   millisecond boundaries, and legacy fine-precision replay.
5. Correct documentation that described context candidate retention as fully
   streaming and record the larger delta-index and capability-loader work as
   deferred, bounded slices.

## Non-goals

- No canonical capability invocation, effect, lease, or executor work.
- No compact authenticated session index or admission-history algorithm change.
- No capability package-header index, cold manifest paging, or loader migration.
- No production embedding worker, batching, or cache.
- No event rewrite or new context event payload version.

## Exit criteria

- A cache drift after a successful first snapshot attempt causes a typed
  candidate-budget failure when the combined retry work crosses N; the old
  reset-at-repair behavior would make the same test succeed.
- Sub-millisecond `valid_from` and `valid_until` drafts are rejected with a typed
  field-specific error and add no event or publication.
- Exact millisecond start is included and exact millisecond expiry is excluded.
- A directly replayed legacy version-1 sub-millisecond window is selected and
  excluded at the same instants as `ContextNode::is_valid_at`.
- Focused tests, strict Clippy, `./scripts/agent-check.sh`, and Rust 1.88 pass.
