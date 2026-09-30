# ADR 0024: Local web app and loopback host guard

Status: accepted for Task 020.

## Context

Ditto was usable only from a terminal. A daily-driver assistant needs a chat
screen that shows streamed answers, the conversation thread, memories and
schedules, and a way to see why an answer used what it used. A browser also
brings browser attacks: any web page the user visits can send requests to a
loopback port, and DNS rebinding makes such a page same-origin with the daemon
while it keeps its own host name.

## Decision

- **Embedded client.** The daemon serves one page (`/`, `/app.js`, `/app.css`,
  `/favicon.svg`) compiled into the binary. The page uses no framework, build
  step or remote resource. It calls the existing typed HTTP API and SSE stream
  like the CLI does, and holds no authority of its own.
- **Browser policy.** Every page response carries
  `Content-Security-Policy: default-src 'none'; script-src 'self';
  style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none';
  form-action 'none'; frame-ancestors 'none'`, plus `nosniff`, `DENY` framing
  and `no-referrer`. Model text is escaped and then rendered through a closed
  markdown subset (code, headings, lists, emphasis and http(s) links).
- **Loopback host guard.** While the daemon is bound to loopback, every route
  (API, stream and page) answers only requests addressed to `localhost` or a
  loopback IP literal, by request-target authority or `Host`. Anything else
  receives 403 before routing. The explicit unauthenticated-remote escape
  hatch keeps its existing behavior. JSON-only commands already stop plain
  cross-site form posts, and no route sends CORS headers.
- **Conversation view.** `GET /v1/conversation?session_id=...&limit=...`
  returns the newest finished agent-run exchanges after the latest reset
  (default and maximum 50), oldest first and unabridged, with the event
  sequence they were read through. It uses the same thread rule as model
  history (ADR 0022) but not its model bounds, and it writes nothing.
- **Why an answer.** The inspector reads the run's own events: the recorded
  context capsule and receipt (memories sent and why, memories left out by
  reason), included history turns, tool calls, request count and outcome.
  Answers stay labeled unverified.
- **Checks.** The gate runs `node scripts/test-web.js` (script syntax and the
  renderer, including injection cases), so Node joins Rust and Python as a
  gate tool. `scripts/web-e2e.js` is a manual browser check that drives the
  real daemon and CLI against a mock model.

## Rejected alternatives

- A framework and bundler would add a toolchain, dependency updates and supply
  chain exposure for one page.
- Serving files from disk would add a runtime path and traversal risk.
- A separate web server process would duplicate routing and authority.
- Permissive CORS or tokens embedded in the page would expose the API to other
  origins.
- Checking only `Origin` would still allow DNS-rebinding reads.

## Consequences and rollback

A browser on the same machine is now a full client, including live runs
started from the CLI or schedules. Clients that address the daemon by a
non-loopback name while it is bound to loopback are refused. Remote or phone
access still needs an authenticated gateway. Rollback removes the web routes,
the guard and the view endpoint; no stored data changes.
