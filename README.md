# Ditto

**A local-first personal general-purpose agent.**

Ditto is being built to keep personal AI work effective, responsive, and
resource-efficient as memories, capabilities, schedules, and experience grow.
Its product goals are lower memory overhead, focused context, reliable memory
and scheduled work, and improvements that earn their ongoing cost. These are
goals to measure, not performance claims already established by the foundation.
**Zero cost, zero overhead** is a primary design premise, including development
effort and future technical debt converging toward zero.

> Context is compiled. Capabilities are paged. Effects are leased. Improvements
> are promoted.

Its semantic microkernel lets frontier models retain strategic freedom while
the runtime controls context, capabilities, side effects, persistence,
verification, and long-lived execution. The personal agent is the product;
the microkernel is its implementation architecture.

See the [product intent](docs/product.md) for the intended user experience,
long-term efficiency goals, and proposed evaluation criteria.

## Current state

The executable foundation includes:

- a schema-versioned, DB-enforced append-only SQLite event spine;
- typed public command ingress with kernel-owned actor and event kind;
- subscribe-first, high-water-bounded, paginated SSE replay and lag recovery;
- SHA-256 content-addressed artifacts with private storage and verified reads;
- compact capability package headers, bounded no-follow discovery on Linux/macOS,
  selected full-manifest verification, strict runtime search, and bounded
  execution epochs; headerless packages retain a bounded compatibility path;
- typed Context IR with provenance validation, trusted compiler directives, and
  locally derived token cost;
- kernel-only trusted admission of session/task context nodes as fixed
  `context.node.recorded` events in the canonical event spine, plus a separate,
  checkpointed and rebuildable `context-projection.db` cache;
- explicit session memory through CLI/HTTP: save exact user text, inspect current
  memories, and correct an active memory with idempotent input promotion;
- one bounded V2 `TaskQuery` shared by read-only joint context/capability
  working-set retrieval, with production lexical ranking and an explicit
  injected embedding seam for tests and explicit local composition;
- orthogonal effect profiles and fail-closed lease primitives;
- versioned provider-neutral model IR, a closed OpenAI Responses adapter, and an
  injected-driver read-only artifact continuation loop;
- explicit CLI/HTTP model runs with current session context, durable retry
  identity, cancellation, and restart inspection;
- explicitly requested local artifact sorting through a one-shot process lease,
  bounded pipes/lifetime, cancellation, and independent line-contract verification;
- model-directed sorting of one explicitly attached file, with separate permission
  for deduplication and verified output inspection after model failure or restart;
- one-shot read-only schedules with durable claims, bounded timer/event wakeup,
  visible expiry/cancellation and restart inspection without automatic retry;
- repository-native instructions for long-running coding agents.

Recurring schedules, scheduled effect grants, additional model
providers, general effectful model-tool continuation, SSH, a production embedding
worker/cache, authenticated remote gateways, additional completion verifiers, and the
improvement compiler are still deferred. They are not represented by fake
success paths.

## Quick start

Requires Rust 1.88 or newer.

```bash
# Terminal 1
cargo run -p ditto-daemon -- \
  --data-dir .ditto \
  --capabilities-dir capabilities \
  --bind 127.0.0.1:8787

# Terminal 2
cargo run -p ditto-cli -- ping
cargo run -p ditto-cli -- input "hello from ditto" --session local
cargo run -p ditto-cli -- events --session local
cargo run -p ditto-cli -- capabilities "run a command on another computer"

# Explicit memories default to the persistent personal session.
cargo run -p ditto-cli -- memory save "I prefer afternoon meetings"
cargo run -p ditto-cli -- memory list
# Use an ID returned above to correct that memory:
# cargo run -p ditto-cli -- memory save "I prefer morning meetings" --replaces MEMORY_ID

curl -N 'http://127.0.0.1:8787/v1/stream?after_seq=0'
```

The daemon refuses non-loopback binding without the explicitly unsafe
`--allow-unauthenticated-remote` escape hatch. That flag does not add
authentication; use loopback until an authenticated gateway exists.

