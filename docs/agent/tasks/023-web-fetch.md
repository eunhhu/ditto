# Task 023: `web.fetch` for links the user sends

Contract and evidence in one file. Decision:
[ADR 0027](../../adr/0027-web-fetch-for-user-links.md).

## Problem

Ditto could not read a linked page. A network tool must not become a way to
exfiltrate memories, reach the user's own network, or let page text steer the
model.

## Contract

- Agent runs grant `web.fetch` the canonical http(s) URLs in the user's
  message (at most five). The tool is offered only then, with a lease of
  min(links, 3) calls on exact `url:` resources. Unlisted URLs are denied
  before authorization and never contacted.
- Fetches use GET without proxy and follow up to five redirects by hand. Every
  hop refuses non-global addresses and connects only to the checked addresses.
  Bounds: 10 s to connect, 20 s in total, the turn deadline, a 2 MiB body,
  text types only, and 24,000 characters of extracted text.
- `agent.fetch.*` events journal each call. Replay recomputes the grant and
  continues from the recorded page without network I/O. The page reaches the
  model labeled as untrusted web content, and version-5 instructions say not
  to follow it.
- `--disable-web-fetch` never offers the tool; a missing package withdraws it.
- Also: the OpenAI-compatible adapter now sends a single structured tool
  result as plain canonical JSON instead of a tagged wrapper.

## Exit criteria

1. A linked page is fetched once, reaches the model with its origin label,
   and replays without network I/O.
2. Unlisted URLs, exhausted budgets and messages without links cannot reach
   the network.
3. The production policy never connects to private addresses.
4. Forged evidence fails replay.
5. The canonical gate passes.

## Evidence (2026-09-30)

- `ditto-web-fetch` (8 tests):
  - address classes: public v4 and v6 accepted; 38 private, reserved and
    IPv4-embedding forms refused;
  - HTML extraction, including quoted `>`, comments, entities, unterminated
    input and skipped script;
  - URL canonicalization and extraction from messages (trailing punctuation,
    balanced parentheses, Korean text, credentials, duplicates, cap);
  - invocations through the real compiler;
  - a local server: redirects (relative, absolute, loop), 404, unsupported
    type, JSON, a 2 MiB+ body, timeout and cancellation;
  - the production policy: six private targets refused while a listening
    socket received no connection.
- Kernel (4 integration tests):
  - a linked page read once with the web instruction segment; replay made no
    request, and a forged page text, an ungranted start URL, a missing
    selection and a claimed-but-denied output each failed replay;
  - an unlisted URL denied with zero requests, the granted one fetched once,
    and a repeat refused as `lease_exhausted`;
  - messages without links, and a disabled configuration, offered no tool; a
    call to it failed as `protocol`;
  - the production policy answered `blocked_address` with zero requests.
- RED: removing the kernel's grant check changed the budget test's outcome
  (the lease's exact-URL scope still refused the fetch), so the test fails.
- Real network, 2026-09-30: the built daemon with a mock OpenAI-compatible
  model that calls `web_fetch` on the first link:
  - `https://example.com/` returned title "Example Domain" and its text,
    labeled untrusted;
  - `http://127.0.0.1:18788/health` returned `blocked_address`;
  - a message without a link offered only `artifact_read`.
- The first gate run timed out once in the corpus harness (a 20 s CLI run
  bound, load average 8.7 on the shared machine); the suite passed alone in
  11.6 s. `./scripts/agent-check.sh` then passed on the unchanged final tree
  in 1 min 49 s (warm caches, pinned Rust 1.88.0): 544 Rust tests (537
  workspace including doctests, seven built-CLI scenarios), 8 + 21 Python
  tests, 12 web renderer cases and both smokes. Local log
  `target/task023/gate.log`, SHA-256
  `3ab7e97e43c3c5061d31bc0db7d83af53901f7f8d5ddebf9935b45c1ab9626bd`.
