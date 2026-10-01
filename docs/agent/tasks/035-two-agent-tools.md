# Task 035: Two agent tools, one tool lifecycle

Contract and evidence in one file. Decision:
[ADR 0036](../../adr/0036-two-agent-tools-one-lifecycle.md). On 2026-10-01
the user asked for Ditto to stop accumulating modules and tools and to be as
light and thin as possible ("주루루 모듈이나 도구달지말고 종합해서 최대한
가볍고 얇게 짜라").

## Problem

An ordinary agent run offered five or six tools (`artifact.read`,
`web.fetch`, `web.search`, `memory.search`, `memory.remember`,
`memory.forget`): their definitions were 1,971 of the 4,538 bytes of every
minimal request, and `artifact.read` could read nothing without an
attachment. Each tool had its own event kinds (eleven `agent.*` kinds),
payload types, run module, replay module and validators, and selection named
each tool twice, once at run time and once in replay.

## Contract

- Turn contract 13. An ordinary agent run offers `web.browse` (while reading
  or searching is enabled; its schema offers only `url`, only `query`, or one
  of the two) and `memory.manage` (`action` `search`, `remember` or `forget`).
  Their rules are those of ADRs 0027, 0029, 0031 and 0033, unchanged: leases,
  grants, refusal order, taint, bounds and results.
- `artifact.read` is selected for an agent run only with an attachment; a
  legacy artifact turn still selects it first. A run with no tool installed
  fails at selection (`tool_selection_failed`).
- Every call of `web.browse`, `memory.manage` and `artifact.sort` journals
  `tool.requested`, `tool.started` for a claimed call, and `tool.output`; a
  stop before authorization is "tool call stopped before authorization".
  Replay projects them as `tool_calls`. `artifact.read` keeps
  `capability.requested` / `execution.*`.
- Run status reads a sort's lifecycle through the schema-8 index
  `events_turn_tools` (fifteen-row sentinel), which replaces
  `events_agent_sort` on open.
- The web app labels a call once its arguments are known ("Searching the
  web…", "Saving to memory…") and keeps memory notices; the inspector lists
  every tool call.

## Exit criteria

1. An ordinary run is offered exactly `web.browse` and `memory.manage`; with
   reading off and no search service, only `memory.manage`.
2. The web, memory and sort behaviors and forgeries of Tasks 011, 023, 024,
   029 and 032 hold on the merged tools, including replay rejecting changed
   results, request normalization, start evidence and selections.
3. Status of a model sort reads the new lifecycle and the migrated index.
4. The browser end-to-end check and the canonical gate pass.

## Evidence (2026-10-01)

- Kernel tests: `read_only_turn/web.rs` (merging the fetch and search tests)
  covers reads, grants, budgets, private addresses, searches, secrets, service
  failures, the search taint and the schema variants, with forgeries of a
  page, a request's normalization, a start's epoch and request URL, a removed
  start, a denial recorded as a read, and a selection without the tool;
  `read_only_turn/memory.rs` (merging the search and write tests) covers
  search over left-out memories, writes, replacement, forgetting, the
  refusal order, the web taint and every recomputed decision; `model_sort.rs`
  keeps its status and replay forgeries on `tool.*`;
  `a_tool_is_offered_only_with_its_package` shows a missing package
  withdraws its tool and a run with none fails at selection.
- Unit tests: `web.browse` normalization and resources for both requests and
  the read-only schema; `memory.manage` normalization of every action and
  the refusal order including a search; the event store's schema-8 migration
  drops `events_agent_sort` and reads at most fifteen tool events of exactly
  one turn.
- Built-CLI Telegram scenario: the line before a `memory.manage` search is
  its own message. Browser end-to-end (`scripts/web-e2e.js`, debug build,
  headless Chromium): 34 of 34 checks, with progress labels and memory
  notices from `tool.*`.
- Measured on the release daemon (same machine, loopback mock model): a
  minimal agent-run request fell from 4,538 to 3,571 bytes, its tool
  definitions from 1,971 bytes (five tools) to 1,005 (two); prompt prefix
  reuse between turns 97.4 % (median); 8 events and 13.05 journal bytes per
  answer byte for a 200-delta answer. Resident memory 11.7 MB at idle
  (2.0 MB anonymous), 15.5 MB after 20 turns and at most 17.5 MB after four
  runs in one session at once (16.4 and 19.1 MB before); the binary is
  19 MB.
- Size: fourteen per-tool modules became five (`tool`, `web`, `memory`,
  `tool_run`, `tool_replay`); the turn module shrank from 8,989 to 7,731
  lines, production code by about 1,280 lines and tests by about 60; five
  capability packages became two.
- `./scripts/agent-check.sh` passed on the unchanged final tree in 5 min
  29 s (pinned Rust 1.88.0): 592 Rust tests (585 workspace including
  doctests, seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer
  cases and both smokes. Local log `target/task035/gate.log`, SHA-256
  `6b062ed2a0f09e8fae8be6b5b00dfab822e962b74263d045ce8d1a297e1282b1`.
