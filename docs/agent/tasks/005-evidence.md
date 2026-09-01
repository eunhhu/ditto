# Task 005 verification evidence

## Reviewed range

- Base commit: `99963faa6fe96dfee5fdc4eb4ae5eb84b57437df`
- Contract commit: `f1177bc`
- Canonical invocation and policy commit: `8215ea0`
- `artifact.read` migration commit:
  `590e455d048492783d6aa4f3522fad8867b08d1c`
- Migration tree: `9c795ac198521d407d464be5b5f04a6d7287f38f`
- Governing decision:
  [ADR 0012](../../adr/0012-canonical-capability-invocation.md)

The closing commit that contains this manifest owns only the verified task-state
updates. The three coherent commits above own the contract, authority boundary,
and reference-capability migration respectively.

## Exit-criterion audit

| Contract claim | Inspectable implementation | Regression evidence |
| --- | --- | --- |
| Authority-free model wire and sealed outputs | `UntrustedToolCall` has exactly call ID, capability ID, and raw arguments and denies unknown fields. `CanonicalInvocation` and `InvocationPermit` have private fields, no unchecked constructor, and no `Deserialize` implementation. | `untrusted_wire_rejects_every_authority_field` rejects effect, resource, device, program, placement, lease, approval, verification, permit, and idempotency inputs. Four compile-fail doctests reject struct literals and `DeserializeOwned` for both sealed types. |
| Exact epoch/revision resolution | An invocable `ExecutionEpoch` entry binds capability ID/version, canonical manifest digest, complete schema digest, and deriver revision. The compiler includes the exact epoch ID in invocation identity and digest and rechecks the deriver revision after derivation. Legacy card-only epochs remain replay-compatible but cannot authorize live work. | `every_epoch_revision_component_is_exact`, `bound_epoch_revision_evidence_round_trips_but_legacy_cards_do_not_authorize`, and `successful_two_request_continuation_persists_exact_epoch_schema_history_and_replays`. The replay corruption suite rejects a forged durable deriver revision. |
| Bounded raw validation, normalization, and derivation | The capability crate structurally validates Draft 2020-12 schema, evaluates raw and normalized instances under fixed byte/depth/work/pattern limits, and admits only the registered `artifact.read` deriver's bounded I/O-free result. Derived effects must satisfy both manifest bounds and resources must match a declared family. | Schema evaluator tests cover exact object fields, patterns, ranges, local references, combiners, recursive-depth exhaustion, unsupported runtime keywords, and integral-number semantics. `compile_binds_exact_epoch_revision_and_revalidates_normalized_arguments` rejects a normalized value outside the schema; `derived_effect_must_satisfy_both_manifest_bounds` rejects both directions. |
| Typed canonical resource authority | Artifact resources validate exact lowercase SHA-256 identity. Lexical path primitives require NFC and component-aware containment while rejecting controls, backslashes, empty/dot/parent components, relative/absolute ambiguity, and filesystem-root authority. No path executor is connected. | `canonical_paths_reject_traversal_siblings_and_unicode_aliases` rejects parent traversal, `/srv/ditto-secrets` as a sibling of `/srv/ditto`, and decomposed Unicode while accepting the intended child and NFC spelling. Artifact derivation tests assert the exact typed identity. |
| Atomic, idempotent authorization | One clone-shared mutex protects invocation-ID/digest binding, policy checks, lease decrement, decision insertion, and retry lookup. Policy receives only a sealed invocation and selects a trusted static policy or harness-side lease. | Five policy integration tests prove failed checks do not consume, one success consumes once, identical retry returns the same permit, conflicting digest fails closed, approval-required issues no permit and consumes nothing, static permits match one digest and expiry, and a concurrent one-call race yields exactly one permit. |
| Existing `artifact.read` path requires a static permit without Task 003 drift | The live kernel pages the exact revision, compiles the model call, establishes the existing same-scope high-water authorization, obtains a sealed no-approval static-policy permit, validates it against the invocation, and only then invokes the existing bounded read authority. Replay validates new additive revision evidence but accepts legacy epochs and performs no policy or artifact I/O. | All 38 `read_only_turn` tests pass, including valid continuation/replay, malformed arguments without reads, unauthorized references, cancellation checkpoints, exact event history, corruption rejection, reopen replay, and absence of `task.completed`. |
| Task stops before a new executor or external effect | The canonicalization and policy modules import no filesystem, network, process, SSH, or credential runtime. Task 005 adds no worker, approval fulfillment, credential resolution, file-writing capability, network capability, or completion emitter. | Scoped source search finds only existing `task.completed` rejection/check fixtures, never an emission in the Task 005 path. The existing read-only integration suite remains the sole execution reference. |

## Replayed commands and verdicts

All commands ran from the repository root on 2026-09-01.

| Command | Verified result |
| --- | --- |
| `rtk cargo test -p ditto-capability -p ditto-artifact-read -p ditto-policy -p ditto-kernel --locked --all-targets` | Passed: 118 tests across 8 suites. |
| `rtk cargo clippy -p ditto-kernel -p ditto-policy -p ditto-artifact-read -p ditto-capability --locked --all-targets -- -D warnings` | Passed with no warning or error. |
| `rtk ./scripts/agent-check.sh` | Passed: tracked canary, formatting, strict workspace/all-feature Clippy, and 344 tests across 36 suites. |
| `rtk cargo +1.88.0 check --locked --workspace --all-targets` | Passed for every workspace crate and target. |
| `rtk git diff --check` | Passed. |

No capability worker, subprocess capability, network, model, credential,
provider, SSH, approval-fulfillment, or billable operation ran. The test suite
used only the pre-existing local temporary artifact-read fixtures.

## Reproducibility identities

- `crates/capability/src/invocation.rs` blob:
  `a66242233215b6c435e28572c3a9080feb9082a6`
- `crates/capability/src/schema_instance.rs` blob:
  `7dd7e334e0857a070a320ae49fc9147de805b390`
- `crates/artifact-read/src/lib.rs` blob:
  `35ee34f68fdce33909767861be8a8b8194857707`
- `crates/policy/src/lib.rs` blob:
  `2ce0fb05897a52e66c6e7c3956dcebc5963d4358`
- `crates/policy/tests/authorization.rs` blob:
  `b9f2bb1042e3fc2fb7265932d07a927a156b0289`
- `crates/kernel/src/turn/run.rs` blob:
  `be2804b07c71d76108cbe616b49ce154de92ec72`
- `crates/kernel/src/turn/replay.rs` blob:
  `50ded836fd9f8a8934c78a5354acc8d1c664f610`
- `crates/kernel/tests/read_only_turn.rs` blob:
  `163f1bd87d8dd4b05001674d604af0d1f25588df`
- `Cargo.lock` blob:
  `3ef4861bf38046e5ea2ce3dff8b85e37ee90d170`
- RTK: `0.46.0`
- Cargo: `1.88.0 (873a06493 2025-05-10)`
- Rust compiler: `1.88.0 (6b00bc388 2025-06-23)`

Local `.omo` and `.surf` contents remain untracked and are not evidence inputs.
