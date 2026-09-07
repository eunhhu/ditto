# Task 012 verification evidence

Date: 2026-09-07. Platform: macOS/aarch64, Rust 1.88.0.
Base: Task 011 main merge `6060e83ab53f04098e96ae2fef8bd140e3aa0183`.
Implementation commit: `7347e0cc33e2fd08cbbc0307326a49f3704f1708`.
Linux [CI run 34071827827](https://github.com/eunhhu/ditto/actions/runs/34071827827)
passed both `rust` and `msrv` for that commit. Subsequent completion-document
changes preserve the source identities below. [PR #17](https://github.com/eunhhu/ditto/pull/17)
records the final-head checks and merge state.

## Tested source identity

| Path | Git object |
| --- | --- |
| `crates` | `b99a969f11ea7a49b5b023c5c249e22857ab895f` |
| `apps` | `00faed09d83b4cfce4fd0d6b608a2427330017e0` |
| `scripts` | `02e668b16d3cc891215d077e6bd4dae760cdd1b7` |
| `capabilities` | `587656a7bdb5587e2402e2e5227da23f7d7e2935` |
| `Cargo.lock` | `8becaa9b566177cc2040bf75682bbbc2329d0e8a` |

No dependency, service, runtime language or model adapter was added.

## Commands and results

- `./scripts/agent-check.sh`: passed canary, format, strict Clippy and **457 tests
  across 46 suites**: 428 unit/integration, 25 compile-fail doctests and 4 required
  built-CLI scenarios. The normal workspace run ignores the four built-CLI
  scenarios, then the script explicitly runs and passes each after building CLI.
- `cargo check --locked --workspace --all-targets`: passed with Rust 1.88.0.
- `cargo build --locked -p ditto-cli -p ditto-daemon`: passed.
- `python3 scripts/smoke-agent-run.py`: passed default-disabled rejection,
  existing memory recovery, SIGTERM with an open SSE follower, durable future
  schedule retention across downtime, startup expiry and no requeue on retry.
- `python3 scripts/smoke-local-sort.py`: passed one actual process start, one
  verified completion, zero model requests and no repeated execution after reopen.

Commands were invoked through RTK. Loopback sockets were denied on initial
sandboxed canonical/smoke attempts; the same checks passed with approved local
socket access. No credentials or live paid provider calls were used. Local gate
transcript: `/tmp/ditto-task012-gate.log` (ephemeral, not a repository artifact).

## Inspectable scenarios

- Kernel schedule tests cover restart before dispatch, current memory at execution,
  pure existing turn replay, terminal recovery, claim-before-admission interruption,
  runtime-owner loss, reserved identity rejection, cancellation before/during
  dispatch, exclusive expiry, provider-disabled expiry, one scheduler owner,
  actual timer/admission wakeup, busy-slot release, late-window recheck, bounded
  admission, changed retry/scope rejection and corrupted index/source rejection.
- Event-store tests cover rejected transitions rolling back both journal and
  index, durable claim recovery after deleting the index, indexed exact/queue
  lookups and streaming source replay without a temporary sort. An initial
  query-plan test exposed SQLite choosing a temporary sort for source replay;
  the final query explicitly uses the schedule sequence index and passes.
- Daemon tests reject actor/kind/provider/lease/process/internal identity fields,
  wrong-session lookup/cancel and remote routes. The required real CLI scenario
  submits with a disabled provider, reopens with an injected model, executes once,
  reads the answer, retries without new events, reopens disabled, and cancels a
  second pending schedule. Invalid offset-free dates fail before admission.

## Limits

This completes one-time text requests, not recurring
cron, notification delivery, scheduled process grants or a proven zero-cost/RAM
benchmark. Startup work grows with schedule-event history, with bounded live
materialization; steady-state queue reads are capped at 100 headers. Delivery is
at most once: a crash in the claim/admission gap remains visibly interrupted.
One kernel owner per directory, process-restart durability and the documented
wall-clock observation policy apply. General answers remain unverified.
