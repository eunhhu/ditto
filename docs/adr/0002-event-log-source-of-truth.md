# ADR 0002: Event log as source of truth

- Status: accepted
- Date: 2026-08-29

## Context

Sessions, UI progress, task graphs, memory, approvals, replay tests, and self-improvement otherwise drift into separate stores and process-local state.

## Decision

Use one append-only event sequence as the durable source of truth. SQLite WAL is the initial implementation. Graphs, search indexes, task state, and client views are projections.

## Consequences

Clients can reconnect and replay. State changes are auditable. Self-improvement candidates retain exact provenance. Projection bugs can be repaired without rewriting history. Compaction and retention must preserve referenced evidence and audit requirements.
