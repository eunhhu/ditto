# Task 029: Memory search and tool progress (ADR 0028 Phase E, in part)

Contract and evidence in one file. Decisions:
[ADR 0029](../../adr/0029-memory-search-tool.md) and
[ADR 0028](../../adr/0028-thin-realtime-harness.md), Phase E; design in
[realtime-harness](../../design/realtime-harness.md).

## Problem

Past about twelve short memories the context budget no longer holds them all,
and selection falls back to lexical overlap with the question. "What is my
pet's name?" then reaches no memory about the dog, and the model cannot ask
for more. While a tool ran, the web app showed an empty bubble.

## Contract

- Turn payload version 8 offers every agent run a builtin `memory.search`
  while the package `capabilities/core/memory-search` is installed and equals
  the bundled manifest; otherwise the tool is withdrawn and a call to it is a
  protocol failure. Legacy artifact turns never see it.
- Input `{ "query": string }`, 1 to 200 characters. A blank query returns
  `invalid_arguments` to the model; the turn goes on.
- The search reads the memories the turn's compilation included or left out
  as irrelevant or over budget, and among them only the user's own assertions
  (origin `user`, status `asserted`). It ranks by the context crate's
  complete-set tokens and relevance score, then by ID, and returns at most
  8 memories and 8 KiB of text, plus `searched`, the number searched.
- `agent.memory.requested` (model) and `agent.memory.output` (capability)
  record each call; replay rebuilds the search space from the recorded
  compilation and recomputes the result exactly.
- No per-turn search work: a turn keeps its compilation and a shared
  reference to the session snapshot, gathers the searchable memories at the
  first search, and replay rebuilds them only for turns that searched.
- While a tool runs and before the model writes again, the web app shows what
  it is doing ("Searching memories…", "Reading the linked page…"), from the
  tool call already on the event stream.
- Deferred with reasons in ADR 0028: one builtin tool lifecycle and parallel
  read-only calls. The design's Phase E exit criteria for those two parts
  (tools migrated with unchanged evidence, three links fetched concurrently)
  are therefore not met.

## Exit criteria

1. A memory the context budget left out reaches the model through a search,
   and replay rejects any change to the recorded result.
2. Nothing but the user's own assertions appears in a result, and
   superseded or other-session memories never do.
3. An invalid query returns an error the model reads; runs without the
   package are not offered the tool.
4. The web app shows progress while a tool runs.
5. The canonical gate passes.

## Evidence (2026-09-30)

- New kernel tests (`read_only_turn/recall.rs`):
  - with 43 memories in the session and the dog memories left out of the
    capsule, a search for "dog called" returns the two current dog memories
    (not the superseded one or another session's), `searched` 42; replay
    passes, and rejects a dropped, reordered, invented or widened result, a
    changed query, and a changed result that the next request's digest also
    carries;
  - an inferred node in the capsule is not returned (`searched` 1);
  - a blank query returns `invalid_arguments` and replays;
  - without the package the run is offered only `artifact.read`, and a
    search call fails with `protocol` and replays.
- New unit tests: `lexical_recall` ranks shared words and ignores function
  words (context crate); a search reads only user-origin asserted nodes among
  seven origin and status pairs, and returns at most 8 results and a best-first
  prefix within 8 KiB (kernel crate). The capability header drift test covers
  the new package.
- Checked red, each against one change and restored:
  - replay accepting the recorded result without recomputing it fails the
    self-consistent forgery;
  - a search space without the left-out memories, shared or at run time
    only, fails the expected IDs, and a replay that rebuilds none fails
    replay;
  - a search without the assertion filter fails both assertion tests.
- Existing tests adapted to the larger tool surface (expected tool lists,
  five package headers read, three manifests paged); the relabel test uses a
  capabilities directory with only `artifact.read`.
- Browser end-to-end (`scripts/web-e2e.js`, debug build, headless Chromium):
  28 of 28 checks, including "a running memory search shows progress" and
  "the memory search result reaches the model".
- `measure-harness.py`, release build, same machine, against the Task 028
  build with its own capabilities directory:
  - 12 memories: work before dispatch 1.0 ms median in both (mean 0.9 →
    1.0 ms), client to provider 2.05 → 2.23 ms, 8 events per turn in both,
    prompt prefix reuse 96.4 → 96.7 %, first request 3,419 → 3,752 bytes
    (tool definitions 891 → 1,224), journal bytes per answer byte 16.3 →
    18.3 (`capabilities.selected` 3,310 → 4,910 bytes of an 8,837-byte
    turn).
  - 1,000 memories, three alternating rounds each: mean work before dispatch
    4.4–4.6 → 4.3–5.4 ms and client to provider 5.8–6.1 → 6.1–7.5 ms, with a
    load average of 3–5 on the shared machine. A first version that copied
    the candidate memories into every turn measured 4.9–6.2 ms and
    6.3–8.3 ms; the shared snapshot replaced it.
- `./scripts/agent-check.sh` passed on the unchanged final tree in 6 min
  35 s (pinned Rust 1.88.0, load average near 5 on the shared machine): 571
  Rust tests (564 workspace including doctests, seven built-CLI scenarios),
  8 + 21 Python tests, 12 web renderer cases and both smokes. Local log
  `target/task029/gate.log`, SHA-256
  `956cbe1cd01a862157a8177aaab5180e2e89d0406f39fef9851bd436b81bd632`.
