# Task 014 evidence

Contract: [human task inspection and offline baseline](014-status-baselines.md).

## Source and scope

Development branch: `dev/task-014-status-baselines`. The report was generated
before the task commit, so it records the base Git head plus SHA-256 identities
for source files, lockfile and measured binaries;
documentation is not included in the code identity. No core crate, protocol,
policy, event schema, kernel, dependency, service or production provider changed.

The CLI presentation adapter opts into `--human`; default stdout JSON and
existing exit semantics remain. Daemon additions are compiled only under
`cfg(test)` and expose only existing routes. The Python harness uses the standard
library, offline locked builds, cleared server/CLI environments, disposable
storage and loopback HTTP. It never selects a live provider.

## Regression evidence

- The user reported a green pre-change canonical gate.
- RED: `cargo test --offline --locked -p ditto-cli --test human` passed the
  default-JSON check and failed four human-output tests because `--human` was
  absent. `python3 scripts/test-personal-baseline.py` failed because the harness
  did not exist.
- GREEN: the same focused CLI target passes eleven tests. They cover independent
  failed model/verified sort and unverified model/denied sort outcomes, pending
  schedules, requested versus terminal cancellation, exhausted repeats with
  interrupted children, exact permission/recovery instructions, unknown states,
  ANSI/OSC/C1/CR/bidi controls and identities printed before uncertain transport.
  Final-review RED checks also reproduced CR/bidi leakage in parser diagnostics
  and reflected environment values in help. Parser errors are now escaped with
  exit code 2 retained; help hides the environment value and help/version remain
  successful stdout output. The ninth regression covers both paths.
- `python3 scripts/test-personal-baseline.py`: eight tests pass; absent RAM and
  samples stay null, missing usage/cost stay unknown, offline spend and no-model
  work are distinguished, dispatch counts exclude intent, and percentile/raw
  sample semantics are checked.
- Independent-review RED regressions reproduced API userinfo in recovery output
  (including default-JSON submission stderr), a shell recovery command exiting
  with parser code 2 for a leading-hyphen session, hardcoded executable paths,
  scheduled work finishing before restart, and daemon/fixture children surviving
  harness SIGTERM. GREEN covers sanitized recovery targets and unchanged JSON,
  actual shell/parser execution with quoted sessions, named Cargo JSON artifacts
  in an alternate target directory, due work held by a test-only disabled
  scheduler driver until restart, and exit 143 with both child and store removed.
- Initial environment failures were an older system Rust and sandbox-denied
  loopback binds. Verification used the already-installed Rust 1.88 toolchain
  with loopback permission, without downloading anything. The first canonical
  attempt then found Clippy's `unused_io_amount` in the HTTP test fixture; the
  read now checks its length, and the focused suite passed again.

## Reproduction and measurement

Commands run from the repository root with the installed Rust 1.88 toolchain on
`PATH` and `CARGO_NET_OFFLINE=true`:

```bash
cargo test --offline --locked -p ditto-cli --test human
python3 scripts/test-personal-baseline.py
./scripts/agent-check.sh
python3 scripts/personal-baseline.py --output docs/agent/tasks/014-baseline.json
git diff --check
```

All commands above passed on 2026-09-17. The canonical gate passed canaries,
formatting, strict all-target/all-feature Clippy and **489 Rust tests across 48
suite summaries**: 459 unit/integration tests, 25 compile-fail doctests and the
five required existing actual-CLI scenarios. It also passed eight Python
accounting/artifact/process tests and both servers' two-request offline baseline
scenarios.
The test-only long-lived fixture server is intentionally ignored by ordinary
Rust test runs and explicitly launched/terminated by the Python harness.
Local gate transcript: `/tmp/ditto-014-blockers-gate.log` (ephemeral).
Final artifact checks passed: `git diff --check`, all 110 recorded source
digests and all three measured binary digests. The executable paths used for
both hashing and execution come from exact named Cargo JSON artifacts, including
inherited alternate target directories. No generated Python cache is retained.

