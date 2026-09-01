# Task 004.1 verification evidence

## Reviewed range

- Contract commit:
  `063e6f259432ea3a5143225ee91dc17cd52f3810`
- Implementation commit:
  `bf58d7a3a001c0505460919a61fa6cc722dc5269`
- Implementation tree:
  `3c38444eaad0e5065888edcea4e5dc4f6430174a`
- Governing decision: [ADR 0011](../../adr/0011-retrieval-resource-envelope.md)

The closing commit that contains this manifest adds the maximum-size lazy
generator regression and records the final handoff/frontier state; inspect it
with the manifest in one tree.

The reviewed range changes no event wire version, makes no external or model
call, and does not connect an effectful executor. The event spine remains the
durable authority; the projection and its process-local verification proof are
rebuildable derivatives.

## Exit-criterion audit

| Contract claim | Inspectable implementation | Regression evidence |
| --- | --- | --- |
| One fixed cumulative request envelope | `RetrievalWorkBudget` is created once in `DittoKernel::retrieve_working_set` and passed through query construction, verified context projection/ranking, and capability ranking. Its five maxima are fixed constants and every charge uses checked addition. | `cumulative_work_budget_accepts_each_exact_maximum_and_rejects_n_plus_one`, `provider_budget_is_reserved_before_an_external_call`, `context_ranking_shares_aggregate_and_provider_budgets_before_work_escapes`, and `capability_ranking_shares_provider_budget_and_keeps_only_bounded_roots`. |
| No unbounded document retention | Context and capability paths construct, charge, score or embed, and drop one document at a time while retaining only their requested top-K values. | `maximum_size_ten_thousand_candidate_generator_stops_at_the_shared_byte_budget` lazily generates 10,000 maximum-size context candidates, reaches the typed candidate-byte ceiling, and proves that document, lexical, and provider work remains zero. The capability top-K test retains one root from 1,000 eligible manifests without retaining retrieval documents. |
| Lifecycle-active capacity | Projection schema 2 filters superseded, disputed, not-yet-valid, and expired context rows before the active candidate limit. Capability manifests default to active and explicitly support active, retired, and quarantined lifecycle states. | `inactive_history_above_candidate_limit_does_not_block_active_snapshot` and `retired_and_quarantined_manifests_do_not_count_or_page_into_working_sets`. |
| Verified snapshot authority and delta verification | `DerivedContextSnapshot` has no candidate-consuming conversion. `VerifiedContextSnapshot` is produced only after process-local source verification or verified checkpoint deltas and is the type consumed by the kernel ranking path. Kernel open performs the full rebuild. | `verified_snapshot_reuses_one_full_replay_and_advances_by_delta`, `repeated_working_sets_reuse_startup_verification_and_advance_by_delta`, and the cache-row/edge/collision repair tests. |
| Validation precedes provider work | `RetrievalScope`, `SessionId`, `TaskId`, and `ContextNodeId` reject non-canonical values. `SearchContext::validate` checks bounds, uniqueness, runtime completeness, and preferred-placement membership before shared-query embedding. | `working_set_identifiers_are_bounded_and_canonical_at_admission`, `search_context_is_bounded_canonical_and_validated_before_document_calls`, and `invalid_scope_and_search_context_are_rejected_before_provider_io`; the provider call log stays empty. |
| Private local SQLite and replayable CI canary | Event and projection SQLite opens reject symlink targets and symlink parents, validate current-user ownership on Unix, set data directories to `0700`, and set present database-family members to `0600`. `scripts/agent-check.sh` invokes the tracked canary first. | `sqlite_family_is_private_regular_and_owned_by_the_effective_user`, `sqlite_open_rejects_database_and_parent_symlinks`, and the canonical gate below. |

Compile-time separation is additionally enforced by the kernel API: there is no
kernel ranking entry point accepting `DerivedContextSnapshot`.

## Replayed commands and verdicts

All commands ran from the repository root on 2026-09-01.

| Command | Verified result |
| --- | --- |
| `rtk ./scripts/agent-check.sh` | Passed: tracked canary, formatting, strict workspace Clippy, and 326 tests across 35 suites. |
| `cargo test --locked --workspace --all-features` (invoked by the canonical gate) | Passed: 326 tests across 35 suites. |
| `rtk cargo test -p ditto-context --locked maximum_size_ten_thousand_candidate_generator_stops_at_the_shared_byte_budget` | Passed: the selected regression test passed with 48 other unit tests filtered. |
| `rtk cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Passed with no warning or error. |
| `rtk cargo +1.88.0 check --locked --workspace --all-targets` | Passed for every workspace crate and target. |
| `rtk git diff --check` | Passed. |

Focused regression runs passed 14 retrieval tests, 27 capability tests, 51
context tests, 36 projection tests, 8 event-store tests, and 15 durable-kernel
projection/working-set tests. No network, credential, model, or billable
embedding operation ran.

## Reproducibility identities

- `Cargo.lock` blob:
  `70aef439832b641f51177d5214effef73374b675`
- `scripts/agent-canary.sh` blob:
  `1a66fe81f2d8bb8caf5e3bcae547d72b2b727ac8`
- `docs/adr/0011-retrieval-resource-envelope.md` blob:
  `e9fc54bda676f6322297de1d4bdde7fb1cb9adfc`
- `crates/context/src/lib.rs` blob:
  `34c39b41b334a6954b84814519c6eb3026439596`
- RTK: `0.46.0`
- Cargo: `1.88.0 (873a06493 2025-05-10)`
- Rust compiler: `1.88.0 (6b00bc388 2025-06-23)`
- SQLite CLI: `3.51.0 2025-06-12`

Local `.omo` and `.surf` contents are neither evidence nor source inputs and
are intentionally not tracked.
