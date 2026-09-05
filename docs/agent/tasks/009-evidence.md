# Task 009 verification evidence

## Tested identity

Task 009 implements [the run contract](009-agent-run.md) under
[ADR 0016](../../adr/0016-explicit-agent-runs.md), on main base `4f5d2ff`.
The final tested source subtrees, before documentation completion, are:

| Object | Git identity |
| --- | --- |
| `crates` tree | `8d6b4841a52b3a5873f9f8962f92b99b3e8b6be2` |
| `apps` tree | `d24673f06223569832621970722fafe5ae2a4f7d` |
| `scripts` tree | `1eefe3da885961d505bd081a42afe34696754b84` |
| `Cargo.lock` blob | `315fa48b93d35d41e506d02c9e9876ef0d6a0e3c` |

## Contract evidence

- `crates/kernel/tests/read_only_turn/agent_runs.rs`: nine tests cover direct
  answers with corrected/scoped memory, read continuation and no-I/O replay,
  four concurrent identical submissions, conflicting retry intent, busy/no-queue
  behavior, scope-safe cancellation, concurrent shutdown drains, interrupted
  admission after restart, panic cleanup and slot reuse, canonical IDs and exact
  16-KiB UTF-8 bounds, corrupted canonical context failing before model I/O, and
  legacy pre-cancellation without lazy context materialization. The complete
  read-only-turn suite passes 48 tests, including historical replay semantics.
- `apps/daemon/src/runs.rs`: adapter tests exercise HTTP 200/202/400/404/409/
  422/429/503, reject client actor/kind/provider/context/lease/task/metadata
  fields, run the actual kernel read/continue path through HTTP, and inspect
  results after reopening with the provider disabled. Default/remote provider
  checks do not load provider credentials.
- The same module's built-CLI smoke launches the actual `ditto` binary against
  the daemon router and deterministic model. It verifies event-driven answer
  waiting, exact retry without new model requests, status, conflict reporting,
  detach, cancellation, and nonzero exit for failed status. The canonical gate
  builds the CLI and explicitly runs this otherwise ignored integration test.
- `apps/cli/src/runs.rs`: chunk-split CRLF parsing and oversized discarded
  payloads demonstrate the 128-byte event-name retention ceiling.
- `crates/event-store/src/lib.rs`: schema-2-to-3 migration, indexed boundary
  query plans without transcript scans or temporary sorting, session and turn
  correlation isolation, reopened results, and append-only triggers. A final
  regression places record-only input before a run in the same task; partial
  `turn_*` indexes and explicit index selection preserve both retry identity
  and ordered boundary lookup.
- `scripts/smoke-agent-run.py`: actual rebuilt daemon/CLI, isolated temporary
  storage and a minimal environment, disabled runs without admission, printed
  recovery identity, missing run inspection/cancellation, record-only input,
  SIGTERM with an open SSE follower, and restart preserving explicit memory.

## Final local commands

On aarch64 macOS, all of the following passed:

```bash
rtk ./scripts/agent-check.sh
rtk cargo +1.88.0 check --locked --workspace --all-targets
rtk cargo build --locked -p ditto-daemon -p ditto-cli
rtk proxy python3 scripts/smoke-agent-run.py
```

The canonical gate passed canaries, formatting, strict Clippy, 388 ordinary
unit/integration tests, 24 compile-fail doctests, and one explicitly invoked
built-CLI smoke: 413 tests across 39 suites. The final code trees above identify
what was tested. Linux CI is independently visible on the associated PR.

## Limits of this evidence

Model and transport behavior is fixture-tested. No paid/live provider request,
provider credential lookup, process capability, scheduler, SSH execution,
automatic model retry on restart, or verified task completion was exercised or
claimed. Status is a projection of trusted terminal events; full journal
verification remains the separate replay API. Cancellation acknowledgement is a
live signal; storage failure or a crash may still leave interrupted work.

The turn uses the existing V1 lexical compiler over a bounded verified snapshot;
production semantic retrieval and automatic transcript/cross-session memory are
deferred. Index plans and parser bounds are not RSS/latency or zero-cost
benchmarks. Event schema 3 adds two indexes and requires a schema-3-aware binary
for rollback; it does not rewrite event payloads. No new runtime dependency
package, database, or service was added. Review was local, not independent
model review.
