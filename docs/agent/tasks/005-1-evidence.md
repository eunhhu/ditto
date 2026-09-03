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
- Final review contract commit:
  `ddf9a6cccaefd016b1f0775b6292fc9d4cb0ea28`
- Final review implementation commit:
  `98e8f676fc325bf4400aae324c2447e56098bcdb`
- Normalized-output work-preflight test commit:
  `7b5e4b9528c17d08953d2de1d1bb8ca6bf824f90`
- Final locally and independently tested code tree:
  `d3ba3e14e136921106a5d5431f1abc3530ecf64a`
- Governing decision:
  [ADR 0012](../../adr/0012-canonical-capability-invocation.md)

The closing commit that contains this manifest owns only verified task-state
and evidence updates. The six implementation commits above own the original
pre-merge correction and its final review closure; the last two add no worker,
external effect, or compact session-index work.

## Exit-criterion audit

| Contract claim | Inspectable implementation | Regression evidence |
| --- | --- | --- |
| Replay evidence is not live authority | `ExecutionEpochEvidence` remains bounded, serializable, and deserializable. `LiveExecutionEpoch` and `InvocableCapabilityBinding` have private fields and implement neither `Serialize` nor `Deserialize`; only the live epoch can issue a binding. `InvocationCompiler` accepts only that binding. | Thirteen capability compile-fail doctests include public-literal, serialization, deserialization, non-cloneable ticket, and evidence-as-compiler-input failures; six policy doctests include duplicate ticket consumption and sealed permit/claim failures. `live_binding_projects_replay_evidence_without_round_trip_authority` round-trips evidence while checking the binding, card, revision, and epoch identity. |
| Card, schema, manifest, revision, and epoch are one live selection | A binding owns the exact epoch ID, model-visible card, complete manifest and schema, and digest-bearing revision. Paging derives all fields together; compilation rederives the card and every revision component before normalization and checks the deriver revision again after derivation. The kernel builds its selected payload and model schema only from that binding and its live epoch's evidence. | `live_epoch_rejects_every_revision_mismatch` covers capability ID, version, manifest, schema, and deriver changes. The evidence test compares binding epoch/card/revision to the live epoch projection. Task 003 replay corruption tests still reject forged selected-capability evidence. |
| Invocation validation uses a closed, exact profile | Ditto Invocation Schema Profile V1 admits only its enumerated boolean, annotation, type, equality, exact-integer, string, array, and object semantics. Unknown keywords, references, combiners, floating-point `number`, and syntactic floats for `integer` fail closed. Exact admitted integers use `i128`; recursive equality is representation-sensitive. | Profile conformance tests distinguish `1` and `1.0` for `const`, `enum`, and `uniqueItems`, check `multipleOf` and integers beyond 2^53, reject unsupported semantics, and reject `artifact.read` `length = 1.0`. |
| Structural and equality work is bounded before recursion or canonical projection | An iterative complete-value preflight charges serialized bytes, JSON depth, and node work before recursive profile validation or canonical JSON projection. Raw and normalized instances receive the same envelope. Recursive `const`, `enum`, and `uniqueItems` equality charges every compared node and compares `serde_json::Number` values directly. | `structural_depth_is_rejected_by_iterative_preflight`, `structural_work_is_rejected_by_iterative_preflight`, `untrusted_and_normalized_arguments_are_preflighted_before_canonical_projection`, and `nested_unique_items_charge_recursive_comparison_work` cover fixed envelope and evaluation exhaustion with the exact typed errors. |
| One live epoch can create only one authorization ledger | `LiveExecutionEpoch` enforces `Paging -> AuthorizationSealed` and issues one private-field, non-cloneable, non-wire `EpochAuthorizationTicket`. `InvocationAuthorizer` consumes it into one `Arc`-owned ledger; cloned handles share its mutex and claim markers. No public or serialized path can recreate the ticket. | `live_epoch_issues_one_ticket_and_never_rearms_paging` rejects second issue and both page-in forms before and after ticket drop. `dropping_the_authorizer_does_not_rearm_epoch_ticket_issuance` covers authorizer drop. Compile-fail doctests reject ticket clone/serde/literal and moving one ticket into two authorizers. |
| Task 003 invalid-argument and replay behavior is preserved | The live turn pages one exact binding, compiles and normalizes raw `artifact.read` arguments exactly once, then decodes the compiler-sealed normalized value. Raw envelope/profile failures retain the existing typed tool result; no permit or bounded artifact read follows. Event versions, result shapes, continuation, terminal status, and replay projection are unchanged. | `compile_binds_exact_epoch_revision_and_revalidates_normalized_arguments` counts exactly one compiler normalization. All 38 `read_only_turn` tests pass; `malformed_negative_and_excessive_arguments_are_error_results_and_continue_without_read` covers malformed reference, negative/excessive ranges, and `length = 1.0` with stable codes and continuation. |
| Authorization state cannot become daemon-lifetime state | The ticket-owned `InvocationAuthorizer` has a fixed epoch expiry, rejects other and expired epochs, and caps permits and approval requests at that expiry. `KernelInner` owns no authorizer; each live turn constructs and drops the sole ticket-backed ledger. | `authorizer_rejects_other_or_expired_epochs` and the static-policy expiry test cover epoch isolation. Source inspection confirms the daemon-owned authorizer field and old repeated `for_epoch` constructor are absent. |
| Atomic lease and idempotency guarantees survive the correction | Invocation-ID binding, retry lookup, all lease checks, decrement, and outcome insertion remain under the sole shared mutex inside the epoch authority window. Failure and approval consume nothing; one success consumes once; identical retry returns the stored outcome; a conflicting digest fails closed. | Eight policy integration tests include failed authorization, approval, successful retry, ID/digest conflict, a barrier-synchronized one-call race with exactly one permit winner, authorizer-drop closure, and shared-handle claim enforcement. |
| One permit can mint at most one future dispatch claim | `ExecutionClaim` is private-field, non-deserializable, non-cloneable, and issued only by atomic `claim_execution`. It binds the epoch, permit ID, invocation digest, claim time, and expiry. Future workers are required by contract to consume it by value; no worker exists. | Five policy compile-fail doctests cover sealed permits and claim literal/deserialization/clone failures. `permit_issues_exactly_one_non_cloneable_execution_claim` proves the second claim fails closed. |
| The slice stops before external effects or the next task | The changed authority modules add no process, network, SSH, credential, filesystem mutation, approval fulfillment, worker, verifier, or completion emitter. The only executor remains the pre-existing bounded read-only Task 003 path. The compact session-index task was not started. | A scoped implementation search for process/network/SSH/`task.completed` symbols returned no match; the implementation diff is confined to capability, policy, kernel migration/tests, ADR, and agent task records. |

