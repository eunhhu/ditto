# Task 034: The chat as a bridge to concurrent work

Contract and evidence in one file. Decision:
[ADR 0035](../../adr/0035-chat-as-a-bridge.md). On 2026-10-01 the user asked
for the chat to be only a bridge to work running behind it, with concurrency
and real time ("항상 채팅쳐놓고 이새끼 작업 끝낼때까지 기다려야하는게
ㅈ같았는데"), for messages sent per state of the work, and for a plain "ㅇㅋ"
to get a reaction instead of spending output tokens.

## Problem

A session ran one answer at a time: the next message got HTTP 429, the web
app hid its send button and Telegram queued every message behind the answer
in progress. A run knew nothing of the session's other work, every
acknowledgment cost a full model call, and text the model wrote before a tool
call was overwritten by the next step.

## Contract

- Four runs and sorts are active at once in total, from one session or
  several; the slot is keyed by input event. New work beyond four is `Busy`
  (HTTP 429) without a queue; scheduled work waits only for that limit
  (`runtime_busy`). Cancel, status and shutdown address each run.
- Turn contract 12. An agent run admitted while other agent runs of its
  session are active records their input event IDs in `agent_run.in_flight`
  (sorted, at most three). Its latest message carries
  `[Ditto: still working on "…" (started N min ago), …]` after the time note
  for each listed run that had not ended when its input was recorded, the
  request on one line and cut to 80 characters. Replay requires each listed
  ID to be an earlier agent-run input of the session that was not an
  acknowledgment, applies the same ended rule and rebuilds the note.
- A message without an attachment that is only an acknowledgment, from a
  closed list compared without case, spaces, emoji variants or closing
  punctuation, is recorded with `agent_run.acknowledged` and starts no turn
  when the last line of the thread's latest answer has no `?` or `？`. Its
  status is `acknowledged` (HTTP 200); a retry returns the same; it never
  joins the thread; replay of its turn ID is rejected. Scheduled runs are
  never acknowledgments.
- The instructions ask for one short line before a step that takes a while,
  for a hand-off to end with its question, and say what the notes mean.
- The web app's composer is never disabled; each running answer has its own
  stop link; text a request wrote before its tool call stays as its own
  message; an acknowledgment shows 👍 on the user's message.
- The Telegram gateway starts answers in message order and waits for them
  at once; it sends text written before a tool call as its own message,
  replies to each message with its answer, reacts 👍 to an acknowledgment, and
  `/stop` stops every answer in progress in the chat. `ditto chat` prints 👍.

## Exit criteria

1. Two runs of one session stream at once, the second carrying the note;
   both replay; four in total are admitted and a fifth run or a sort is
   refused.
2. Replay rejects a listed ID that is absent, the run itself, or not an
   input; a dropped list; a list on an earlier run; and a listed run's end
   moved before the message.
3. An acknowledgment after an answer without a question makes no model
   request and is not in the next run's history; after an answer whose last
   line asks, the same kind of word runs.
4. The Telegram scenario answers a second message while the first runs,
   reacts to an acknowledgment without a model call, stops one answer, and
   sends the line before a tool call as its own message.
5. The browser end-to-end check passes, and the canonical gate passes.

## Evidence (2026-10-01)

- Kernel tests (`read_only_turn/sessions.rs`):
  - `four_runs_stream_at_once_from_one_session_or_several`: two runs in one
    session and one each in two others reach the provider together; a fifth
    run in a running session or a new one, and a sort, are `Busy`; cancelling
    one leaves the other in its session running; all replay;
  - `a_later_run_sees_the_runs_still_in_flight`: the second message reads
    `[Ditto: still working on "Build the release and publish it" (started
    just now)]`; both turns replay; the forgeries of criterion 2 are
    rejected, while the same journal renumbered still replays; a run after
    both ended has no note;
  - `an_acknowledgment_costs_no_model_call_unless_it_answers_a_question`:
    "ㅇㅋ!" is `acknowledged` with no model request, one journal event and a
    rejected replay, and a retry and status return the same; after
    "Shall I publish it now? 🙂", "ok" runs and its history holds the two
    earlier questions only.
- Adapted: the schedule tests fill all four slots to show waiting and the
  wake-up when one ends; a due run in a running session starts beside it;
  the idempotent-start and sort tests no longer expect a per-session refusal.
  A unit test fixes the acknowledgment list, including what is not on it
  ("네", "좋아", "ok?", "ok, and the docs too").
- Built-CLI Telegram scenario (mock Bot API, real daemon router): criterion
  4, including the note in the second run's request and `setMessageReaction`
  with 👍 on message 15.
- Browser end-to-end (`scripts/web-e2e.js`, debug build, headless Chromium):
  34 of 34 checks, adding "a second message is answered while the first
  still runs", "stop cancels that answer alone", "an acknowledgment gets a
  reaction instead of an answer" (0 model requests) and "what Ditto says
  before a tool call stays as its own message".
- Release daemon on the same machine (instant loopback model unless noted,
  12 memories): 11.8 MB resident at idle (2.0 MB anonymous), 16.4 MB after 20
  turns, 19.1 MB at most after four runs in one session at once; those four
  (provider answering after 0.5 s) were all accepted, reached the provider
  within 7.1 ms of each other and finished in 516–523 ms; an acknowledgment
  answered HTTP 200 in 1.4 ms with no model request. The binary is 19 MB.
- The instructions grew by 244 bytes (2,066 to 2,310), constant across
  turns. Production code grew by about 325 lines (kernel slot, note and
  acknowledgment; Telegram and web clients), tests by about 320.
- `./scripts/agent-check.sh` passed on the unchanged final tree in 5 min
  10 s (pinned Rust 1.88.0): 593 Rust tests (586 workspace including
  doctests, seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer
  cases and both smokes. Local log `target/task034/gate.log`, SHA-256
  `79d11f0067825b2a4319e38453048755c38afd4220eb4505197702e68669bf17`.