The [recorded report](014-baseline.json) contains all raw samples, source/binary
digests and recovery assertions. Base Git head:
`786a0587bdfed175579289309f9a938bb63f7424`. Environment: Linux/aarch64,
`6.18.39+rpt-rpi-2712`, four CPUs (reported CPU part `0xd0b`), 8,454,029,312
bytes system RAM, Rust 1.88.0, Python 3.13.5, debug/default-feature builds,
three existing capability packages and one second of idle observation.
Each server starts with fresh storage and performs 12 repeated requests on
`pear\napple\npear`, verifying exact unique output `apple\npear\n`. Subsequent
recovery scenarios are accounted separately from that repeated workload.

| Measurement | Production daemon, provider disabled | Fixture-backed test server |
| --- | ---: | ---: |
| Initial readiness, ms | 23.66 | 22.25 |
| Two recovery readiness samples, ms | 22.25 / 22.23 | 43.10 / 43.09 |
| Startup RSS, bytes | 18,726,912 | 18,661,376 |
| Idle RSS, bytes | 18,939,904 | 18,726,912 |
| RSS after first / twelfth request, bytes | 20,758,528 / 21,610,496 | 21,741,568 / 22,282,240 |
| High-water RSS through repeated phase, bytes | 21,610,496 | 22,282,240 |
| CLI end-to-end p50 / p95, ms | 55.01 / 59.63 (sort) | 74.99 / 142.78 (model-sort) |
| Repeated-phase model requests / tool dispatches | 0 / 12 | 24 / 12 |
| Whole-workload model requests / tool dispatches | 0 / 13 | 29 / 14 |
| Startup and idle model/tool calls | 0 / 0 | 0 / 0 |
| Known external provider spend, USD | 0 | 0 |
| Live-equivalent model cost, USD | 0 (no model requests) | unknown |

The 29 fixture model requests match 29 independently logged driver calls. All
12 repeated sorts in each run passed their exact output contract. Restart
preserved original results and repeat cancellation; exact retries appended no
events. The production-disabled schedule expired with no model call. The
fixture first process has its scheduler driver disabled while explicit runs
remain enabled; only the restarted process enables scheduled work. The
report records zero pre-shutdown claims through sequence 228, then two claims
and two model requests at sequences 229, 233, 237 and 241, reconciled with two
new independent driver calls. Fixture one-shot and repeat work therefore
dispatched after restart; a blocked model request became interrupted after
process loss and did not retry. Full-workload on-disk
data at inspection was 2,884,142 bytes (production) and 5,111,395 bytes (fixture),
including SQLite/WAL/projection and fixture call-log data; it is separate from
RAM. Timing samples are observational, with no controlled-host or performance
superiority claim, and the two workloads must not be subtracted to infer overhead.

## Inspectable scenarios and limits

[CLI regressions](../../../apps/cli/tests/human.rs) use the actual executable and
controlled HTTP responses, including failures that would be hard to time
reliably with a real worker. The
[baseline harness](../../../scripts/personal-baseline.py) runs both the real
production-disabled daemon and the
[injected test server](../../../apps/daemon/src/baseline.rs) through the actual
CLI. It checks bounded OS sorts, model continuation with a separate verified
sort, exact retry without new events, all four human start/status paths, disabled
schedule expiry, enabled schedule/repeat dispatch after restart, durable repeat
cancellation and abrupt owner-loss interruption without automatic retry.
The injected driver's own call log is reconciled with durable model requests.

This is a small debug-build, fixed-catalogue baseline, not v0.1 readiness,
long-use quality, free inference, zero total cost or a comparative benchmark.
Linux RSS/high-water readings cover the server process only, excluding CLI/sort
children. Startup timings include health polling; latencies include CLI startup,
HTTP, durable work and terminal output. No isolated model/tool timers or
Ditto-only overhead subtraction are available. Model usage, live-equivalent
cost and quality are unavailable for the fixture; null preserves that fact.
Known zero external provider spend applies only to the offline workload.
Recovery tests cover abrupt process loss with SQLite WAL, not host/power loss.
Standalone sort's existing status response omits input/mode; human output states
that limitation rather than deriving a permission from model text.

General approval fulfillment, durable grants, new endpoints, arbitrary process
execution, web/TUI, notifications, providers, embeddings and self-improvement
remain deferred. The benchmark performed no external provider, credentialed API
or non-loopback write; repository delivery is tracked separately in Git and the PR.
