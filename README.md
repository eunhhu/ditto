# Ditto

A local-first personal AI agent built as a Rust "semantic microkernel": the model owns intent and judgment, the runtime owns context, capabilities, side effects, persistence and long-lived execution.

**Status:** Prototype — the daemon, CLI, web app, Telegram gateway, memory, scheduling and bounded tools work end to end; calendar cron, general effectful tools, SSH, a production embedding worker, authenticated remote access and the improvement compiler are still deferred and are not faked.

> Context is compiled. Capabilities are paged. Effects are leased. Improvements are promoted.

See [docs/product.md](docs/product.md) for the product intent and [docs/architecture.md](docs/architecture.md) for the design.

## Features

- **Append-only SQLite event spine** with schema versioning, typed command ingress, paginated SSE replay (`/v1/stream`) and lag recovery.
- **Content-addressed artifacts** (SHA-256) with private storage and verified reads.
- **Capability packages** under `capabilities/core/*` with compact headers, bounded discovery and verified manifests.
- **Compiled context**: typed Context IR with provenance, session memories admitted as `context.node.recorded` events, and a rebuildable `context-projection.db` cache.
- **Memory**: save, correct and forget memories from the CLI, HTTP, web app or Telegram; during a run the model may also remember lasting facts, replace outdated ones and forget on request (labelled as its inference, never taken from fetched pages).
- **Model runs**: provider-neutral model IR with an OpenAI Responses adapter and an OpenAI-compatible `/chat/completions` adapter (local models via Ollama, hosted ones via a key). Durable retry identity, cancellation and restart inspection.
- **Conversation threads** and an interactive `ditto chat`; up to four answers may run at once in one session.
- **Two model tools**: `memory.manage` (search / remember / forget) and `web.browse` (read a linked or found page, search through a SearXNG-compatible service), on one journaled tool lifecycle with hard per-answer limits.
- **Bounded local sorting** of an attached file through a one-shot process lease with independent verification.
- **Schedules**: one-shot future requests and finite fixed-interval repeats with durable claims, visible expiry/missed states and no automatic retry.
- **Local web app** served by the daemon (streamed chat, threads, memories, schedules, a per-answer "Why?" view) behind a strict CSP and loopback host guard.
- **Telegram gateway** (`ditto telegram`) for allowed private chats, with streamed drafts, a stop button and delivery of scheduled results.
- Reproducible offline RAM/latency/accounting baselines (`scripts/personal-baseline.py`, `scripts/personal-quality.py`).

## How it works

- `apps/daemon` (`ditto-daemon`) owns the data directory, exposes the HTTP API and the embedded web app, and runs the scheduler. Every accepted command is appended to the SQLite event spine (`crates/event-store`) before it is reported as durable.
- `crates/kernel` composes the pieces: it admits context nodes, compiles the model-facing context (`crates/context`, `crates/context-projection`, `crates/retrieval`), selects capabilities (`crates/capability`), enforces effect profiles and leases (`crates/policy`) and drives model turns (`crates/model`, `crates/model-openai`).
- Files live in a content-addressed store (`crates/artifact-store`); `artifact-read` and `artifact-sort` are the two bounded capabilities that touch them. `crates/web-fetch` performs public-address-only page reads.
- `apps/cli` (`ditto-cli`) is a thin client over the same HTTP API that the web app and Telegram gateway use; there is no public arbitrary event-append endpoint.
- Clients observe progress by replaying and following the event stream rather than by polling the model.

## Tech stack

- Runtime/Language: Rust 2024 edition, `rust-version = 1.88` (see `rust-toolchain.toml`)
- Framework: `axum` HTTP server, `tokio` runtime, `clap` CLI, `reqwest` (rustls) client
- Storage: SQLite via bundled `rusqlite`; content-addressed artifact files under the data directory
- Infra/Tooling: Cargo workspace (16 members), `scripts/agent-check.sh` quality gate, GitHub Actions (`.github/workflows/ci.yml`), Python/Node smoke and e2e scripts

## Getting started

### Requirements

- Rust 1.88 or newer
- Linux or macOS (`/usr/bin/sort` is required for local sorting)
- Optional: an OpenAI-compatible model server (e.g. Ollama) or a hosted provider key; a SearXNG instance for web search

### Install

```bash
git clone https://github.com/eunhhu/ditto
cd ditto
cargo build -p ditto-daemon -p ditto-cli --locked
```

### Run

```bash
# Terminal 1 — daemon without any model provider (records input, memories, schedules)
cargo run -p ditto-daemon -- --data-dir .ditto --capabilities-dir capabilities --bind 127.0.0.1:8787

# Terminal 2 — CLI
cargo run -p ditto-cli -- ping
cargo run -p ditto-cli -- input "hello from ditto" --session local
cargo run -p ditto-cli -- memory save "I prefer afternoon meetings"
cargo run -p ditto-cli -- memory list
curl -N 'http://127.0.0.1:8787/v1/stream?after_seq=0'
```

To get answers, start the daemon with a provider:

```bash
# Local model through Ollama (no key)
cargo run -p ditto-daemon -- --provider openai-compatible \
  --base-url http://127.0.0.1:11434/v1 --model qwen2.5:7b

# Hosted OpenAI-compatible endpoint (may incur charges)
DITTO_MODEL_API_KEY=... cargo run -p ditto-daemon -- --provider openai-compatible \
  --base-url https://api.example.com/v1 --model MODEL_NAME

# OpenAI Responses adapter
OPENAI_API_KEY=... cargo run -p ditto-daemon -- --provider openai
```

Then:

