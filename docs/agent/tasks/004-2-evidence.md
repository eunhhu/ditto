# Task 004.2 verification evidence

## Reviewed range

- Reviewed base commit:
  `70e79731a756c872f7fc35087bf2af1071d049f2`
- Governing decision: the Task 004.2 amendment to
  [ADR 0011](../../adr/0011-retrieval-resource-envelope.md)

The closing commit that contains this manifest owns the correction code, tests,
and verified agent-state updates as one tree.

## Correctness audit

| Finding | Resolution | Regression evidence |
| --- | --- | --- |
| Cache repair reset the cumulative candidate budget | The repaired active-snapshot capture continues with the same cloned request-local budget charged by the first capture. The reset from the caller's original budget was removed. | `cache_repair_accumulates_candidate_work_across_both_snapshot_attempts` forces SQLite drift after a successful first capture. Its precharged headroom fits either capture alone but not both, so it returns typed `candidate_bytes` exhaustion and no `VerifiedContextSnapshot`. |
| SQL truncated validity while Rust retained nanoseconds | New trusted durable admission rejects a field-specific non-millisecond `valid_from` or `valid_until`. Projection schema 3 stores each timestamp's millisecond value and remaining 0..999,999 nanoseconds, then applies the exact inclusive-start/exclusive-end comparison in SQL. Legacy version-1 source events are not rewritten or rejected. | Kernel negative cases prove both fields fail before append/publication. `schema_three_replays_legacy_submillisecond_validity_and_exact_millisecond_boundaries` proves legacy 500–1,500 microsecond selection plus exact millisecond start/expiry boundaries agree with `ContextNode::is_valid_at`. |
| Context retention was documented as fully streaming | ADR, task, frontier, handoff, and evidence language now distinguishes bounded candidate materialization from streaming document processing and top-K ranked retention. | Documentation canary and canonical gate below. |

## Replayed commands and verdicts

All commands ran from the repository root on 2026-09-01.

| Command | Verified result |
| --- | --- |
| `rtk cargo test -p ditto-context-projection --locked --all-targets` | Passed: 38 tests across 2 suites. |
| `rtk cargo test -p ditto-kernel --locked --test durable_context_projection` | Passed: 15 tests. |
| `rtk cargo clippy -p ditto-context-projection -p ditto-kernel --locked --all-targets --all-features -- -D warnings` | Passed with no warning or error. |
| `rtk ./scripts/agent-check.sh` | Passed: tracked canary, formatting, strict workspace Clippy, and 328 tests across 35 suites. |
| `rtk cargo +1.88.0 check --locked --workspace --all-targets` | Passed for every workspace crate and target. |
| `rtk git diff --check` | Passed. |

No network, model, credential, provider, or billable embedding operation ran.

## Reproducibility identities

- `crates/context-projection/src/lib.rs` blob:
  `6e12278496d90abfe666e218a29a5493e7aeff64`
- `crates/context-projection/tests/projection.rs` blob:
  `688443e29521bfc8694f7e80d8965790a00e260c`
- `crates/kernel/tests/durable_context_projection.rs` blob:
  `64222d45d3221c4c3f95e549b90cd1923b443afc`
- `crates/kernel/tests/durable_context_projection/working_set.rs` blob:
  `2434e61518c3c24b1cf5d5fbc39007e84e8bddff`
- `docs/adr/0011-retrieval-resource-envelope.md` blob:
  `72581eaffc02e52c7ae6a2debf99971451364324`
- `Cargo.lock` blob:
  `70aef439832b641f51177d5214effef73374b675`
- Cargo and Rust compiler: `1.88.0`
- SQLite CLI: `3.51.0 2025-06-12`

Local `.omo` and `.surf` contents remain untracked and are not evidence inputs.
