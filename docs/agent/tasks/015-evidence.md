# Task 015 verification and offline context/history evidence

## Contract and implementation

[Task contract](015-quality-history-workloads.md),
[harness](../../../scripts/personal-quality.py), and
[regressions](../../../scripts/test-personal-quality.py).

Both profiles use the actual CLI and loopback daemon test executable with its
injected offline driver, the existing durable save/correction and memory-list
paths, abrupt process restart, and the kernel's existing V1 lexical model-context
compiler. The only Rust change is opt-in capsule observation inside the existing
`cfg(test)` driver. `fixture-calls.txt` retains its one-ID-per-call format.
Production logging, providers, endpoints, wire/policy/authority contracts and
completion semantics are unchanged; no ADR or new dependency is needed.

The synthetic facts are `cedar timezone is UTC`, corrected by exact memory ID
to `cedar timezone is KST`; `bananas ripen tomorrow` in the same session; and
`cedar timezone is PRIVATE` in session `elsewhere`. The longer profile adds
`synthetic unrelated pebble {i:06d}` for each i in 0..999 to session `personal`.
The fixed query is `cedar timezone`. Minimal history has the same four saves
and no noise. Each profile performs five unique requests only after reopen and
source-verified paginated listing of all active memories in both sessions.
IDs and timestamps are freshly generated; content, ordering and counts are fixed.

Each driver observation retains the exact serialized model-facing ContextCapsule.
The harness matches every observation to the ordered durable model request and
its exact capsule, independently reconciles the call log, and checks one
corrected node with exact text, User/Asserted/Session metadata and source input.
Exclusion assessment checks IDs and values and rejects extra or duplicate nodes.
Every request must pass correction recall, stale/irrelevant/noise exclusion and
scope isolation. Answers remain `unverified`; no fixture answer is assessed and
no `task.completed` event is permitted.

## RED evidence

Before implementing the harness or driver observation:

- `python3 scripts/test-personal-quality.py` exited 1. Four assessment regressions
  errored with `FileNotFoundError` for the missing `scripts/personal-quality.py`.
  The integration regression initially hit the environment's loopback socket
  restriction (`PermissionError: Operation not permitted`), so that attempt did
  not establish workflow RED.
- With localhost access, `python3 scripts/test-personal-quality.py WorkflowTests`
  exited 1 after actual CLI saves, exact correction, restart and two requests.
  It failed reading the absent `data/fixture-contexts.jsonl`. This establishes
  the missing observation at the real model boundary before implementation.

Local transcripts are retained in ignored `target/task015/red.log` and
`target/task015/red-workflow.log`. The four missing-module errors establish
missing assessment implementation, not a pre-existing runtime retrieval defect.

## Reproduction

Run from the repository root using the installed Rust 1.88 toolchain and cached
dependencies. These settings keep build and temporary files inside the repository:

```bash
mkdir -p target/task015/tmp
export CARGO_NET_OFFLINE=true
export CARGO_TARGET_DIR="$PWD/target"
export TMPDIR="$PWD/target/task015/tmp"
python3 scripts/test-personal-quality.py
python3 scripts/test-personal-baseline.py
python3 scripts/personal-quality.py --history-size 4 --samples 2 --output target/task015/smoke.json
./scripts/agent-check.sh
python3 scripts/personal-quality.py --output docs/agent/tasks/015-quality-history.json
git diff --check
```

The build uses Task 014's exact named Cargo JSON artifact resolver; it never
guesses a `target/debug` executable. The report records repository-relative
artifact paths and SHA-256 for the CLI, production daemon and fixture executable;
only the CLI and fixture are executed by this workload. The production daemon
is built by the reused resolver. Runtime environments are cleared, HTTP is
loopback-only with proxy discovery disabled, and no live provider is selected.
Source hashes include all relevant apps/crates/capabilities/scripts and build
manifests, including uncommitted files. Source/binary hashes are checked again
before report output. Documentation and reports are excluded to avoid recursive
hashes. The existing Task 014 report remains historical Task 014 evidence.

## Verification and measurements

All reproduction commands passed on 2026-09-18. Focused quality regressions:
**8 passed**; existing Task 014 regressions: **8 passed**. The canonical gate
passed canaries, formatting, strict all-target/all-feature Clippy, **490 Rust
tests across 48 suite summaries** (460 unit/integration, 25 compile-fail
doctests, five existing actual-CLI scenarios), both eight-test Python suites,
the Task 014 two-server smoke and Task 015 two-profile smoke. Local logs are
`target/task015/focused.log`, `baseline-focused.log`, `smoke.log`, `gate.log`
and `recorded.log` in that directory. No test or required measurement remains
blocked. The initial sandbox socket denial was resolved by localhost access.

