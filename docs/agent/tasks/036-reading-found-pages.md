# Task 036: Reading the pages a search found

Contract and evidence in one file. Decision:
[ADR 0037](../../adr/0037-reading-found-pages.md). On 2026-10-01 the user asked
to carry on with the remaining work ("마저 작업 ㄱㄱ"); this was the first
item still open on the frontier.

## Problem

Ditto searched the web on its own (Task 032) but read only the results'
titles and snippets: a result page could be read only if the user sent its
link. Answers about current or outside facts rested on snippets of at most
400 characters.

## Contract

- Turn contract 14. A turn's read grant is the canonical URLs of the user's
  message followed by the URLs each of its searches returned, from that
  search's `tool.output` on, in order and without duplicates. Any other URL,
  including a result's before its search returned it, is
  `permission_denied` without network I/O; a deployment that does not read
  pages grants none.
- With a search service the read lease allows three calls in total, shared
  by the user's links and found pages; without one it keeps min(links, 3).
  The lease's exact `url:` resources grow with each search's results
  (`InvocationAuthorizer::extend_lease_resources`); its budget and expiry do
  not.
- Found pages are read under the fetch policy of ADR 0027 (public addresses
  only in production) and reach the model as untrusted pages; memory writes
  stay refused after them.
- Replay grows the grant from the recorded results in the same order and
  checks each read against it, without network I/O.
- No event or payload changes. The `web.browse` summary of a deployment that
  reads and searches names the found pages; the web app labels a read
  "Reading a page…".

## Exit criteria

1. A page a search returned is read later in the same turn; before the
   search returns it, and for a URL neither linked nor returned, the read is
   `permission_denied` and the page is never contacted.
2. The user's links and found pages share three reads; a fourth is
   `lease_exhausted`.
3. Under the production policy a search service's link into the local
   network is `blocked_address` and never contacted.
4. Replay recomputes the grant in order: an early denial and a later read of
   the same page both replay, with no network I/O; a denial recorded for a
   granted page is invalid.
5. An extended lease permits exactly the added resources, within its
   unchanged budget.
6. The browser end-to-end check and the canonical gate pass.

## Evidence (2026-10-01)

- Kernel tests (`read_only_turn/web.rs`):
  `pages_a_search_found_are_read_within_the_shared_budget` reads a result
  page after its search, refuses it before the search and refuses a URL
  neither linked nor returned without contacting either, shares three reads
  with the user's link (the fourth is `lease_exhausted`), and replays the
  turn in order without searching or fetching again;
  `the_production_policy_never_reaches_private_links` now also shows a
  search service's loopback link is `blocked_address` and never contacted.
  The other web tests hold unchanged, including min(links, 3) without a
  search service.
- Unit tests: `found_pages_are_granted_once_returned_and_only_while_reading`
  (kernel) checks the grant's order, deduplication and budgets and that a
  recorded denial of a granted page is invalid;
  `an_extended_lease_permits_exactly_the_added_resources_within_its_budget`
  (policy) checks that an extension adds exact resources once, leaves other
  resources denied and does not add calls.
- Browser end-to-end (`scripts/web-e2e.js`, debug build, headless
  Chromium): 34 of 34 checks.
- Measured on the release daemon (same machine, loopback mock model): a
  minimal agent-run request without a search service is unchanged (3,568
  bytes at this run's local time, tool definitions 1,005 bytes); with one,
  the tool definitions are 1,186 bytes, 34 more than before for the longer
  `web.browse` summary. Prompt prefix reuse 97.43 % (median), 8 events and
  13.05 journal bytes per answer byte for a 200-delta answer, as in Task 035.
  Resident memory 11.6 MB at idle (2.0 MB anonymous), 15.6 MB after 20 turns
  and at most 17.2 MB after four runs in one session at once (11.7, 15.5 and
  17.5 MB in Task 035).
- Size: production code grew by 84 lines (the grant type, the lease
  extension and their wiring); the turn module is 7,828 lines including a
  42-line unit test.
- `./scripts/agent-check.sh` passed on the final code in 3 min 18 s (pinned
  Rust 1.88.0, warm build cache): 595 Rust tests (588 workspace including
  doctests, seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer
  cases and both smokes. Local log `target/task036/gate.log`, SHA-256
  `a4ba6e86e96bdfb3dd2b06d29c5b464e050eb9e27570aea22e31f1e89e99aaee`.
  Afterwards only this evidence and the handoff changed; `cargo fmt --check`
  and `./scripts/agent-canary.sh`, with the new documents tracked, passed
  again on the final tree.
