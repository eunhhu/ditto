# Task 013 verification evidence

Date: 2026-09-09. Platform: macOS/aarch64, Rust 1.88.0.
Base: Task 012 main merge `e1771763fbd82ba60b830827cfed0cd436e99ad5`.
Local checks passed. PR/Linux CI are pending; Task 013 is not yet marked complete.

## Tested source identity

| Path | Git object |
| --- | --- |
| `crates` | `91af4c116de4b9145ca9f3b4f4a4f102522b3f53` |
| `apps` | `aadeae4e262a315cb6d56a1981fbd69f38d92852` |
| `scripts` | `02eb437858daec7355cfdd9587317e3faa21a19d` |
| `capabilities` | `587656a7bdb5587e2402e2e5227da23f7d7e2935` |
| `Cargo.lock` | `8becaa9b566177cc2040bf75682bbbc2329d0e8a` |

No dependency, service, runtime language or model adapter was added.

## Commands and results

- `./scripts/agent-check.sh`: passed canary, format, strict Clippy and **476 tests
  across 47 suites**: 446 unit/integration, 25 compile-fail doctests and five
  required actual CLI scenarios. The workspace run ignores those five CLI
  scenarios, then the script explicitly runs each after building the executable.
- `cargo check --locked --workspace --all-targets`: passed with Rust 1.88.0.
- `cargo build --locked -p ditto-cli -p ditto-daemon`: passed.
- `python3 scripts/smoke-agent-run.py`: passed actual disabled-provider ingress,
  memory/SSE shutdown/reopen, one-shot expiry, repeat missed-count advancement
  without child creation, identical retry and durable parent cancellation.
- `python3 scripts/smoke-local-sort.py`: passed one actual process start, one
  verified completion, zero model requests and no execution on restart/retry.

Commands were invoked through RTK with disposable local storage. No credentials
or live paid-provider calls were used. The initial strict gate found an unused
test import; it was removed and the entire gate then passed. Local transcript:
`/tmp/ditto-task013-gate.log` (ephemeral, not a repository artifact).

## Inspectable scenarios

- [Kernel repeat tests](../../../crates/kernel/src/schedule/tests/repeats.rs)
  cover independent anchored runs, pure existing turn replay and terminal
  inspection after restart; 998 missed ordinals in one record followed by one
  currently eligible run; lost ownership before admission and during a run;
  permanent private run reservations; child-only and parent cancellation,
  including the running final child after timetable exhaustion; shared capacity,
  normalized retry identity and scope rejection; invalid time/count/index state;
  late eligibility recheck; the existing timer/slot-release wakeup; and earliest
  due selection across both queue types with repeat expiry while occupied.
- [Storage tests](../../../crates/event-store/src/recurrence/tests.rs) prove
  journal/child/cursor rollback together on rejected claims, including failure
  after the child insert; run reservation collision without cursor advance;
  source/checkpoint drift rejection; deleted-index reconstruction of claims,
  aggregate misses and cancellation; schema-5 one-shot migration without source
  rewriting; exclusive expiry arithmetic with extreme timestamps; and query
  plans using exact/active/source indexes without a temporary sort.
- [HTTP/CLI tests](../../../apps/daemon/src/schedules/tests/repeats.rs) reject
  injected authority/progress/process fields, cross-session access, changed
  retries and every repeat route on a non-loopback listener. The required actual
  CLI scenario accepts a repeat while disabled, reopens enabled with a fixture
  provider, executes its first occurrence once, inspects original child evidence,
  retries without appending events, reopens disabled, cancels the parent and
  preserves the cancelled series and original result across another reopen.

## Limits

This evidence covers finite elapsed-time read-only recurrence. Calendar cron,
indefinite repeats, notification delivery and scheduled effect grants are still
deferred. It establishes no live-model quality, free-inference, RAM or latency
benchmark. At most 100 future intents and no future child pool are retained by
queue selection; startup replay still grows with recorded schedule history.
One kernel owner per directory, SQLite WAL/NORMAL process-restart durability and
the documented wall-clock observation limits apply. An at-most-once claim can
remain interrupted after owner loss. Parent `exhausted` is timetable state;
general model answers remain explicitly unverified.
