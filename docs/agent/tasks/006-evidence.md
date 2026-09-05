# Task 006 verification evidence

## Reviewed range

- Reviewed base commit:
  `0b5259d358843b2349dab89535b3e84719fa28aa`
- Contract commit:
  `5a7402f46a4044022238f06cc32c7d1a2cee05f2`
- Implementation and adversarial-test commit:
  `71a41791dfbf3e5c7affca38d8a4fa1de90c045f`
- Tested implementation tree:
  `4f4a2fce9187439d3f7c81fa604fd6659ba21be3`
- Governing decision:
  [ADR 0013](../../adr/0013-compact-source-verified-session-index.md)

The closing commit that contains this manifest owns the exact non-content
data-version recovery clarification plus verified task-state and evidence
updates. The implementation commit owns projection schema 4, the compact index
and proof path, deterministic counters, focused documentation, and all Task 006
regression fixtures.

## Exit-criterion audit

| Contract claim | Inspectable implementation | Regression evidence |
| --- | --- | --- |
| The event spine remains the sole durable authority | Kernel open and explicit rebuild reset the derived schema and replay the event store in fixed 500-event pages. `SourceVerification` is process-local and non-serialized; it binds the exact schema-4 checkpoint, event anchor, canonical digest, SQLite data version, and compact identity map. Low-level derived snapshots and diagnostic index checkpoints carry no source authority. | Existing source actor, session/task scope, prior-source, greatest-causation, identity, and exact-scope supersession adversarial tests all pass. Cache-only rows, identities, and edges still trigger repair or rejection without changing source events. |
| Normal retrieval and admission do not scan an affected session from sequence zero | Verified delta synchronization starts at the exact proof checkpoint. New context dependencies use compact `(session_id, node_id)` lookups plus at most 64 exact event-ID source lookups. Live admission reads only the process-local source-verified identity map and exact cited sources. The only remaining sequence-zero scoped view is the explicitly named audit path; startup/recovery full replay remains global and page-bounded. | `verified_snapshot_reuses_one_full_replay_and_advances_by_delta` proves unchanged reads add no replay work. `million_event_prefix_steady_state_visits_only_delta_and_compact_index` fixes 1,000,000 ordinary events and 10,000 identities, then observes `full_replay_events=1010000`, `steady_delta_events=1`, and `admission_index_lookups=1`. |
| Schema 4 is deterministic and atomically checkpointed | `session_index_nodes` stores immutable identity, scope, canonical node/provenance/supersession digests, source/edge counts, greatest causation, and accounted bytes. `session_index_state` stores each session's ordered digest/count/bytes. The singleton checkpoint stores the ordered global digest. A projection page inserts retrieval rows, edges, index rows, session state, and the digest-bearing checkpoint in one SQLite transaction. | `schema_four_global_and_session_digests_are_rebuild_and_migration_stable` compares repeat rebuild and schema-3 replacement. Existing incremental/reopen/cache-deletion equivalence and real-trigger atomic rollback tests pass. `canonical_index_digest_is_domain_separated_ordered_and_field_sensitive` fixes digest sensitivity. |
| Normal work has exact fixed bounds | Checked counters cap one session at 65,536 identities and 256 MiB accounted index bytes, and one verified delta at 65,536 events, 64 MiB context payload bytes, and 2,000,000 verification-work units. A charge precedes each event validation, context decode, source lookup, identity lookup, and supersession lookup. The event-count loop refuses another event-store page when no event budget remains. | `compact_index_and_delta_counters_accept_exact_bounds_and_reject_n_plus_one` covers every exact N/N+1 counter and leaves the counter at N. `normal_delta_accepts_n_events_and_rejects_before_committing_n_plus_one` drives a real event store: N succeeds, N+1 returns the typed dimension error, the projection checkpoint remains at N, and all N+1 source events remain immutable. |
| Mutable cache state cannot acquire authority | Before reusing a generation after an external SQLite data-version change, the loader recomputes the global and per-session chains, checks every index row against its projected JSON, active-filter columns, complete outgoing edge shape, counts, and accounted bytes, and compares the resulting identities with the remembered source proof plus the canonical delta. A mismatch gets one reset/full replay/recheck. Canonical delta failures are prevalidated first so cache drift cannot mask their existing typed error. | `compact_index_row_state_and_checkpoint_tampering_rebuild_once_without_authority` covers deleted index rows and altered node, session, and global digests. Existing row/edge/anchor corruption matrices pass. `persistent_post_rebuild_session_index_corruption_returns_no_snapshot` changes the rebuilt session digest and receives the typed integrity error without a value or second rebuild. |
| Task 003 and post-append recovery semantics are unchanged | No event payload version, context ingress, publication order, working-set result, capability, invocation, permit, executor, or replay contract changed. Projection failure still occurs after durable acceptance, publishes the exact committed record once, and recovers without another append. | All 15 durable-kernel projection/working-set tests and all 38 Task 003 read-only-turn tests pass, including `committed_projection_failure_publishes_once_returns_record_and_recovers_without_duplicate`. No `task.completed` producer was added. |

