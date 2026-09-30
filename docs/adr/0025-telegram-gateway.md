# ADR 0025: Telegram gateway and scheduled result delivery

Status: accepted for Task 021. Amends the runtime-composition plan in
`docs/architecture.md`, which assigned gateways to TypeScript/Bun.

## Context

A personal assistant has to reach the user away from the computer, and
scheduled requests are only useful if their results arrive. Telegram offers
a free bot API with long polling, so it needs no inbound port, public address
or webhook. Bot API 10.3 (2026-08-24) streams partial answers with
`sendMessageDraft`, and its optional stop button reports
`stopped_message_generation`.

## Decision

- **Client, not kernel.** `ditto telegram` is a CLI subcommand. It calls the
  daemon's public HTTP API and event stream like any client and gains no
  authority. The bot token comes only from `DITTO_TELEGRAM_BOT_TOKEN`, stays
  in the gateway process, appears only in Bot API request paths and is
  scrubbed from every error. The daemon never receives it.
- **Who may talk.** Only private chats whose user ID is in `--allow-user`
  (required) are handled. Other users and group chats are ignored without a
  reply, before any daemon call.
- **Conversation.** Chats use one Ditto session (default `personal`), so they
  share memories and the conversation thread with the CLI and web app.
  Messages are handled in order by one worker; a message that meets another
  client's run waits for the run slot. `/new`, `/remember`, `/memories` and
  `/help` map to the typed commands. `/stop` and the draft stop button cancel
  the answer in progress. Replies are plain text, so model output never
  becomes Telegram markup.
- **Streaming.** An empty draft shows "Thinking…". Text deltas from the event
  stream update the draft at most about once a second. The final answer is
  sent with `sendMessage`, split at 4,000 UTF-16 units and replying to the
  question. A failed draft falls back to the typing action.
- **Idempotent retries.** The run request ID is a canonical ULID derived from
  the chat, message ID and date. A redelivered update therefore retries the
  same run: the daemon returns the recorded result without another model
  call.
- **Scheduled results.** Run status gains an additive
  `schedule_request_id` naming the schedule or repeat occurrence that started
  the run. When a run the gateway did not start finishes, the gateway reads
  its status and delivers it to every allowed user only if a schedule started
  it, titled with its request. After each run terminal the last handled event
  sequence is saved to a state file (`--state-file`) by atomic replace, so
  results that finish while the gateway is down arrive after restart. Without
  saved state the gateway starts at the present. Delivery is at least once.

## Rejected alternatives

- Webhooks need a public HTTPS endpoint, which a local-first daemon lacks.
- Running the gateway inside the daemon would put a third-party credential
  and network loop in the kernel process.
- A TypeScript/Bun gateway would add a runtime and toolchain to install and
  gate for one HTTP client; the Rust CLI already has one.
- Replying to strangers ("not allowed") confirms the bot exists and invites
  probing.
- Inferring scheduled runs from event order would break when admission fails.

## Consequences and rollback

The user can chat, stop answers, manage memories and receive scheduled
results on a phone. The gateway must run alongside the daemon. Answers stay
unverified model output. Rollback removes the subcommand and the optional
status field; no stored data changes.
