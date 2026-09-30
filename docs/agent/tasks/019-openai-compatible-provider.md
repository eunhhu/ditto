# Task 019: OpenAI-compatible model provider

Contract and evidence in one file. Decision:
[ADR 0023](../../adr/0023-openai-compatible-provider.md).

## Problem

Ditto could only answer through the closed OpenAI `gpt-5.6` profile, so using
it at all required that one paid account. A daily-driver assistant needs free
local models and a choice of hosted providers.

## Contract

- `--provider openai-compatible --base-url URL --model NAME` (or
  `DITTO_PROVIDER`, `DITTO_BASE_URL`, `DITTO_MODEL`) drives runs, schedules and
  chats through any `/chat/completions` server. `DITTO_MODEL_API_KEY` supplies
  an optional bearer key; `--include-usage` requests stream usage.
- Configuration fails at startup for a missing model or URL, a non-loopback
  plain-HTTP URL, a URL with credentials, query or fragment, an invalid model
  name or a non-loopback listener. Redirects are never followed.
- Requests carry one system message (stable prefix plus context capsule),
  native history, readable function names and explicit tool controls.
  Streams map text, split or whole tool calls, finish reasons and usage onto
  the validated model stream; malformed or unfinished streams fail closed.
- The Responses profile and every recorded turn format are unchanged.

## Exit criteria

1. A real HTTP server receives the key only as a bearer header and answers a
   streamed tool call and its continuation through the kernel turn loop.
2. Ollama-style streams (whole calls, `stop` with calls, missing IDs or
   indexes, object arguments) and error, malformed or unfinished streams are
   covered.
3. The built daemon and CLI work against a compatible server with flags and
   with environment variables, without writing the key to logs or storage.
4. The canonical gate passes.

## Evidence (2026-09-30)

- Adapter: 9 chat unit tests (projection, single system message, unsupported
  shapes, readable and digest names, text/usage, split calls, whole calls
  with `stop`, index-less fragments, errors and missing finish, endpoint
  rules) and 2 tests over a real local HTTP listener (bearer key, keyless
  local server, bounded HTTP error). The 29 Responses contract tests pass
  unchanged on the shared lifecycle.
- Daemon: flag parsing and configuration rejections; an end-to-end kernel run
  where a mock `/v1/chat/completions` streams a split `artifact_read` call and
  then the answer. The continuation carried the assistant `tool_calls` entry
  and the `tool` result, the run finished `unverified` with the streamed
  text, and replay succeeded.
- Real processes: the built daemon (flags, then `DITTO_*` variables) against
  an Ollama-shaped mock with a dummy key. `ditto run` and a `ditto chat`
  session sent four requests, each with the bearer key, the saved memory in
  the system message and one tool. The history roles were `[system, user]`,
  then 4 and 6 messages as the thread continued from the run, then
  `[system, user]` after `/new`. The key appeared in neither the daemon log
  nor the data directory.
- `./scripts/agent-check.sh` passed on the final tree in 3 min 48 s (warm
  caches) on the pinned Rust 1.88.0 toolchain: 517 Rust tests (512 workspace
  including doctests, five built-CLI scenarios), 8 + 21 Python tests and both
  smokes. Local log `target/task019/gate.log`, SHA-256
  `a91075e27905a03c7400a0a139a659605cb7abee34b48881ae562897db81124a`.
- Not run: any live hosted provider or real local model (none installed and
  no charges approved). Answer quality and tool-use reliability per model
  remain unmeasured.
