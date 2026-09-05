# Task 008 verification evidence

## Scope and identities

The user authorized merging the prior work and proceeding to the next task.
PR #7 merged as `bc990a9e2e003ef04999867dc49d7843f3824847`; PR #12 then merged
into main as `32bb4d16893403db6dd4b108d51ec7832980a934`, this task's base.
[ADR 0015](../../adr/0015-explicit-user-memory.md) defines the new memory ingress.

These Git object identities pin the tested implementation independently of
closing documentation edits. Inspect them with `git ls-tree` or compare against
the containing commit's subtrees:

- `crates` tree: `5a2081e38cd38625995d139e6e9c83c981270855`
- `apps` tree: `f60b424ec850d679ee3b01f10bb8d96a731973f1`
- `scripts` tree: `64b5f0541aeb94b888b1a4677a40d3b4d0669bf6`
- `Cargo.lock` blob: `25f9eca6f31e95f5ab43e9572799b4dfdecc246f`

## Exit-criterion audit

| Contract | Inspectable evidence |
| --- | --- |
| Explicit user text becomes durable, scoped context | `kernel/src/memory.rs` resolves an exact input event, requires User/input.received/same-session/task-free provenance, and fixes all node fields. The HTTP command denies unknown fields. `wrong_sources_scope_and_client_authority_never_create_memory` covers model actors, other event kinds, task sources, wrong sessions, malformed IDs, and injected authority. No context append or publication occurs on rejection. |
| Save, correction, and retry agree | `save_correct_retry_and_rebuild_preserve_exact_text_and_one_active_memory` saves Korean text, replaces it, repeats both original and replacement requests, and checks original event IDs, unchanged counts/publications, no resurrection, and one active replacement. Changed replacement intent conflicts. |
| Concurrent requests cannot fork one correction | `concurrent_corrections_and_retries_append_exactly_once` races two corrections of one old memory: one succeeds and one conflicts. Four concurrent identical promotions append exactly one event and return the same event ID. Validation and commit hold the existing context admission mutex. |
| Reads remain bounded and isolated | `memory_and_page_bounds_are_exact_and_paging_does_not_append_or_full_replay` accepts 4,096 UTF-8 bytes and rejects 4,097 before context append. A 101-memory set returns ordered pages of 100 and 1; limits 0 and 101 reject. Listing adds no events or full replay. Other-session reads are empty. |
| User assertions remain exact source text | `memory_listing_rejects_text_that_differs_from_user_evidence_and_repairs_cache_drift` rejects an otherwise valid trusted draft whose memory text differs from its source, excludes unrelated context sharing a short prefix, and rebuilds a deleted cache row without rewriting history. |
| Accepted-but-unprojected is distinguishable from failure | `accepted_projection_failure_is_explicit_and_retry_recovers_without_duplicate` uses a real SQLite trigger. The durable write returns a pending projection outcome, publishes once, and later retries the same identity with no duplicate. Existing trusted context-admission publication/recovery tests also pass after extraction of the shared commit helper. |
| Corrected memories participate in existing retrieval | `corrected_memory_enters_existing_working_set_without_resurrecting_old_text` retrieves the corrected preference through the existing lexical joint working-set API and excludes the superseded text, without adding any event. |
| HTTP preserves the domain outcomes | The daemon's real loopback HTTP test checks unknown authority rejection, 201 creation, 200 retry, 409 conflict, invalid queries, 202 accepted/pending, path-free 500 storage failure, and recovery to the same identity. |
| Users can operate the feature through the actual CLI | `scripts/smoke-user-memory.py` starts built binaries with disposable data. It verifies save, correction, retry, scope isolation, failed promotion after input capture plus explicit recovery, exact UTF-8 bounds, paging, and daemon restart after deleting only the derived cache. It finishes with two matching active memories. |

## Replayed commands

All local commands ran on 2026-09-06 with Rust 1.88.0 on aarch64 macOS.

| Command | Result |
| --- | --- |
| `rtk cargo test -p ditto-kernel -p ditto-daemon --locked` | Passed 69 tests across six suites before the final two domain scenarios were added. |
| `rtk cargo test -p ditto-kernel --test user_memory --locked` | Passed all seven memory domain scenarios; the final generic-prefix assertion is also covered by the canonical gate. |
| `rtk ./scripts/agent-check.sh` | Passed canaries, formatting, strict all-target/all-feature Clippy, 374 unit/integration tests, and 24 compile-fail doctests across 38 suites. |
| `rtk cargo +1.88.0 check --locked --workspace --all-targets` | Passed for all local workspace targets. |
| `rtk cargo build -p ditto-daemon -p ditto-cli --locked` then `rtk proxy python3 scripts/smoke-user-memory.py` | Passed all eight real CLI scenarios on the final implementation. The script uses Python's standard library, loopback, disposable files, and no external service. |
| `rtk git diff --check` and `rtk git diff --cached --check` | Passed. |

Development tests caught that uppercase event ULIDs do not satisfy existing
lowercase canonical context IDs. The memory ID now uses lowercase while
provenance retains the exact original event ID. Strict Clippy also caught an
unnecessary `Ok(...?)` in CLI transport; it was removed before the final gate.

## Practical limits

- Input capture and memory promotion are separate durable commands. CLI errors
  explicitly identify captured input when promotion is unconfirmed; no saved
  memory is claimed for that intermediate state. Identical promotion retries
  are idempotent, not arbitrary repeated `memory save TEXT` calls.
- The local-user API remains the existing trusted loopback ingress. It is not
  a multi-user authorization boundary or a model-facing memory-writing tool.
- `personal` is an ordinary session. Global memory, deletion, automatic
  extraction, model scheduling, and automatic model injection are deferred.
- Listing uses the existing bounded active snapshot, including its 10,000-node
  and cumulative byte limits, before returning at most 100 exact-source items.
  Pages across concurrent edits are fresh snapshots, not one frozen traversal.
- No inference, embedding, credential resolution, additional runtime dependency,
  persistent service, or new store was introduced. Runtime/latency/RSS improvements
  are not claimed. Linux execution is recorded separately by the PR's CI checks;
  local evidence is explicitly macOS. Local `.omo` and `.surf` remain untracked.
