# Task 016.1: Personal-scale recall and typed turn failures

Contract and evidence in one file. Decision:
[ADR 0021](../../adr/0021-complete-personal-context-and-typed-turn-failures.md).

## Problem

A 2026-09-30 review probed the real V1 compiler with kernel-shaped memories.
For `What is my meeting preference?` it selected four unrelated memories and
not `I prefer afternoon meetings`; paraphrased questions (Task 017 P1–P3)
selected nothing relevant. Causes: exact-token overlap only, function words
(`my`, `is`) counting as overlap, and the fixed query text
`local content read` matching memories about reading. Tasks 015/016 used
literal queries without function words and could not see this. Replay also
matched validator-derived failure text by string grammar.

## Contract

- Run turns select context with `ContextSelection::CompleteSet` on the request
  text alone: when required context plus every eligible memory fits the
  selection budget, all are sent (lexical matches first, then by ID, reason
  `complete-set`); otherwise the positive-overlap fallback applies, without
  function words. Scope, supersession, provenance, validity and budgets are
  unchanged.
- New turns write payload version 2; a turn never mixes versions. Replay reads
  versions 1 and 2 with the rules of the recorded version; run status reads
  both.
- Version-2 validator-derived failures carry a closed `TurnFailureReason`;
  replay validates the reason and stage, not the wording. Version-1 failures
  carry none.
- The Task 016 corpus harness moves to schema 3: expected capsules come from
  the frozen seeds and the frozen complete-set rule, never from observations.
  The gate's longer profile uses twelve noise memories so that both modes run.

Not in scope: semantic retrieval, a larger budget, answer-quality grading,
multi-turn conversation or notification delivery.

## Exit criteria

1. A regression that fails on the pre-change code shows paraphrased questions
   receiving the complete current memory set, with superseded and other-session
   memories absent.
2. Context unit tests cover complete inclusion, the budget fallback, required
   context order and forged `complete-set` receipts.
3. Replay tests cover version-1 relabeled turns, mixed versions, unsupported
   versions and forged or missing typed reasons.
4. Corpus schema 3 passes in both modes; the canonical gate and the Rust 1.88
   check pass.

## Evidence (2026-09-30, Linux/aarch64 Raspberry Pi 5, Rust 1.88.0)

- **RED.** The new kernel test
  `paraphrased_personal_questions_receive_the_complete_current_memory_set`,
  copied onto pre-change commit `d3cadca`, failed at its first question:
  `left: 0, right: 6` (no memory reached the model). The companion over-budget
  test passed there, as expected for a boundary test. Local log
  `target/task016-1/red-paraphrase-pre-change.log`, SHA-256
  `7ea806874b1ad056b3f74e3bf2d66eeb68a1c83f28b92d32785d630b96597ca4`.
- **GREEN.** `ditto-context` 55 unit tests (six new) and two doctests; kernel
  `read_only_turn` 60 tests (four new, three updated to the version-2
  contract), including all three driver-contract reasons end to end.
- **Corpus smoke** (`--history-size 12 --samples 2`, schema 3): zero-noise
  profile in complete mode (360 estimated tokens): exact set/order 10/10,
  Recall@2 12/12, returned precision 12/50, stale and scope leaks 0/10.
  Twelve-noise profile in fallback mode (1,236 tokens): exact set/order 10/10,
  precision 12/12, irrelevant leaks 0/38, noise leaks 0/120. Quality harness
  tests 21/21 and Task 014 baseline tests 8/8 passed.
- **Gate.** `./scripts/agent-check.sh` passed on the final tree (git status and
  diff hashes unchanged from start to end): canaries, formatting, strict
  Clippy, 500 Rust tests (495 workspace including doctests, five built-CLI
  scenarios), 8 + 21 Python tests and both smokes, in 10 min 40 s. The Rust
  1.88 offline workspace/all-target check passed. Local logs
  `target/task016-1/gate.log` (SHA-256
  `56aeb418782fdcf52a73070d6c11469336972ca062b51a7d6df5e20bb9864624`) and
  `msrv.log` (`837f5f616d49749e080969acbb508916473aaec6225cdb5ff27f016a9c187470`).

No live provider, credential, network download or paid call was used. Answers
remain fixture text and unassessed.
