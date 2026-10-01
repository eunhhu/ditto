# Task 030: Capability selection recorded by reference

Contract and evidence in one file. Decision:
[ADR 0030](../../adr/0030-capability-selection-by-reference.md).

## Problem

`capabilities.selected` recorded every offered builtin manifest in full, with
the card and revision derived from each, on every turn: 4,910 of the 8,837
journal bytes of a short agent turn after Task 029. Replay already required
the manifests to equal their packages, so the bytes restated the code.

## Contract

- Turn payload version 9 records `capabilities.selected` as `epoch_id` and
  `contracts`, each bound contract's revision in page order.
- Every builtin manifest equals its package; `artifact.read` now also compares
  its manifest digest with the package's, which covers `lifecycle`.
- Replay rebuilds the full selection from the packages and checks each
  recorded revision against them. Versions 1 to 8 keep the full form, and
  each version accepts only its own form.

## Exit criteria

1. A version-9 selection holds only its reference fields, and a different
   digest, deriver, epoch, an unknown contract or no contract is rejected.
2. The same turn with its selection in full replays as versions 7 and 8, where
   the manifest and card forgeries are still rejected.
3. Journal bytes per turn fall by the selection's share, with turn start
   unchanged.
4. The canonical gate passes.

## Evidence (2026-10-01)

- Kernel tests: the corruption test checks the version-9 fields and rejects a
  zeroed manifest digest, a changed deriver revision, another epoch ID, an
  unknown capability and an empty contract list; its manifest, retrieval,
  card and revision forgeries now run on the same turn converted to the full
  version-8 form, which replays. The relabel test replays one turn as versions
  1 to 8 with its selection in full and rejects the reference form as
  version 8 and the full form as version 9. The `web.fetch` test removes the
  fetch contract instead of its manifest.
- An `artifact.read` package with a retired lifecycle is rejected, and the
  packaged manifest equals the canonical contract.
- Checked red: with replay's revision comparison disabled, the zeroed digest
  replays and the corruption test fails.
- `measure-harness.py`, release build, same machine, against the Task 029
  build:
  - one short agent turn with three tools: `capabilities.selected` 4,910 →
    952 bytes, the whole turn 8,837 → 4,879 bytes;
  - with twelve memories, journal bytes per answer byte 18.34 → 13.39 for a
    200-delta answer and 5.32 → 4.33 for a 1,000-delta answer; work before
    dispatch 1.0 ms median in both; client to provider 2.20 → 2.08 ms; prompt
    prefix reuse unchanged at 96.75 %.
- `./scripts/agent-check.sh` passed on the unchanged final tree in 2 min 1 s
  (pinned Rust 1.88.0): 571 Rust tests (564 workspace including doctests,
  seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer cases and
  both smokes. Local log `target/task030/gate.log`, SHA-256
  `2eebf98916f4871cc4a001d75c3055c1b2e249925f3bc228d5e99d2506745ab9`. Two
  earlier runs on the same tree failed only in the baseline harness's
  30-second readiness check while another program on the host was repacking
  a repository and compiling (swap full): hashing the debug binaries before
  the first server start took 36 seconds.
