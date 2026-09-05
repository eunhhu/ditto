# Task 010 verification evidence

Base: main `aefe9d28be564db7a9fe1205fe0c9f996b2e2ec9` (Task 009 / PR #14).
Branch: `dev/task-010-local-process`.
Contract: [Task 010](010-local-process.md), [ADR 0017](../../adr/0017-bounded-local-sort.md).

## Tested implementation identity

These Git objects identify the code tested locally, independently of subsequent
documentation-only evidence updates:

| Component | Git object |
| --- | --- |
| `crates/` tree | `e5fcce49337a8d49657142c22cbd18858037108a` |
| `apps/` tree | `e330ad06f26e845e3161f3ec7eb1b5358501fc58` |
| `scripts/` tree | `cdf025bf47573a292fd8c939e444bcc6def45a44` |
| `capabilities/` tree | `587656a7bdb5587e2402e2e5227da23f7d7e2935` |
| `Cargo.lock` blob | `40da6ef7fe4f3efb63aa447d53804e58ca74f7b0` |

## Replayable checks

| Contract | Evidence |
| --- | --- |
| Actual fixed process and independent verifier | `crates/artifact-sort/tests/contract.rs`: empty/LF/CR/Unicode, duplicates, inert command-shaped input, exact 64 KiB input, missing/extra/unsorted/unterminated/invalid-unique output |
| Canonical authority before dispatch | same suite: closed raw schema, manifest drift, exact input hash, cross-epoch and expired claims, one-shot consumption, precancellation and byte/line/NUL/UTF-8 bounds |
| Owned process lifecycle | `crates/artifact-sort/src/process.rs` tests: actual child PID cancellation and timeout/reaping, dropped future cleanup, stdout overflow, nonzero exit, and closed child environment |
| Durable public requests | `crates/kernel/tests/sort_runs.rs`: real sort and verified artifact references, identity/conflict/busy/scope, cancellation, concurrent shutdown, unpolled-owner loss/reopen without rerun, prior record-only task notes, corrupt verifier records and changed artifact bytes |
| Shared model/worker admission | existing concurrent agent-run test now also proves a sort cannot enter an occupied model slot; legacy artifact-read suite keeps exactly one selected full manifest despite three installed headers |
| Public trust and placement | daemon `sorts::tests::http_sort_without_provider_rejects_authority_and_remote_routes`: authority-field rejection, scoped lookup/cancel, retry/conflict and absent remote routes without a provider |
| Real CLI with daemon router/kernel/process | `sorts::tests::built_cli_sort_wait_retry_status_and_cancel`: file/unique request, event-followed verified result, status/cancel/retry, changed-option failure and zero model events |
| Production binaries and restart | `scripts/smoke-local-sort.py`: default-disabled provider, sort/unique, status/cancel, exact retry/conflict, nonblocking FIFO rejection, SIGTERM/restart and same-ID no execution |
| Regression and toolchain | canonical gate and Rust 1.88 workspace/all-target check below |

Local macOS/aarch64 commands completed successfully:

```bash
rtk ./scripts/agent-check.sh
rtk cargo +1.88.0 check --locked --workspace --all-targets
rtk cargo build -p ditto-daemon -p ditto-cli
rtk proxy python3 scripts/smoke-local-sort.py
rtk ./scripts/agent-canary.sh
rtk git diff --check
```

The canonical gate passed formatting, strict Clippy, **403 unit/integration
tests + 25 compile-fail doctests + 2 required built-CLI tests = 430 tests across
44 suites**. Both initially ignored CLI tests are explicitly executed by the
gate after building the binary. The initial broad regression exposed a stale
two-header fixture expectation; it was updated to three installed headers while
retaining the assertion that only one full manifest is paged for artifact.read.

Production smoke output recorded one process dispatch, one verified completion,
zero model requests, and no execution for a retry after restart. Process/pipe
failure fixtures are private test-only commands; production has no program or
shell selection seam. Corruption tests deliberately disable SQLite's update
trigger in disposable data; production events remain append-only.

Review was local and focused on canonical inputs, exact claim consumption,
process ownership/drop, bounded pipes and hash verification, completion roots,
shared admission, retry/recovery and default-disabled model behavior. No separate
model review, live model/provider call, provider credential access, or new
service was used in local verification. Linux CI is a separate PR check, not inferred from local
success; its final result is recorded with the PR.

## Limits

This implements one explicit sort profile, not general command execution or
automatic model-to-process dispatch. OS sort is trusted; private scratch and
leases are not an arbitrary-program sandbox. Graceful cancellation and future
drop are covered; daemon SIGKILL/host failure cannot promise managed wall-clock
cleanup, and recovery never reruns that identity. Sort-specific completion does
not certify a broader model goal. No RSS, latency superiority, or zero-overhead
measurement is claimed. No database migration or new third-party dependency was
needed; one capability-owned crate reuses existing workspace dependencies.