```bash
cargo run -p ditto-cli -- run "What is my meeting preference?"
cargo run -p ditto-cli -- chat                     # interactive; /new, /remember, /exit
cargo run -p ditto-cli -- run "Sort this list" --sort-file list.txt --human
cargo run -p ditto-cli -- schedule "Summarize my week" \
  --at "2026-09-10T09:00:00+09:00" --expires "2026-09-10T10:00:00+09:00"
cargo run -p ditto-cli -- repeat "Daily summary" \
  --at "2026-09-10T09:00:00+09:00" --expires "2026-09-10T10:00:00+09:00" \
  --every-seconds 86400 --occurrences 7
```

Open `http://127.0.0.1:8787/` for the web app. For Telegram, create a bot with @BotFather and run the gateway next to the daemon:

```bash
DITTO_TELEGRAM_BOT_TOKEN=... cargo run -p ditto-cli -- telegram --allow-user 123456789
```

The daemon refuses non-loopback binds unless `--allow-unauthenticated-remote` is given; that flag adds no authentication, and model execution still requires loopback.

### Development

```bash
./scripts/agent-check.sh            # fmt, clippy -D warnings, tests, built-CLI integration tests
python3 scripts/smoke-user-memory.py
python3 scripts/smoke-agent-run.py
python3 scripts/smoke-local-sort.py
node scripts/web-e2e.js             # headless Chromium against the real daemon (puppeteer-core)
python3 scripts/personal-baseline.py --output target/ditto-baseline.json
```

Coding-agent instructions start at [`AGENTS.md`](AGENTS.md) and [`docs/agent/NEXT.md`](docs/agent/NEXT.md).

## HTTP surface

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/health` | Liveness, durable count and latest sequence |
| `GET` | `/` | Embedded web app |
| `POST` | `/v1/commands/input` | Submit user input |
| `POST` | `/v1/commands/conversation/reset` · `GET /v1/conversation` | Threads |
| `POST` | `/v1/commands/memory`, `/v1/commands/memory/forget` · `GET /v1/memories` | Memory |
| `POST` | `/v1/commands/run`, `/v1/commands/run/cancel` · `GET /v1/runs` | Model runs |
| `POST` | `/v1/commands/schedule`, `/v1/commands/schedule/cancel` · `GET /v1/schedules`, `/v1/schedules/pending` | One-shot schedules (loopback only) |
| `POST` | `/v1/commands/repeat`, `/v1/commands/repeat/cancel` · `GET /v1/repeats`, `/v1/repeats/active` | Repeats (loopback only) |
| `POST` | `/v1/commands/sort`, `/v1/commands/sort/cancel` · `GET /v1/sorts` | Local sort (loopback only) |
| `GET` | `/v1/events`, `/v1/stream` | Durable event pages and replay/follow stream |
| `GET` | `/v1/capabilities` | Capability card search |

Contracts are specified in [`docs/specs/`](docs/specs) and decisions in [`docs/adr/`](docs/adr).

## Project structure

```
apps/
  cli/          ditto-cli: commands, chat, memory, runs, schedules, sorts, telegram gateway
  daemon/       ditto-daemon: HTTP API, embedded web app, scheduler, baseline harness
crates/
  kernel/               composition root: admission, runs, scheduler, tool lifecycle
  event-store/          append-only SQLite event spine
  artifact-store/       SHA-256 content-addressed files
  artifact-read/, artifact-sort/   bounded artifact capabilities
  capability/           capability package headers, discovery, verification
  context/, context-projection/, retrieval/   Context IR, projection cache, working-set retrieval
  model/, model-openai/ provider-neutral model IR and OpenAI(-compatible) adapters
  policy/               effect profiles and lease primitives
  protocol/             public command/event types
  web-fetch/            public-address-only page reads
capabilities/core/      shipped capability packages (artifact-read, artifact-sort, memory-manage, web-browse, …)
docs/                   product, architecture, ADRs, specs, design notes, coding-agent task contracts
scripts/                quality gate, smoke tests, web e2e, offline baselines
```

## Configuration

All daemon flags can also be set through environment variables.

| Variable | Used by | Purpose |
| --- | --- | --- |
| `DITTO_DATA_DIR` | daemon | Data directory (default `.ditto`) |
| `DITTO_CAPABILITIES_DIR` | daemon | Capability packages root (default `capabilities`) |
| `DITTO_BIND` | daemon | Listen address (default `127.0.0.1:8787`) |
| `DITTO_ALLOW_UNAUTHENTICATED_REMOTE` | daemon | Allow a non-loopback bind (unsafe; no auth added) |
| `DITTO_PROVIDER` | daemon | `disabled` (default), `openai-compatible` or `openai` |
| `DITTO_MODEL`, `DITTO_BASE_URL`, `DITTO_INCLUDE_USAGE` | daemon | Model name, API root and usage reporting for `openai-compatible` |
| `DITTO_MODEL_API_KEY` | daemon | Key for a hosted `openai-compatible` endpoint |
| `OPENAI_API_KEY` | daemon | Key for `--provider openai` |
| `DITTO_DISABLE_WEB_FETCH` | daemon | Never read web pages |
| `DITTO_SEARCH_URL` | daemon | SearXNG-compatible search service; without it there is no web search |
| `DITTO_API` | cli | Daemon base URL (default `http://127.0.0.1:8787`) |
| `DITTO_TELEGRAM_BOT_TOKEN`, `DITTO_TELEGRAM_ALLOWED_USERS` | cli `telegram` | Bot token and comma-separated allowed user IDs |
| `RUST_LOG` | daemon | Tracing filter (default `ditto=info`) |
| `TZ` | daemon | Local time shown to the model |

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

Licensed under either [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