To answer requests, explicitly start the daemon with `--provider openai` and
provide `OPENAI_API_KEY` through its environment. This selects the existing
closed `gpt-5.6` adapter and may incur provider charges. The default is
`--provider disabled`; input recording, memory operations, and schedule maintenance
make no model calls. With an enabled provider, startup can dispatch previously
accepted due schedules inside their start windows. Model execution requires loopback even with the
remote escape hatch.

```bash
cargo run -p ditto-cli -- run "What is my meeting preference?"
# run prints the request ID before submission, then waits on durable events.
# Use --detach to return after acceptance, or --request-id ID for an exact retry.
cargo run -p ditto-cli -- run-status REQUEST_ID
cargo run -p ditto-cli -- run-cancel REQUEST_ID
```

Runs default to the `personal` session. Relevant current session memory is
compiled into context; prior conversation text is not automatically reinserted.
The model can answer directly or read an already-rooted, same-scope artifact.
An explicit attachment also permits one bounded sort:

```bash
cargo run -p ditto-cli -- run "Sort this list and explain the result" --sort-file list.txt
# Add --allow-deduplicate to also permit removing exact duplicate lines.
```

The model receives an artifact reference and the permission, with file content
available through a bounded read when needed. It cannot sort another artifact,
deduplicate without permission, or dispatch twice. `run-status` includes a
separate `sort` result: verified text remains inspectable even if the subsequent
model answer fails. Retry identity includes the exact file and permission.
General file/process tools are not connected yet. One run executes at a time;
other immediate requests are rejected without queuing. An interrupted run is inspectable and
never automatically restarted. A model answer remains `unverified`.

Local file sorting works with the default provider-disabled daemon on Linux and
macOS (`/usr/bin/sort` required):

```bash
cargo run -p ditto-cli -- sort list.txt --unique
cargo run -p ditto-cli -- sort-status REQUEST_ID
cargo run -p ditto-cli -- sort-cancel REQUEST_ID
```

The CLI reads a regular UTF-8 file of at most 64 KiB / 4096 lines. Output is JSON
with sorted text and an immutable artifact reference; the original file is
unchanged. Ordering is by bytes, LF separates lines, CR remains data, and a
nonempty last line gains LF. `--unique` removes exact duplicate lines. The
process uses a cleared environment and private scratch, runs for at most five
seconds while owned, and shares the single execution slot with model runs.
`verified` means the exact line ordering and multiplicity contract passed, not
that a broader model goal was completed. Same-ID retries do not rerun work.
This closed profile has no shell or caller-selected executable. Model dispatch
requires the explicit per-run attachment above; general process profiles remain
future work.

One-time future requests use the same agent and current session memory. Choose
future timestamps with explicit offsets; the second timestamp is the exclusive
latest start, at most 24 hours after the due time:

```bash
cargo run -p ditto-cli -- schedule "Summarize my meeting preferences" \
  --at "2026-09-08T09:00:00+09:00" --expires "2026-09-08T10:00:00+09:00"
cargo run -p ditto-cli -- schedule-list
cargo run -p ditto-cli -- schedule-status REQUEST_ID
cargo run -p ditto-cli -- schedule-cancel REQUEST_ID
```

Scheduling prints the request ID before submission and returns after durable
acceptance. Identical `--request-id` retries retain the original attempt. Up to
100 pending requests can wait without loading prompts or calling a model. The
default disabled provider retains them until expiry; status explains what they
are waiting for. An enabled provider dispatches due work when the shared run slot
is free. A missed window becomes `missed`, and an uncertain claimed attempt after
restart becomes `interrupted`; neither retries automatically. Completed answers
remain `unverified`, with the original run result available through status.

This first schedule contract supports one-time text requests and scoped artifact
reads. Recurrence, sort permissions and notification delivery remain future work.
See [the time, restart and delivery contract](docs/adr/0019-one-shot-scheduled-runs.md).