## Replayed commands and verdicts

All commands ran from the repository root on 2026-09-01.

| Command | Verified result |
| --- | --- |
| `rtk cargo test -p ditto-capability` | Passed: 57 tests across unit and compile-fail suites. |
| `rtk cargo test -p ditto-policy` | Passed: 14 tests across integration and compile-fail suites. |
| `rtk cargo test -p ditto-kernel` | Passed: 60 tests across unit, integration, and compile-fail suites. |
| `rtk ./scripts/agent-check.sh` | Passed: tracked canary, formatting, strict workspace/all-target/all-feature Clippy, all workspace tests, and all compile-fail doctests. |
| `rtk cargo +1.88.0 check --locked --workspace --all-targets` | Passed for every workspace crate and target. |
| `rtk git diff --check` | Passed. |
| PR #6 Actions run [`33522132273`](https://github.com/eunhhu/ditto/actions/runs/33522132273) on `7b5e4b9528c17d08953d2de1d1bb8ca6bf824f90` | Passed independently: `msrv` in 22 seconds and `rust` repository gate in 1 minute 31 seconds. |

The scoped implementation search below returned no match:

```bash
rtk rg -n '(std::process|Command::|TcpStream|reqwest|ssh|task\.completed)' crates/capability/src/invocation.rs crates/capability/src/schema_instance.rs crates/policy/src/lib.rs
```

No capability worker, subprocess capability, network, model, credential,
provider, SSH, approval-fulfillment, file-mutation, or billable operation ran.
The test suite used only existing local temporary fixtures.

## Reproducibility identities

- `crates/capability/src/invocation.rs` blob:
  `5053772f7db3af3864314c3d6bad05083e46e83f`
- `crates/capability/src/schema_instance.rs` blob:
  `cfb7895d61ff17f3f88c33f7eeb299347260b546`
- `crates/capability/src/lib.rs` blob:
  `296e2bcba533c345c0e2c38defd70dab9df56747`
- `crates/policy/src/lib.rs` blob:
  `b6f174fe8b29e94d6f52ee6e3d00aadde02bef92`
- `crates/policy/tests/authorization.rs` blob:
  `988c6c1a1a946b96a3c6f284021c2616dd5066ab`
- `crates/kernel/src/turn/run.rs` blob:
  `2cabc7224711f9b388c3b9063021aaf4c6887865`
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
