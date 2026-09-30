# Task 020: Local web app

Contract and evidence in one file. Decision:
[ADR 0024](../../adr/0024-local-web-app.md).

## Problem

Ditto was usable only from a terminal, and nothing showed why an answer used
what it used. A browser client also exposes the loopback daemon to DNS
rebinding from any page the user visits.

## Contract

- The daemon serves an embedded page at `/` (with `/app.js`, `/app.css` and
  `/favicon.svg`) under a same-origin-only content security policy. The page
  offers streamed chat, the current thread, new conversation, stop, memories
  (save and correct), schedules and repeats (create, list, cancel), and a
  per-answer inspector. English and Korean follow the browser language; light,
  dark and phone layouts are supported.
- Model text is escaped, then rendered through a closed markdown subset.
- While bound to loopback, every route answers only requests addressed to
  `localhost` or a loopback IP literal; other names get 403 before routing.
- `GET /v1/conversation` returns the current thread's newest finished
  exchanges (at most 50), oldest first and unabridged, with `through_seq`.
  The thread loader is shared with model history; model bounds are unchanged.

## Exit criteria

1. Foreign host names cannot read the page or use the API, including POST
   commands and absolute-form targets; loopback names work.
2. Injection through model or user text does not create markup.
3. In a real browser against the real daemon: streaming, thread persistence
   across reloads, live runs from other clients, stop, memory correction,
   inspector reasons, schedules, and no console errors or policy violations.
4. The canonical gate passes with the web checks.

## Evidence (2026-09-30)

- Kernel: the conversation view test covers order, unabridged text, failed
  and other-session exclusion, limit clamping, reset and invalid sessions. The
  two Task 018 thread tests pass on the shared loader.
- Daemon: asset headers and page shape (one same-origin script, no inline
  style, handler or remote URL, 404 for unknown assets). Host guard: five
  loopback forms are served; six foreign names are refused on four routes,
  including a POST that would record input; an absolute-form target and a
  Host-less request are refused; no event is written; the remote assembly is
  unchanged. With the guard disabled, the test failed (`evil.example` got
  200). Conversation endpoint: two runs listed oldest first; invalid session,
  unknown field and missing session return 400; a reset empties the view.
- `node scripts/test-web.js`: script syntax and 12 renderer cases, including
  escaped tags, attribute breakout, `javascript:` text and forged placeholders.
- `node scripts/web-e2e.js` passed 26 of 26 checks in headless Chromium
  151.0.7922.173 (puppeteer-core 24.43.1, Node 24.20.0) against the built
  daemon and CLI with a loopback mock model:
  - incremental streaming, memory in context and markdown;
  - inspector reason `all memories fit, so all were sent`;
  - follow-up history, reload restoring the thread, a CLI run appearing live;
  - stop ending in `Stopped: cancelled`, new conversation, escaped injection;
  - memory correction reaching the model, schedule create and cancel;
  - header and composer staying in view in a long thread and at phone width;
  - dark theme, no horizontal overflow, the Korean interface;
  - 403 for a foreign Host, and no console error, policy violation or dialog.
- A layout regression reproduced before its fix: with the earlier stylesheet,
  both in-view checks failed (24 of 26).
- `./scripts/agent-check.sh` passed on the final tree in 4 min 44 s on the
  pinned Rust 1.88.0 toolchain: 521 Rust tests (516 workspace including
  doctests, five built-CLI scenarios), 8 + 21 Python tests, 12 web renderer
  cases and both smokes. Local log `target/task020/gate.log`, SHA-256
  `f2072c79c9f02f934b55f05aba6c803bd0275ae311d4fdd9ee9f9aee7a7aa468`.
