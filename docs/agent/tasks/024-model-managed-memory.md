# Task 024: Memory that Ditto manages

Contract and evidence in one file. Decision:
[ADR 0031](../../adr/0031-model-managed-memory.md). On 2026-10-01 the user
approved model-managed memory ("능동적으로 Memory 관리되게끔"), which reverses
ADR 0015's restriction of memory writes to the user's own ingress.

## Problem

Only the user could save, correct or replace a memory. Ditto could neither
keep what the user said for later conversations without `/remember`, nor
update an outdated memory, nor forget one when asked.

## Contract

- Turn payload version 10 offers agent runs `memory.remember` (`text`, 1 to
  500 characters, optional `replaces`) and `memory.forget` (`memory_id`), each
  withdrawn when its package is missing or altered.
- A write is a session-scoped, task-free `memory.written` event and the
  context node it sources: a remembered fact with origin `model`, status
  `inferred`, or a disputed node that supersedes a forgotten memory. The turn
  journals `agent.memory_write.requested` and `agent.memory_write.output`.
- Refusals, in order: `invalid_arguments`, `untrusted_content_read` (after
  `web.fetch`, `artifact.read` or `artifact.sort` in the same turn),
  `credential`, `memory_unavailable` (the target is not an active memory of
  the session), `limit_reached` (three writes per turn, one lease).
- The memory list marks Ditto's memories `inferred`; the web app says what
  Ditto remembered, replaced or forgot, and badges inferred memories;
  Telegram's `/memories` marks them. `memory.search` from version 10 also
  reads inferred memories, marked.
- The second and third system segments describe both kinds of memory and when
  to remember, replace or forget.
- Replay recomputes every decision and checks a written result against its
  `memory.written` event and node.

## Exit criteria

1. A fact Ditto remembers is listed as inferred, reaches later turns' context
   as an inference, and is found by search.
2. Replacing and forgetting take memories out of context, search and the
   list; the user can correct an inferred memory.
3. Writes after reading a file or page, credential-shaped facts, invalid
   calls and writes past the limit are refused, and nothing is written.
4. Replay rejects a forged memory ID, text, a missing record, a changed
   request and a refusal or code the rules do not give.
5. The web app shows the notice and the badge.
6. The canonical gate passes.

## Evidence (2026-10-01)

- New kernel tests (`read_only_turn/memory_write.rs`):
  - a remembered fact is listed as inferred and sourced from its
    `memory.written` event (model, no task); a later turn's capsule carries it
    as origin `model`, status `inferred`, and a search finds it marked
    `inferred`; both turns replay;
  - replacing the user's memory leaves only the new one; the user corrects
    Ditto's memory like any other; forgetting it empties the list and the next
    context, and forgetting it again or forgetting the replaced memory is
    `memory_unavailable`;
  - after an `artifact.read` call in the same turn a write is
    `untrusted_content_read` and nothing is recorded;
  - in one turn a password-shaped fact is `credential`, a blank fact and a
    malformed `replaces` are `invalid_arguments`, three writes succeed and the
    fourth is `limit_reached`;
  - replay rejects another memory ID, a changed `memory.written` text, a
    missing node or `memory.written` event, a changed normalized request, a
    refusal recorded for a write that happened, and another refusal code even
    when the next request's digest carries it;
  - without the packages a write is a protocol failure, and the turn replays.
- New unit tests: credential shapes and ordinary facts, the refusal order,
  forgetting as a disputed superseding node, and argument normalization.
- Existing tests adapted: tool lists, seven package headers, five paged
  manifests, the version-10 search label, and the version relabel tests, which
  reseal requests with the earlier memory instructions (kept frozen in the
  test file) to replay one turn as versions 1 to 9.
- Checked red, each against one change and restored: the taint rule, the
  credential rule, the write limit, the run-time target check, replay's
  refusal recomputation and the version-10 search of inferred memories each
  disabled make a test fail.
- Browser end-to-end (`scripts/web-e2e.js`, debug build, headless Chromium):
  30 of 30 checks, including "Ditto says what it remembered" (the notice
  under the answer) and "the memory list marks what Ditto inferred".
- `measure-harness.py`, release build, same machine, against the Task 030
  build, twelve memories, 200-delta answers, two rounds each: work before
  dispatch 1.0 ms median in both, client to provider 2.47–2.56 → 2.50–2.68
  ms, eight events per turn in both, journal bytes per answer byte 13.39 →
  14.10, prompt prefix reuse 96.75 → 97.62 %, first request 3,748 → 5,224
  bytes (tool definitions 1,224 → 2,101). One short agent turn journals 5,448
  bytes instead of 4,879 (`capabilities.selected` 952 → 1,515).
- `./scripts/agent-check.sh` passed on the unchanged final tree in 3 min
  42 s (pinned Rust 1.88.0): 582 Rust tests (575 workspace including doctests,
  seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer cases and
  both smokes. Local log `target/task024/gate.log`, SHA-256
  `d465f10c4c20a4b34bd170ff19288dfdb9331c6e1656e550024fc1ff18abca06`.
