# Task 007: Bounded capability package headers

## Status

Active on `dev/task-007-capability-package-headers`, stacked on the completed
Task 006 branch and the user's product-intent documentation commits.

## Objective

Remove full manifest loading and retention from header-backed catalogue startup
and search while preserving the existing invocation and retrieval contracts.
See [ADR 0014](../../adr/0014-capability-package-headers.md).

## Exit criteria

- Bounded descriptor-safe discovery and a distinct generated header format.
- Header-backed startup/search read no full bodies; headerless compatibility
  reads are explicit and metered; selected paging verifies the body and header.
- Fixed N/N+1 filesystem, package, byte, and retained-data limits fail without
  a partial catalogue or unauthorized manifest.
- Header projection preserves legacy/V2 ranking, filters, complements, and
  cumulative candidate-byte charging.
- Bundled headers are reproducibly generated and checked for drift.
- Existing artifact turns use paging, record a stable path-free failure when
  paging fails, and preserve successful and historical no-I/O replay.
- Focused tests, the canonical gate, MSRV, and diff checks pass with tracked
  evidence. No provider call, worker, scheduler, or additional feature is added.
