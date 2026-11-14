# Improvement Patches

Self-improvement is an evidence-gated promotion pipeline, not background skill generation.

## Learning layers

| Layer | Meaning | Default lifetime |
|---|---|---|
| Trace | Raw events and outputs | retention policy |
| Claim | Fact, preference, or environment state | scope and validity |
| Fragment | Small situational lesson | expiring |
| Recipe | Repeatedly verified procedure | versioned |
| Capability | Executable implementation | strongly gated |

Most experience remains a trace.

## Candidate signals

Deterministic detectors may open a candidate after repeated user corrections, retrieval misses, argument errors, retries, approval repetition, token/latency regression, or completion-verifier mismatch. No signal means no model call, and a detector that can produce the candidate itself makes none.

## Patch envelope

```yaml
kind: retrieval_patch
target: device.process.run
base_hash: 19a21f
operations:
  - add_positive_example: restart a service on the home server
  - add_negative_example: reboot the entire machine
evidence:
  - run: 8241
  - run: 9120
expected_metric:
  tool_retrieval_retry_count: -1 or better
scope: global
expires_after: 90d
```

## Promotion states

```text
candidate → deduplicated → validated → replay-tested → shadow
          → canary → active → deprecated | rolled-back | archived
```

A permanent promotion normally requires at least three independent cases or explicit user approval, no unrelated replay regression, a bounded token/latency cost, no widened effect scope, an expiry, and a rollback path.

Below permanence, a change that widens no effect and can be undone applies on its own as an expiring candidate and is shown in the learning ledger ([ADR 0049](../adr/0049-learned-behavior-decisions.md)). A change that widens a capability, an effect or a resource still needs the user's approval. Rollback is the user's undo, plus automatic retirement when the item's target is forgotten or superseded or when it expires unused.

A model call that drafts a patch is allowed only as a retrospection that a detector wakes, under ADR 0049's ceiling.

## Immutable surface

The kernel binary, root policy, credential store, patch evaluator, audit log, pinned context, active model-provider settings, and evaluator results are not self-editable.
