# ADR 0026: Assistant instructions and local time

Status: accepted for Task 022. Adds turn payload version 4 after ADR 0022.

## Context

Every model request opened with two sentences written for the harness: "You
are Ditto's model strategy component…". They never told the model that it
serves one person, what the context block holds, that it cannot save memories
or set reminders, or what time it is. Real models, especially small local ones,
therefore misread the memory block. They could not answer "what day is it" or
"tomorrow", and they could promise to remember something with no way to do
it. That last case is a fake success path.

## Decision

Turn payload version 4 replaces the system instructions of new turns with
three fixed assistant segments and one time segment:

1. Ditto is the user's personal assistant on their own computer; be helpful,
   concise and honest, and answer in the language of the latest message.
2. `DITTO_CONTEXT_V1` lists what the user explicitly asked Ditto to remember,
   with provenance. User-asserted items are facts about this user unless the
   conversation corrects them. Earlier messages precede the latest one.
3. The model cannot save or change memories, set reminders, browse the web or
   act outside the conversation except through supplied tools, and never
   claims an action it did not take. The user saves a memory with
   `/remember <fact>` and creates reminders in Ditto's schedules.
4. `Current local time: Wednesday, 30 September 2026, 14:04 (UTC+09:00).`

The time is the input's durable acceptance time in the host's local zone
(`TZ` selects another). `context.compiled` records the UTC offset in minutes
as `utc_offset_minutes` (at most 14 hours either way). Replay recomputes all
four segments from that offset and the input's recorded time, and rejects any
other text. Versions 1–3 keep the frozen legacy instructions and carry no
offset; a turn never mixes the two forms. The time segment comes last, so the
fixed segments stay a stable prefix for provider caching.

`/remember <fact>` now works in every client: the web app, Telegram and, new
here, `ditto chat`.

## Rejected alternatives

- Editing the legacy text in place would break replay of recorded turns.
- Stating UTC only would make "today" and "tomorrow" wrong for most users.
- Reading the zone again during replay would let a later zone change alter
  history; the recorded offset fixes it.
- Letting the model save free-text memories is a separate decision: model
  output must not be recorded as a user assertion (see NEXT).

## Consequences and rollback

Answers can use the date and the memory block correctly, and the model is
told plainly what it cannot do. Instruction wording is part of the durable
contract: changing it needs a new payload version. Rollback stops writing
version 4; recorded version-4 turns still need this replay rule.
