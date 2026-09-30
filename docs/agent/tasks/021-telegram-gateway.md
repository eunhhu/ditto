# Task 021: Telegram gateway and scheduled result delivery

Contract and evidence in one file. Decision:
[ADR 0025](../../adr/0025-telegram-gateway.md).

## Problem

Ditto could be used only at the computer, and scheduled results waited until
someone looked. A daily assistant has to answer on the phone and push
scheduled results there.

## Contract

- `ditto telegram --allow-user ID` (token from `DITTO_TELEGRAM_BOT_TOKEN`)
  long-polls the Bot API. It answers only private chats of allowed users and
  ignores everything else before any daemon call.
- Questions become runs in one session, processed in order. Answers stream
  into a draft with a stop button and arrive as plain-text replies (split at
  4,000 UTF-16 units). `/new`, `/remember`, `/memories`, `/help` and `/stop`
  map to the typed daemon commands. Fixed replies follow the user's Telegram
  language (Korean or English).
- Request IDs derive from the Telegram message, so redelivered updates retry
  the same run.
- Run status reports `schedule_request_id` for runs started by a schedule or
  repeat occurrence. The gateway delivers exactly those runs from other
  clients to the allowed users, saves its event cursor after each run
  terminal, and resumes from it after a restart.
- The token never reaches the daemon, events, logs or errors.

## Exit criteria

1. Strangers and group chats get no reply and cause no daemon writes.
2. Streamed answers, commands, stop and scheduled delivery work end to end
   with the built CLI.
3. A restart neither repeats a model call for a redelivered message nor
   re-delivers a scheduled result.
4. The token appears in no output, error or event.
5. The canonical gate passes.

## Evidence (2026-09-30)

- Kernel: one-shot schedule runs and all three repeat occurrence runs report
  their schedule request ID; a direct run reports none; another session cannot
  inspect the run.
- CLI unit tests (8): command parsing with `@bot` suffixes, the allowlist and
  chat-type filter, deterministic canonical request IDs, UTF-16 splitting
  (emoji, Korean, oversized characters), draft tails, and the SSE parser (only
  relevant payloads kept, 3 MiB events skipped, CRLF and keep-alives). Also
  token scrubbing, including a server echoing the token and a closed port,
  and atomic cursor state.
- Built-CLI scenario (added to the gate): the real `ditto telegram` process
  against a mock Bot API and the daemon router with the real scheduler:
  - an allowed message streamed through a stop-button draft and was answered
    as a reply;
  - a stranger and a group chat got nothing, and their text is in no event;
  - `/new` answered in Korean for a Korean client, and `/remember` saved a
    memory;
  - the draft stop button cancelled a blocked answer (`Stopped.`);
  - a scheduled run was delivered as `⏰ <request>` with its answer, while a
    run from another client was not delivered;
  - after a restart a redelivered message got the recorded answer without a
    model call, and the scheduled result was not sent again;
  - the token was in neither the gateway output nor the events.
- RED checks: disabling the allowlist made the scenario fail (a reply reached
  the stranger). Delivering every finished run made it fail (the other
  client's run was pushed).
- `./scripts/agent-check.sh` passed on the final tree in 3 min 58 s on the
  pinned Rust 1.88.0 toolchain: 530 Rust tests (524 workspace including
  doctests, six built-CLI scenarios), 8 + 21 Python tests, 12 web renderer
  cases and both smokes. Local log `target/task021/gate.log`, SHA-256
  `6ce80984eeaa0108223ef72def42236b66785ef388ff5171e45331699e806e3d`.
- Not run: a real Telegram bot (it needs the user's token). The mock follows
  the Bot API 10.3 documentation read on 2026-09-30.
