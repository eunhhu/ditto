# Task 005.1 verification evidence

## Reviewed range

- Task 005 closing commit:
  `c38663d2e786f9ee9ecdf650b499f54a5c101371`
- Task 005.1 contract commit:
  `ad18b33bc1fb9ddd9d1e89cee1fcb388a4c70bf8`
- Task 005.1 implementation commit:
  `85fde0ad862220435f28a1effdc52bb7f2136183`
- Implementation tree: `8939ab9fa482ffeddc1346fbd98eb54fddf6a79e`
- Adversarial closure commit:
  `cb4c71ecfb64bc69445fc91ed49263d896131676`
- Final tested code tree: `f05af74d707498040004fb6d77b3aba6b467f439`
- Governing decision:
  [ADR 0012](../../adr/0012-canonical-capability-invocation.md)

The closing commit that contains this manifest owns only verified task-state
and evidence updates. The three Task 005.1 commits above own the pre-merge
contract correction, implementation, and final direct card/preflight regression
coverage respectively.

## Exit-criterion audit

| Contract claim | Inspectable implementation | Regression evidence |
| --- | --- | --- |
| Replay evidence is not live authority | `ExecutionEpochEvidence` remains bounded, serializable, and deserializable. `LiveExecutionEpoch` and `InvocableCapabilityBinding` have private fields and implement neither `Serialize` nor `Deserialize`; only the live epoch can issue a binding. `InvocationCompiler` accepts only that binding. | Nine capability compile-fail doctests include public-literal, serialization, deserialization, and evidence-as-compiler-input failures. `live_binding_projects_replay_evidence_without_round_trip_authority` round-trips evidence while checking the binding, card, revision, and epoch identity. |
| Card, schema, manifest, revision, and epoch are one live selection | A binding owns the exact epoch ID, model-visible card, complete manifest and schema, and digest-bearing revision. Paging derives all fields together; compilation rederives the card and every revision component before normalization and checks the deriver revision again after derivation. The kernel builds its selected payload and model schema only from that binding and its live epoch's evidence. | `live_epoch_rejects_every_revision_mismatch` covers capability ID, version, manifest, schema, and deriver changes. The evidence test compares binding epoch/card/revision to the live epoch projection. Task 003 replay corruption tests still reject forged selected-capability evidence. |
| Invocation validation uses a closed, exact profile | Ditto Invocation Schema Profile V1 admits only its enumerated boolean, annotation, type, equality, exact-integer, string, array, and object semantics. Unknown keywords, references, combiners, floating-point `number`, and syntactic floats for `integer` fail closed. Exact admitted integers use `i128`; recursive equality is representation-sensitive. | Profile conformance tests distinguish `1` and `1.0` for `const`, `enum`, and `uniqueItems`, check `multipleOf` and integers beyond 2^53, reject unsupported semantics, and reject `artifact.read` `length = 1.0`. |
| Structural work is bounded before recursion | An iterative complete-value preflight charges serialized bytes, JSON depth, and node work before either the provider-neutral structural validator or closed-profile validator recurses. Raw and normalized argument instances receive the same envelope before evaluation. | `structural_depth_is_rejected_by_iterative_preflight` and `structural_work_is_rejected_by_iterative_preflight` exercise the fixed schema limits. Existing normalized-argument and oversized-wire tests remain green. |
| Task 003 invalid-argument and replay behavior is preserved | The live turn pages one exact binding, compiles through the closed profile, and creates a turn-local epoch authorizer. Raw profile failures retain the existing `invalid_arguments` tool result; no permit or bounded artifact read follows. Event versions, result shapes, continuation, terminal status, and replay projection are unchanged. | All 38 `read_only_turn` tests pass. `malformed_negative_and_excessive_arguments_are_error_results_and_continue_without_read` now includes `length = 1.0` and verifies the stable error projection plus continuation. |
| Authorization state cannot become daemon-lifetime state | `InvocationAuthorizer` borrows one `LiveExecutionEpoch`, has a fixed epoch expiry, rejects other and expired epochs, and caps permits and approval requests at that expiry. `KernelInner` no longer owns an authorizer; each live turn constructs and drops its own ledger. | `authorizer_rejects_other_or_expired_epochs` and the static-policy expiry test cover epoch isolation. Source inspection confirms the daemon-owned authorizer field and constructor were removed. |
| Atomic lease and idempotency guarantees survive the correction | Invocation-ID binding, retry lookup, all lease checks, decrement, and outcome insertion remain under one mutex inside the epoch authority window. Failure and approval consume nothing; one success consumes once; identical retry returns the stored outcome; a conflicting digest fails closed. | Seven policy integration tests include failed authorization, approval, successful retry, ID/digest conflict, and a barrier-synchronized one-call race with exactly one permit winner. |
| One permit can mint at most one future dispatch claim | `ExecutionClaim` is private-field, non-deserializable, non-cloneable, and issued only by atomic `claim_execution`. It binds the epoch, permit ID, invocation digest, claim time, and expiry. Future workers are required by contract to consume it by value; no worker exists. | Five policy compile-fail doctests cover sealed permits and claim literal/deserialization/clone failures. `permit_issues_exactly_one_non_cloneable_execution_claim` proves the second claim fails closed. |
| The slice stops before external effects or the next task | The changed authority modules add no process, network, SSH, credential, filesystem mutation, approval fulfillment, worker, verifier, or completion emitter. The only executor remains the pre-existing bounded read-only Task 003 path. The compact session-index task was not started. | A scoped added-line search for process/network/SSH/`task.completed` symbols returned no match; the implementation diff is confined to capability, policy, kernel migration/tests, ADR, and agent task records. |

