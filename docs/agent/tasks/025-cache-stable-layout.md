# Task 025: Cache-stable prompt layout (ADR 0028 Phase A)

Contract and evidence in one file. Decision:
[ADR 0028](../../adr/0028-thin-realtime-harness.md), Phase A; design in
[realtime-harness](../../design/realtime-harness.md).

## Problem

Only half of each prompt (median 50.6 %) was byte-identical to the previous
turn's, so provider prompt caches and llama.cpp/Ollama KV caches recomputed
the rest every turn. There were three causes:

- the complete-set capsule was ordered by relevance to each question;
- the history window slid by one exchange per turn past eight exchanges;
- the local time sat in the instructions, ahead of both.

## Contract (turn payload version 6)

- The instructions carry no time. The latest user message starts with
  `[Ditto: local time …]` and a blank line, and an added instruction says Ditto
  wrote this note.
- The capsule the model sees is in ID order (admission order for memories).
  Selection, receipts and the recorded capsule are unchanged.
- `web.fetch` is offered to every agent run while enabled; unlisted URLs get
  `permission_denied` without network I/O.
- The history window starts at a multiple of 8 finished `run_*` turns of the
  thread, holds at most 16 exchanges, and steps forward while over 24 KiB. An
  oversized final step keeps the newest that fit.
- Replay recomputes all four. Versions 1–5 replay under their own rules, and
  mixed forms fail.

## Exit criteria

1. Median prefix reuse between turns ≥ 90 % in `measure-harness.py`.
2. For different questions, requests share instructions, tools and capsule,
   and the history grows append-only between steps.
3. Replay rejects forged notes, offsets, windows and mixed-version forms.
4. The corpus harness measures the new presentation.
5. The canonical gate passes.

## Evidence (2026-09-30)

- `measure-harness.py`, release build, same machine: median prefix reuse
  50.6 % → 96.4 % (minimum 48.4 % → 96.0 %). Harness cost per turn is
  unchanged: 5 ms before dispatch, 75 µs per delta; phases B–D target those.
- New kernel test: three different questions produce requests with identical
  instructions, tools and capsule, with the earlier history and question
  preserved. With relevance ordering restored it failed (RED), then passed
  again.
- The stepped window:
  - kernel test: 16 exchanges fit one window from the thread's start, the
    17th steps it to exchange 8, and the next turn only appends; a window
    forged as sliding fails replay;
  - unit tests: step anchors for thread lengths 0–41, byte stepping, and the
    oversized final step.
- The time note: the kernel test recomputes the note from the recorded offset
  and input time. A forged note, a shifted, out-of-range or missing offset,
  and a time segment reinserted into the instructions all fail replay.
  Relabeling a version-6 turn as 4 or 5 fails, and versions 1–3 still replay
  in legacy form.
- The tool surface: a link-free message gets `web.fetch`, and a call to a
  local page is denied with zero requests to it; disabled fetch hides the
  tool; the sort tests list exact tool IDs.
- The corpus harness (report schema 4) expects ID-ordered capsules, checks
  the note and query of each request, and asserts that all ten complete-set
  queries sent a byte-identical capsule. A note paired with the wrong query
  is a new adversary.
- The built-CLI `ditto chat` and Telegram scenarios pass with a model double
  that answers the user's words after the note.
- The first gate run failed in the offline baseline: its model double blocked
  only on a message equal to `baseline block`, so the interrupted-run case
  finished instead. The double now matches the words after the note, and the
  smoke passed alone. `./scripts/agent-check.sh` then passed on the unchanged
  final tree in 1 min 54 s (warm caches, pinned Rust 1.88.0): 547 Rust tests
  (540 workspace including doctests, seven built-CLI scenarios), 8 + 21 Python
  tests, 12 web renderer cases and both smokes. Local log
  `target/task025/gate.log`, SHA-256
  `ea5ea277310df266ca66f872614f5be38aa06288f029b4502fccfaba0bff8934`.
