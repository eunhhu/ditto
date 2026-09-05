# Task 007 verification evidence

## Implemented range

- Base after product-intent clarification:
  `664dc03b9d1bb99a850d63e2264b39be9b27378f`
- Implementation, ADR, and regression tests:
  `a2efcd3d073ea6eaa3411712e46eaff5e9d8a796`
- Tested implementation tree:
  `47c94e7086b50dc714352300d5534316d9586f8d`
- Governing decision:
  [ADR 0014](../../adr/0014-capability-package-headers.md)

The closing documentation commit records verified task state and this evidence.
The branch is stacked on Task 006; it does not require merging or modifying
Task 006's open pull request. Product guidance remains
[the user's personal-agent intent](../../product.md).

## Exit-criterion audit

| Contract | Implementation | Regression evidence |
| --- | --- | --- |
| Startup/search does not load full bodies from generated packages | `CapabilityHeader` contains identity, lifecycle, retrieval/card fields, the historical candidate-byte count, and an exact source-byte digest. `ManifestSource::File` retains only a path. Search uses borrowed header fields. | `startup_and_search_are_cold_and_selected_bodies_are_not_retained` checks zero body reads before paging, one selected read, unchanged retained bytes, and no cached fallback after deletion. Its body includes a 400,000-byte verification string while startup bytes and accounted retained header data are each below 4,096 bytes. |
| A growing collection of unused packages does not cause full-body loading | Discovery is bounded and the catalogue has no accumulating full-manifest cache. | `large_header_catalogue_never_reads_unused_bodies_and_keeps_order` installs 1,000 headers, removes 999 unselected bodies, verifies ordered retrieval with zero full-body reads, and pages one selected body. |
| Headers do not acquire invocation authority | Selected paging reads bounded bytes, hashes once, parses/validates the full manifest, and compares the complete header projection. Existing live epoch/schema validation remains mandatory. | `body_drift_and_header_contradictions_fail_page_in` rejects a changed digest and a contradictory header with an otherwise valid body digest. `invalid_header_version_digest_accounting_and_unknown_fields_are_rejected` covers malformed header metadata. |
| Historical retrieval semantics and explicit compatibility remain | Legacy/V2 search retain their entry points, ordering, filters, complement behavior, and cumulative candidate accounting. Headerless loading reads/validates one body, derives the header and digest, and discards the body. In-memory insertion explicitly retains the caller-owned manifest. | `legacy_and_header_packages_preserve_search_and_charge_the_same_candidate_work` compares header projections and search outputs. Existing ranking, hard-filter, negative-example, complement, provider-budget, lifecycle-capacity, and exact V2 bound tests pass. |
| Inactive packages remain cold | Inactive headers neither resolve their own complement links nor page full bodies. Active links must resolve to installed headers. | `inactive_packages_do_not_parse_bodies_resolve_complements_or_page` uses retired/quarantined headers, malformed bodies, and unknown inactive links. `active_unknown_complements_and_duplicate_ids_fail_catalogue_load` checks the active catalogue contract. |
| Filesystem work and metadata are bounded | Descriptor-relative Linux/macOS traversal uses no-follow directory/file opens, streaming directory entries, regular-file checks, and bounded reads. The configured root parent is resolved once; paging uses those resolved paths. | `root_descendant_and_metadata_symlinks_are_rejected_including_later_replacements` checks initial and post-load replacement cases. `non_regular_metadata_is_rejected_without_blocking` rejects a FIFO. `discovery_depth_is_checked_at_n_and_before_n_plus_one` drives real 16/17-level trees. |
| Limits reject overflow without a partial catalogue or manifest | Fixed maxima are 65,536 entries, 16,384 packages, 64 KiB/header, 1 MiB/body, 64 MiB aggregate startup input, and 16 MiB accounted retained header data, with checked charging. | `exact_header_and_manifest_byte_limits_reject_n_plus_one` reads actual exact-size and oversized files. `fixed_aggregate_envelopes_accept_n_and_leave_counters_unchanged_at_n_plus_one` covers checked aggregate arithmetic and overflow. `traversal_respects_previously_consumed_entry_and_package_budgets` drives real traversal at the final available entry/package and rejects the next one. The aggregate tests use preconsumed budgets, not a claimed full 16,384-package benchmark. |
| Bundled metadata is reproducibly packaged | The public generator reads a bounded no-follow manifest and returns JSON. The example writes stdout; loading never writes headers. Both bundled packages include generated headers. | `bundled_headers_match_the_generator` compares checked-in JSON with fresh generation from each bundled manifest. |
| Existing turns and pure replay retain their contracts | The artifact turn pages before model invocation. A page failure records exactly `installed artifact.read package could not be verified` as a path-free capability-contract failure. Missing/inactive packages remain unavailable. | All 39 read-only-turn tests pass. `changed_selected_package_fails_before_model_and_replays_without_package_io` observes no driver request or execution/completion event, deletes the package, replays the failure without package I/O, and rejects a forged failure message. The successful two-request test observes zero startup body reads and one selected page. |

## Commands and results

Commands ran from the repository root on 2026-09-06, with Rust 1.88.0 on
`aarch64-apple-darwin`.

| Command | Result |
| --- | --- |
| `rtk cargo test -p ditto-capability -p ditto-kernel --locked` | Passed 129 tests across seven suites before the final three regression tests were added. Those final tests are included in the canonical gate below. |
| `rtk ./scripts/agent-check.sh` | Passed: tracked canaries, formatting, strict workspace/all-target/all-feature Clippy, 366 unit/integration tests, and 24 compile-fail doctests across 37 suites. The final run includes the single-hash paging refinement. |
| `rtk cargo +1.88.0 check --locked --workspace --all-targets` | Passed on the final implementation for all workspace crates and targets available on the local host. |
| `rtk git diff --check` and `rtk git diff --cached --check` | Passed. |
| [PR #12](https://github.com/eunhhu/ditto/pull/12), Linux Actions run [`33983282889`](https://github.com/eunhhu/ditto/actions/runs/33983282889), head `50c9474829203a0a05937f94658c8e7368c3bed2` | Passed both the `rust` repository gate and `msrv`. This head adds only the local evidence/task-closure documents to the implementation commit above. |

Development verification found that rejecting all ancestor symlinks also rejected
the normal macOS temporary-directory alias. The implementation and ADR now
resolve the configured root's parent once and still reject symlinks at the root
or below it. The focused suites and final gate passed after that correction.
Strict Clippy also caught three needless borrows during the retrieval refactor;
they were removed and the final gate is clean.

## Practical boundaries

- These counters demonstrate avoided reads and retained metadata, not measured
  RSS, allocation counts, wall-clock latency, or a zero-cost result. Retained
  accounting includes owned header fields/capacities, source paths, and ID-index
  entries; it excludes outer collection capacity and allocator overhead.
- Only the aarch64 macOS Rust target was installed for local checking. The
  separate Linux CI run above verifies the implementation on Ubuntu with Rust
  1.88. Devin's status said its full review was skipped because its trial had
  expired and no credits remained; a green status is not an independent model
  review, and no such approval is claimed.
- Package roots remain trusted installation input. Headers are not signatures,
  and unused bodies are not integrity-audited at startup. Reload after explicit
  header regeneration; there is no hot reload or daemon-managed header cache.
- Headerless packages have one bounded compatibility read at startup. Explicit
  in-memory insertion retains its supplied manifest and has no source-file
  digest; it is not evidence of file-backed cold loading.
- No live application provider/model call, credential resolution, capability worker,
  scheduler, SSH, paid inference, or completion-verifier feature was added or
  executed. Tests use local temporary files, FIFOs, symlinks, and existing local
  fixtures. External repository operations were the branch push, draft PR, and
  checks. Local `.omo` and `.surf` state remains untracked and is not evidence.