## HTTP surface

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/health` | Liveness, durable count, and latest sequence |
| `POST` | `/v1/commands/input` | Submit user input; kernel assigns event authority |
| `POST` | `/v1/commands/memory` | Save an existing same-session user input, optionally replacing an active memory |
| `GET` | `/v1/memories` | Inspect current session memories with an ID cursor |
| `POST` | `/v1/commands/run` | Admit one model run, optionally permitting one attached-file sort |
| `GET` | `/v1/runs` | Inspect a run by session and request ID |
| `POST` | `/v1/commands/run/cancel` | Signal cancellation of the matching live run |
| `POST` | `/v1/commands/schedule` | Durably schedule one future read-only request; loopback only |
| `GET` | `/v1/schedules` | Inspect schedule and original run by session/request ID; loopback only |
| `GET` | `/v1/schedules/pending` | List pending schedules in one session; loopback only |
| `POST` | `/v1/commands/schedule/cancel` | Cancel pending or active scheduled work; loopback only |
| `POST` | `/v1/commands/sort` | Explicit local sort with exact request identity; loopback only |
| `GET` | `/v1/sorts` | Inspect verified sort output by session and request ID; loopback only |
| `POST` | `/v1/commands/sort/cancel` | Cancel the matching sort; loopback only |
| `GET` | `/v1/events` | Query one durable event page |
| `GET` | `/v1/stream` | Replay all pages through a high-water mark, then follow |
| `GET` | `/v1/capabilities` | Catalogue-level capability card search |

There is intentionally no public arbitrary event-append endpoint.

Memory saving uses the existing input event and context admission. Text is
bounded to 4 KiB, preserved exactly as recorded, and never inferred by a model.
Use `--session NAME` for an isolated set and `memory list --after-id ID` to
continue a page. If input capture succeeds but memory saving fails, the CLI
reports the input ID for `memory from-input INPUT_ID` recovery. A pending
projection is reported separately from the accepted durable write. See the
[memory contract](docs/specs/event-protocol.md#explicit-user-memory).

## Architecture

```mermaid
flowchart TB
    UI[Web · CLI · Gateways · ACP] --> CMD[Typed Commands]
    CMD --> KERNEL[Semantic Agent Microkernel]
    KERNEL --> STORE[(Append-only Event Spine)]
    STORE --> PROJ[(Rebuildable context-projection.db)]
    KERNEL --> ART[Content-addressed Artifacts]
    KERNEL --> CTX[Context Compiler]
    PROJ --> CTX
    KERNEL --> PAGER[Capability Pager]
    KERNEL --> MODEL[Frontier Model Drivers]
    MODEL --> EXEC[Canonical Invocation]
    EXEC --> POLICY[Effect Firewall]
    POLICY --> WORKERS[Lazy Isolated Workers]
    STORE --> CLIENTS[Unified Replay/Follow Stream]
    STORE --> IMPROVE[Evidence-gated Improvement]
```

- **The model owns intent, strategy, and judgment.**
- **The harness owns context, capabilities, effects, persistence, and execution
  lifetime.**

See [`docs/architecture.md`](docs/architecture.md).

## Development

```bash
./scripts/agent-check.sh

# End-to-end memory verification with disposable local data:
cargo build -p ditto-daemon -p ditto-cli --locked
python3 scripts/smoke-user-memory.py
python3 scripts/smoke-agent-run.py
python3 scripts/smoke-local-sort.py
# Actual CLI + daemon router + deterministic model fixture, without API calls:
cargo test -p ditto-daemon built_cli_run_wait_retry_status_and_cancel -- --ignored
cargo test -p ditto-daemon built_cli_model_sort_permission_retry_and_disabled_status -- --ignored
```

Long-running Codex or other coding-agent work starts at [`AGENTS.md`](AGENTS.md)
and [`docs/agent/NEXT.md`](docs/agent/NEXT.md). A paste-ready autonomous-run
prompt lives in [`docs/agent/CODEX-RUN.md`](docs/agent/CODEX-RUN.md).

The implementation frontier and next priority are tracked in
[`docs/agent/NEXT.md`](docs/agent/NEXT.md).

## Invariants

1. No housekeeping-only model call.
2. No eager capability implementation loading.
3. No full capability catalogue in model context.
4. No durable memory without provenance and scope.
5. No model inference represented as a user assertion.
6. No public client choosing trusted event authority.
7. No side effect authorized from a model's self-reported effect.
8. No credential material visible to the model.
9. No privileged action without a bounded lease.
10. No verified completion without task-specific evidence.
11. No permanent improvement from one successful trajectory.
12. No periodic LLM heartbeat when an event can wake the task.
13. No mandatory infrastructure beyond one daemon and local storage.

## License

Licensed under either Apache License 2.0 or MIT, at your option.
