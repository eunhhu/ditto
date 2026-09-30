# Task 022: Assistant instructions and local time

Contract and evidence in one file. Decision:
[ADR 0026](../../adr/0026-assistant-instructions-and-local-time.md).

## Problem

The model's instructions were written for the harness ("model strategy
component"). They never explained the memory block, never gave the date, and
never said that the model cannot save memories. So a model could promise to
remember something with no way to do it.

## Contract

- New turns use payload version 4. Its instructions are three fixed
  personal-assistant segments (role and language, memory block and history,
  limits with `/remember` and schedules) plus
  `Current local time: <weekday>, <date>, <HH:MM> (UTC±HH:MM).`
- The time is the input's recorded acceptance time in the host zone. The
  offset is recorded in `context.compiled` as `utc_offset_minutes` (±14 h),
  and replay recomputes all four segments from it.
- Versions 1–3 keep their frozen legacy instructions without an offset; mixed
  forms fail replay.
- `ditto chat` accepts `/remember <fact>`, matching the web app and Telegram.

## Exit criteria

1. A version-4 request states the local time that follows from the recorded
   offset and input time, and replays.
2. A forged, out-of-range or missing offset, altered time text, and mixed
   legacy and version-4 forms fail replay.
3. `ditto chat` saves a memory that reaches the next request and keeps its
   thread behavior.
4. The canonical gate passes.

## Evidence (2026-09-30)

- Kernel: the new instruction test recomputes the time segment independently
  from the recorded offset and the input's `recorded_at`, checks the four
  segments and replays. An offset shifted by an hour, an offset of 15 hours,
  a removed offset and a replaced time text each fail replay.
- The relabeling test now pins the legacy wording verbatim. Versions 1, 2 and
  3 replay with legacy instructions and no offset. Version 3 with version-4
  instructions, version 4 with legacy instructions, and unknown versions fail.
- Built-CLI `ditto chat` scenario (new, in the gate), in one piped session:
  - "hello" was answered, `/remember I like green tea` saved a memory;
  - the follow-up request carried the previous exchange (three messages) and
    the new memory in its context;
  - after `/new` the next request carried only its question.
- The first gate run failed in the corpus harness (3 of 21 tests): it pinned
  payload version 3. The harness now expects version 4, and its mutation
  cases use the module constant.
- `./scripts/agent-check.sh` then passed on the final tree in 1 min 46 s
  (warm caches, pinned Rust 1.88.0): 532 Rust tests (525 workspace including
  doctests, seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer
  cases and both smokes. Local log `target/task022/gate.log`, SHA-256
  `4e1c10fe3025889c1f0087e8b1ec4262d4bddb41b6f73f2aa9da7c795f117af5`.
- Not run: an answer-quality comparison with a real model (none installed;
  no paid calls). The change is wording and time; its effect on answers is
  unmeasured.
