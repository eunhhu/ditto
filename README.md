# Ditto

> **A local-first semantic microkernel for frontier agents.  
> One binary, infinite capabilities, constant context.**

Ditto compiles task context, pages capabilities on demand, gates effects with
bounded leases, and records every durable state transition in one append-only
event stream. The architecture and implementation order live in
[`docs/PLAN.md`](docs/PLAN.md).

## Repository status

This scaffold implements the first executable vertical slice:

- SQLite WAL event spine with replay
- content-addressed artifact storage
- deterministic context receipt compilation
- bounded capability working sets
- typed effect claims and leases
- shell-free local `device.process.run`
- model-driver streaming contract with a deterministic development driver
- Unix-domain-socket daemon and streaming CLI

Later slices have explicit package boundaries but no fake production
implementations. Python is not a core dependency, and capability code never
loads into the daemon process dynamically.

## Quick start

```bash
cargo run -p ditto-daemon -- --data-dir .ditto
```

In another terminal:

```bash
cargo run -p ditto-cli -- --socket .ditto/ditto.sock run "inspect local service status"
```

Replay a task using the task ID printed by the CLI:

```bash
cargo run -p ditto-cli -- --socket .ditto/ditto.sock events --task <task-id>
```

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The TypeScript capability SDK lives in `sdk/typescript` and targets Bun. It is
protocol-only at this stage so workers remain isolated over JSON Lines.