The full default [raw report](015-quality-history.json) was recorded at
`2026-09-18T02:49:37.586427+00:00`, after the gate finished. Its base Git head is
`d1575bfd0f018bdfdee8dcccbdbd2b674d5026dd`; source digests bind the uncommitted implementation.
Environment: `Linux-6.18.39+rpt-rpi-2712-aarch64-with-glibc2.41`, 4 CPUs, reported CPU part
`0xd0b`, 8454029312 bytes system RAM,
Rust 1.88.0, Python 3.14.7, debug/default-feature builds and three capability
packages. Recorded build overrides (`CARGO_BUILD_JOBS`, `CARGO_BUILD_TARGET`,
`RUSTFLAGS`) are unset. No controlled-host or statistical-significance claim is made.

The following values are copied without rounding from the JSON. Raw query
samples and every request's exact capsule, IDs, durable sequence and outcomes
are retained there. For five samples nearest-rank p50 is the third sorted
sample; p95 is the maximum.

| Measurement | Minimal history | Longer history |
| --- | ---: | ---: |
| Unrelated memories | 0 | 1000 |
| Saved memories including superseded | 4 | 1004 |
| Unique queries after restart | 5 | 5 |
| Seed duration, ms | 49.88143197260797 | 14935.015156981535 |
| Initial readiness, ms | 56.243886006996036 | 115.02124997787178 |
| Recovery readiness, ms | 22.49470999231562 | 1029.2690399801359 |
| Recovered memory listing, ms | 13.61192500917241 | 895.3175549977459 |
| RSS after seed, bytes | 19300352 | 21676032 |
| RSS after final query, bytes | 21463040 | 25444352 |
| Restarted process high-water RSS after final query, bytes | 21463040 | 25772032 |
| Query CLI p50, ms (nearest rank) | 62.34989600488916 | 182.13127000490203 |
| Query CLI p95, ms (nearest rank) | 64.24439395777881 | 186.1987269949168 |
| Durable events after seed / final query | 8 / 43 | 2008 / 2043 |
| Storage after seed, bytes | 770072 | 10337744 |
| Storage after final query including fixture logs, bytes | 2234552 | 10450576 |
| Storage after final query excluding fixture logs, bytes | 2232672 | 10448696 |
| Fixture logs after final query, bytes | 1880 | 1880 |
| Durable model requests / independently logged calls | 5 / 5 | 5 / 5 |
| Tool dispatches | 0 | 0 |
| Selected nodes per query | 1, 1, 1, 1, 1 | 1, 1, 1, 1, 1 |
| Serialized capsule bytes per query | 227, 227, 227, 227, 227 | 227, 227, 227, 227, 227 |
| Known external provider spend, USD | 0 | 0 |
| correction_recall | true | true |
| stale_fact_exclusion | true | true |
| irrelevant_exclusion | true | true |
| scope_isolation | true | true |
| noise_exclusion | true | true |
| only_expected_context | true | true |

There were zero model/tool calls during seed or recovery. All active memories
were recovered and listed without new events. The five query requests in each
profile correspond one-to-one to five model events, five driver-call lines and
five exact context observations. All ten outcomes are assessed from those
capsules; terminal model answers remain unverified. The unchanged 227-byte
selected capsule does not imply constant retrieval time or process memory.

Final review checked the complete source/document diff, opt-in cfg(test)
placement, unavailable fields, offline/cleared runtime configuration, cleanup,
report/table consistency and measurement identity. All 113 source digests and
three binary digests match the final measured files. The canonical gate's
Task 014 compatibility run passes; the historical Task 014 report is not
relabeled as a measurement of this branch. The quality SIGTERM regression
observes a real server PID, requires exit 143, verifies the child is reaped and
the temporary store removed, and rejects a leftover output report.

## Interpretation and limits

The capsule assessment measures this synthetic correction/exclusion contract.
It establishes no general or model-answer quality, semantic retrieval,
cross-session recall, automatic transcript memory or performance superiority.
The explicit session boundary is tested by exclusion of a matching private
value; the experiment does not enable cross-session memory. It does not
complete the broader v0.1 milestone or evaluate learning or growing catalogues.

Latency includes CLI spawn, HTTP, kernel/storage work, fixture output and terminal
JSON. No warmup is discarded. Recovered memory listing precedes queries and is
timed separately. Capsule capture adds test-only serialization and file I/O;
these latencies cannot be treated as isolated production overhead. Profiles run
sequentially on one uncontrolled host, without a performance threshold.

Readiness includes a health probe polled every 10 ms. Seed duration includes
all real CLI saves and the exact correction. RSS/high-water RSS cover the server
process only, exclude CLI children and reset with the restarted process. File
sizes are live logical sizes, including SQLite/WAL/SHM and separately identified
fixture logs; they are neither allocated disk blocks nor retained production
storage alone. All temporary stores and servers are removed after measurement.
Recovery covers process kill/reopen, not power/host loss.

Model-answer quality, fixture/live token usage, live-equivalent cost, first
useful progress and isolated model/tool/Ditto timing are null/unavailable with
reasons. External provider spend is known zero; total operating cost is unknown.
No credential use, live provider call, download, commit, push or PR is part of
this slice's execution.
