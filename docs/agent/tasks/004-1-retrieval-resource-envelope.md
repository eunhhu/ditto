# Task 004.1: Retrieval resource envelope and lifecycle

## Status

Active under [ADR 0011](../../adr/0011-retrieval-resource-envelope.md).

## Objective

Keep Task 004's source-authority and all-or-nothing semantics while bounding the
composed memory, lexical, and provider work of one working-set request and
removing full session replay from its steady-state path.

## Required vertical slice

1. Add one cumulative `RetrievalWorkBudget` shared across query, context, and
   capability work, with typed exact-dimension failures before work occurs.
2. Rank context and capability roots as streaming top-K values; never retain a
   maximum-sized document per eligible candidate.
3. Count lifecycle-active context/capability candidates rather than immutable
   retired/superseded history.
4. Rebuild from source at open/recovery, then validate only checkpoint deltas on
   normal retrieval. Separate derived and verified snapshot types.
5. Validate typed working-set scope and bounded `SearchContext` before an
   injected provider call; reject non-canonical new durable context identity.
6. Enforce private local SQLite paths and add tracked, CI-replayable canary and
   audit evidence.

## Non-goals

- No Task 005 invocation/effect/lease/executor work.
- No production embedding provider, cache, vector database, FTS service, model
  call, credential, or network request.
- No event rewrite, history compaction, public context route, or cross-process
  writer claim.
- No Task 003 event or replay behavior change.

## Exit criteria

- A 10,000-item maximum-size generator remains within the fixed cumulative
  envelope and returns a typed budget error or bounded top-K without retaining
  all documents.
- Provider call and input-byte N/N+1 tests prove no over-budget call occurs.
- Superseded/expired/disputed context history and retired/quarantined manifests
  do not consume active candidate capacity.
- A second unchanged working-set query performs no full canonical replay; a
  later event is validated as a delta. Cache corruption still causes one source
  rebuild and never authorizes a result.
- Only `VerifiedContextSnapshot` can feed the kernel ranking path.
- Invalid scope, context identity, SearchContext bounds, and unavailable
  preferred placement fail before provider work.
- Unix SQLite permission/symlink/ownership tests, tracked canaries, focused
  tests, strict Clippy, `./scripts/agent-check.sh`, and Rust 1.88 all pass.
