# ADR 0035: The chat as a bridge to concurrent work

Status: accepted on 2026-10-01 for Task 034. Turn contract 12 (replacing 11,
as [ADR 0034](0034-one-turn-contract.md) requires). Replaces
[ADR 0028](0028-thin-realtime-harness.md) Phase D's one run per session.

## Context

A session admitted one run at a time: while Ditto worked, the next message
was refused (HTTP 429) and every client made the user wait. The user found
this the worst part ("항상 채팅쳐놓고 이새끼 작업 끝낼때까지 기다려야하는게
ㅈ같았는데") and pointed at assistants whose chat is only a bridge to work
running behind it: they post a message at each state of the work, keep
talking while it runs, and answer a plain "ㅇㅋ" with a reaction instead of
spending tokens on a reply.

## Decision

- **Runs at once in one session.** The run slot holds up to four runs and
  sorts in total, from any sessions, keyed by input event, with no limit per
  session. New work is refused (HTTP 429) only while four are active.
  Scheduled work waits only for that limit. Cancellation, status and shutdown
  address each run itself. Each run still sees its thread's finished
  exchanges.
- **Work in flight.** When a run is admitted while other agent runs of its
  session are active, its input lists their input event IDs
  (`agent_run.in_flight`, sorted, at most three), and its latest message
  carries a second Ditto note after the local time:
  `[Ditto: still working on "…" (started 2 min ago), …]`, each request on one
  line and cut to 80 characters. A listed run that had ended before this
  input was recorded is left out, at run time and in replay alike, so a run
  that finished just as the message arrived is in the history, not the note.
  Replay requires each listed ID to be an earlier agent run of the session
  that was not an acknowledgment, and rebuilds the note. The instructions say
  what the notes mean and not to redo work in flight.
- **Acknowledgments cost nothing.** A message without an attachment that is
  only an acknowledgment (a closed list such as "ㅇㅋ", "ok", "고마워",
  "thanks" or "👍", compared without case, spaces, emoji variants or closing
  punctuation) is recorded but starts no turn when the last line of the
  thread's latest answer asks nothing (has no `?` or `？`). Its status is
  `acknowledged`, it never joins the conversation thread, and clients show a
  👍 reaction on the message. After an answer that asks, it is an answer and
  runs as usual; the instructions tell the model to end a hand-off with its
  question. Agreement words such as "네" or "좋아" are not on the list, and
  scheduled runs are never acknowledgments.
- **A message per state.** The instructions ask for one short line before a
  step that takes a while. Clients show the text a model request wrote before
  its tool call as its own message as soon as the tool starts: "Looking that
  up.", then the answer. The final request's text remains the answer, which
  history and status read.
- **Clients.** The web app never disables its composer, shows each answer
  under its own message with its own stop link, and a reaction for an
  acknowledgment. The Telegram gateway starts answers in message order and
  waits for them at once, sends what the model says before a tool call as its
  own message, replies to each message with its answer, reacts to an
  acknowledgment, and `/stop` stops every answer in progress.

## Consequences

The user can keep talking while Ditto works: a quick question is answered
while a long one runs, and a later message knows what is still in flight.
Answers can arrive in a different order from the questions; each replies to
its own message. Four runs bound the load on the machine and the provider.
The instructions grew by 244 bytes (2,066 to 2,310), constant across turns.
Lines said before a tool call are not shown again after a web page reload;
the answer is.

## Alternatives

- **A queue per session.** Still makes the next message wait.
- **A model call to classify every message.** Costs a call for "ㅇㅋ"; a
  closed list costs nothing, and the question rule keeps answers to
  questions as real messages.
- **Letting runs see each other's partial work.** Partial text is not a
  decided fact; the note names the work and its age, and the finished answer
  joins the thread.
- **Keeping the lines said before tool calls in history.** They narrate
  rather than decide; the answer carries what matters, at no extra bytes.