## Replayed commands and verdicts

All commands ran from the repository root on 2026-09-04.

| Command | Verified result |
| --- | --- |
| `rtk cargo test -p ditto-context-projection` | Passed: 45 tests across unit and integration suites, including the 1,010,000-event fixture. |
| `rtk cargo test -p ditto-kernel --test durable_context_projection` | Passed: 15 tests. |
| `rtk cargo clippy -p ditto-context-projection --all-targets --all-features -- -D warnings` | Passed with no warning or error. |
| `rtk proxy cargo test -p ditto-context-projection --test projection million_event_prefix_steady_state_visits_only_delta_and_compact_index -- --exact --nocapture` | Passed in 8.03 seconds; emitted the deterministic counters `ordinary_events=1000000 context_identities=10000 full_replay_events=1010000 steady_delta_events=1 admission_index_lookups=1`. The duration is observational, not a gate. |
| `rtk proxy cargo test -p ditto-context-projection --test projection normal_delta_accepts_n_events_and_rejects_before_committing_n_plus_one -- --exact --nocapture` | Passed in 0.66 seconds. |
| `rtk ./scripts/agent-check.sh` | Passed: tracked canary, formatting, strict workspace/all-target/all-feature Clippy, 351 unit/integration tests, and 24 compile-fail doctests across 36 suites. |
| `rtk cargo +1.88.0 check --locked --workspace --all-targets` | Passed for every workspace crate and target. |
| `rtk git diff --check` | Passed. |
| PR #7 Actions run [`33868899639`](https://github.com/eunhhu/ditto/actions/runs/33868899639) on `71a41791dfbf3e5c7affca38d8a4fa1de90c045f` | Passed independently: `msrv` in 27 seconds and the `rust` repository gate in 1 minute 35 seconds. |

No Ditto capability worker, subprocess, model/provider request, application
network access, credential resolution, SSH, approval fulfillment, file-mutation
capability, or completion event ran. Tests used only local temporary SQLite
fixtures; the requested Git push and pull-request checks were the only external
repository operations.

## Reproducibility identities

- `crates/context-projection/src/lib.rs` blob:
  `dddfdf89fab55c8b5cc1c718b0cc86a79169f62d`
- `crates/context-projection/tests/projection.rs` blob:
  `f05737ba76672ee6e2d712169f5c315cfcfa3b4e`
- `docs/adr/0013-compact-source-verified-session-index.md` blob:
  `2ff76fa40776678069bc6e6ec565bbf04e597a7d`
- `docs/agent/tasks/006-compact-session-index.md` blob:
  `436a3d4b66d1d6c8f7b5971b916a16477a61092e`
- `docs/adr/0011-retrieval-resource-envelope.md` blob:
  `6f843f3ec94aba9df95e6e45aaf994f1fa067518`
- `docs/architecture.md` blob:
  `d965c44df6dc2a255a4874671a5798f8d49e9427`
- `docs/specs/context-ir.md` blob:
  `00de2b8be279e47d7ace559bdeb63e3f7f6fec6e`
- `docs/specs/event-protocol.md` blob:
  `7c64d02aeb7ad5efe60b337de4c5ad62cd7c2c09`
- `Cargo.lock` blob:
  `3ef4861bf38046e5ea2ce3dff8b85e37ee90d170`
- RTK: `0.46.0`
- Cargo: `1.88.0 (873a06493 2025-05-10)`
- Rust compiler: `1.88.0 (6b00bc388 2025-06-23)`

Local `.omo` and `.surf` contents remain untracked and are not evidence inputs.