## Replayed commands and verdicts

All commands ran from the repository root on 2026-09-01.

| Command | Verified result |
| --- | --- |
| `rtk cargo test --locked -p ditto-capability -p ditto-policy -p ditto-kernel --all-features` | Passed: 121 tests across the focused unit, integration, and compile-fail suites. |
| `rtk ./scripts/agent-check.sh` | Passed: tracked canary, formatting, strict workspace/all-target/all-feature Clippy, all workspace tests, and all compile-fail doctests. |
| `rtk cargo +1.88.0 check --locked --workspace --all-targets` | Passed for every workspace crate and target. |
| `rtk git diff --check` | Passed. |

The scoped added-line search below returned no match:

```bash
rtk git diff c38663d..cb4c71e -- crates/capability/src/invocation.rs crates/capability/src/schema_instance.rs crates/policy/src/lib.rs | rtk rg -n '^\+.*(std::process|Command::|TcpStream|reqwest|ssh|task\.completed)'
```

No capability worker, subprocess capability, network, model, credential,
provider, SSH, approval-fulfillment, file-mutation, or billable operation ran.
The test suite used only existing local temporary fixtures.

## Reproducibility identities

- `crates/capability/src/invocation.rs` blob:
  `bdf5eadbb979dcfba8678215b280de7cc486944b`
- `crates/capability/src/schema_instance.rs` blob:
  `6d333e223e41300aef9062f9d062f64a3b981185`
- `crates/capability/src/lib.rs` blob:
  `4cd7af7ba1e7dd5d7184e6e8e02d7c9e29e00c71`
- `crates/policy/src/lib.rs` blob:
  `5f16da1972c6c524beff694f205e889b35b3e0d8`
- `crates/policy/tests/authorization.rs` blob:
  `2a9e556438580a5229d16b0ffdcad0b64b164532`
- `crates/kernel/src/turn/run.rs` blob:
  `b417b574d5e5c00836869983bb43ab4ea7b17077`
- `crates/kernel/src/turn/replay.rs` blob:
  `9a3543fe391537d6c650f4458cd47d5842e11b1e`
- `crates/kernel/tests/read_only_turn.rs` blob:
  `8c7c1197049ef6aa146127bd3aa6e155cad71f34`
- `Cargo.lock` blob:
  `3ef4861bf38046e5ea2ce3dff8b85e37ee90d170`
- RTK: `0.46.0`
- Cargo: `1.88.0 (873a06493 2025-05-10)`
- Rust compiler: `1.88.0 (6b00bc388 2025-06-23)`

Local `.omo` and `.surf` contents remain untracked and are not evidence inputs.
