# ADR 0004: Selective migration from the Codex prototype

## Status

Accepted.

## Decision

`main` remains the canonical architecture and runtime composition root. The `codex` prototype is a donor, not a branch to merge wholesale. A donor component is accepted only when it has a narrow boundary, preserves `main` invariants, and can be covered by focused tests.

The first accepted components are:

- SQLite triggers that enforce the existing event store's append-only contract without replacing server-issued event IDs or timestamps;
- a standalone SHA-256 artifact store with atomic installation, deduplication, metadata, size limits, symlink rejection, range reads, and verified reads;
- context provenance validation, epistemic constraints, lenses, and provenance-bearing graph edges inside the existing context crate;
- hard-filtered capability retrieval and a bounded execution epoch that only appends newly paged cards and preserves prefix ordering.

The model driver, turn loop, scheduler, task ledger, process executor, alternate protocol/schema surface, and directory breadth from the prototype are not migrated. They overlap stronger `main` boundaries or require a separate design decision.

## Consequences

Migration proceeds as small commits against `main`, with the donor branch retained for comparison. Every accepted slice must pass formatting, strict Clippy, workspace tests, and a regression test for its central invariant. Prototype breadth is not evidence of integration readiness.
