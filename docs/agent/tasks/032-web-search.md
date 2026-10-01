# Task 032: Web search on its own; consent as a hand-off

Contract and evidence in one file. Decision:
[ADR 0033](../../adr/0033-autonomous-web-search.md). On 2026-10-01 the user
asked for the approval layer to be a hand-off ("승인레이어는 hand-off로") and
for Ditto to work on its own ("요즘 ai 하네스 트렌드가 알아서 잘 딱").

## Problem

Web search had been held back until each query could be approved
(ADR 0027), and nothing told the model to act on its own or how to ask the
user when it truly needs them.

## Contract

- No turn waits for a person. From turn payload version 11 the instructions
  say to use the supplied tools without asking first, and to hand off only
  what needs the user (a decision, information only they have, consent to
  something irreversible) by saying exactly that in the answer and stopping.
- `web.search` (`{ query }`, 1 to 200 characters) is offered to agent runs
  when the daemon has `--search-url` (a SearXNG-compatible service), and
  runs without approval. The configured endpoint is the only resource the
  turn's three-call lease allows; the request is
  `<endpoint>/search?q=<query>&format=json`. A credential-shaped query is
  refused and never sent; a blank one is `invalid_arguments`.
- At most five results return, `{ title, url, snippet }`, bounded, labeled
  untrusted; service failures return `http_status`, `invalid_response`,
  `connection` or `timeout` to the model.
- `agent.search.requested`, `agent.search.started` (claim evidence and the
  request URL) and `agent.search.output` record each call; replay checks them
  and feeds the recorded results back without network I/O.
- A `web.search` call refuses later memory writes in the turn.
- The web app shows "Searching the web…" while it runs.

## Exit criteria

1. A search reaches the configured service once with the trimmed query, its
   bounded results reach the model, and the turn replays without a request.
2. Replay rejects changed results, a request URL for another query, a
   missing start and a selection without the tool.
3. A fourth search, a secret and a blank query are refused, and only three
   requests leave the machine; service failures reach the model as errors.
4. Without a search service the tool is not offered.
5. The canonical gate passes.

## Evidence (2026-10-01)

- Kernel tests (`read_only_turn/web_search.rs`, a SearXNG-shaped service on
  loopback):
  - the tool surface is `artifact.read`, `web.fetch`, `web.search` and the
    three memory tools; the service saw exactly
    `/search?q=rust+release+%26+news&format=json`; five of seven results
    reached the model as untrusted search results; replay made no request
    and rejected a changed title, another query's request URL, a missing
    start and a selection without `web.search`;
  - in one turn a password-shaped query is `credential`, a blank one
    `invalid_arguments`, three searches succeed and the fourth is
    `lease_exhausted`; the service saw three requests, none with the secret;
  - HTTP 500 and a non-JSON body reach the model as `http_status` 500 and
    `invalid_response`, and replay;
  - a memory write after a search is `untrusted_content_read`;
  - without `--search-url` the tool is not offered and a call to it is a
    protocol failure.
- Unit tests: request URLs round-trip, and other URLs carry no query;
  SearXNG results are bounded links in order.
- Existing tests adapted to the version-11 instructions (six system
  segments) and eight package headers; the relabel helpers also remove the
  autonomy segment, and a turn now replays as versions 1 to 10.
- Checked red, each against one change and restored: the credential refusal,
  the three-search lease, replay's request URL check and the search taint
  each disabled make a test fail.
- Browser end-to-end (`scripts/web-e2e.js`, debug build, headless Chromium,
  a mock search service passed with `--search-url`): 32 of 32 checks,
  including "Ditto searches the web on its own" with "Searching the web…"
  shown while it runs.
- The autonomy instruction adds 393 bytes to every agent-run request.
- `./scripts/agent-check.sh` passed on the unchanged final tree in 1 min
  37 s (pinned Rust 1.88.0): 591 Rust tests (584 workspace including doctests,
  seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer cases and
  both smokes. Local log `target/task032/gate.log`, SHA-256
  `179cde18a816834ab38fea6f34e4a2f7ba3114d6c531600323af380b3ae89ddc`. An
  earlier run failed because the quality harness still expected payload
  version 10; `scripts/personal-quality.py` now expects 11.
