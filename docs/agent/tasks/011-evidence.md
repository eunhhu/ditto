# Task 011 verification evidence

Initial base: main `ef65fff624703a7ad886967dc93f27a7bdbd33c5` (Task 010 / PR #15).
Final integrated base: `cbc301d57cd5a5b196366fda9999aeef1642def2`, including the
separately merged dependency updates from PRs #8–#11.
Branch: `dev/task-011-model-sort`.
Contract: [Task 011](011-model-sort.md), [ADR 0018](../../adr/0018-user-scoped-model-sort.md).

## Tested implementation identity

These Git objects identify the tested implementation independently of later
documentation-only updates:

| Component | Git object |
| --- | --- |
| `crates/` tree | `9f129aca79d18a70cdcfc59a49f0e6f8051ec34c` |
| `apps/` tree | `07e1e4ec3de35a50f8d5c191ac87ec249f366586` |
| `scripts/` tree | `4deefcb3c1e66376e8a24d4183d0835fddcde905` |
| `capabilities/` tree | `587656a7bdb5587e2402e2e5227da23f7d7e2935` |
| `Cargo.lock` blob | `8becaa9b566177cc2040bf75682bbbc2329d0e8a` |

## Replayable checks

| Contract | Evidence |
| --- | --- |
| Model → real process → verified artifact → read → answer | `crates/kernel/tests/read_only_turn/model_sort.rs::model_sort_continues_with_verified_output_and_survives_restart`: initial reference/permission without file content, exact output, three model requests, no model task.completed, replay and identical restart retry |
| No inferred or expanded authority | Same suite: absent permission omits sort schema and rejects dispatch; raw extra fields, other artifact and unapproved deduplication preserve the lease; allowed call executes once and subsequent call cannot erase the result |
| Retry and admission bounds | Same suite: missing/changed permission, exact bytes including final LF, NUL, 4097 lines and 65537 bytes; rejected input creates no event/model work |
| Cancellation and interruption | Same suite: cancellation after sort request and after claim, no new successful artifact; actual runtime loss while the post-sort model request waits, independent verified status and no reexecution |
| Independent durable result | Same suite: later provider-fixture failure preserves verified output after reopen; changed grant/request/start/result/root evidence fails closed; altered input/output objects fail status while pure replay needs no artifact access |
| Bounded migration/inspection | `crates/event-store/src/lib.rs::schema_four_sort_lookup_migrates_without_rewrite_and_bounds_exact_turn_work`: schema-3 migration retains the exact old record, query planner uses an indexed SEARCH without sort, unrelated records are excluded and a nine-row sentinel bounds lookup |
| Public trust boundary | daemon `runs::tests::http_sort_permission_cannot_supply_internal_authority_or_bypass_disabled_provider`: nested authority fields and missing required permission bit rejected; provider-disabled attachment admits no work |
| Actual CLI and daemon router | `runs::tests::built_cli_model_sort_permission_retry_and_disabled_status`: file/permission flags, real sort output, status, exact retry, changed-permission conflict, required-flag validation and provider-disabled restart inspection |
| Production binaries | `scripts/smoke-agent-run.py`: disabled attachment creates no artifact/admission; existing memory, SIGTERM/SSE and restart checks. `scripts/smoke-local-sort.py`: provider-free sort still verifies with one dispatch and no retry execution |
| Existing contracts | All legacy artifact-read/replay, explicit-sort authority/process/verifier, memory/context, capability, policy and model suites in the canonical gate |

Local macOS/aarch64 commands passed:

```bash
rtk ./scripts/agent-check.sh
rtk cargo +1.88.0 check --locked --workspace --all-targets
rtk cargo build --locked -p ditto-daemon -p ditto-cli
rtk proxy python3 scripts/smoke-agent-run.py
rtk proxy python3 scripts/smoke-local-sort.py
rtk ./scripts/agent-canary.sh
rtk git diff --check
```

The canonical gate passed formatting, strict all-target/all-feature Clippy,
**416 unit/integration tests + 25 compile-fail doctests + 3 required built-CLI
tests = 444 tests across 45 suites**. Initially ignored CLI tests are explicitly
run after the gate builds the binary. The first gate found a test assertion
requiring EventRecord equality; the test now compares its complete serialized
record. The final gate is green. No skipped check is counted as passed.

The first Linux PR run (33998585130) exposed independently merged main's sha2
0.11 update: its output container no longer implements LowerHex. Integrating
that main reproduced the compiler failure locally. The compatibility fix keeps
lowercase, zero-padded hashes with bounded byte encoding and reuses the existing
capability encoder. Three known-vector tests in artifact-store, artifact-sort
and capability prove persisted references/package digests retain their format.
The final canonical/MSRV checks and production smoke use the integrated
versions of sha2, toml, base64 and tower-http; those updates are preserved.

Review focused on live canonical authority, lazy disclosure, shared ownership
and cancellation, at-most-once dispatch, evidence consistency, independent
result inspection, old-trace compatibility and bounded storage lookup. It was
performed locally; no separate agent or web-chat model review was requested.
No live/paid provider call or provider credential access occurred. Linux CI is
reported separately by the PR; local macOS success does not establish it.

## Limits

This enables one closed process profile with explicit submission-time permission.
It does not add pending-approval fulfillment, arbitrary programs, scheduling,
automatic improvement or broader model-task verification. The existing trusted
OS sort containment/host-crash limits remain. Pure replay checks structural
evidence; status hashes and verifies bounded artifacts. Source corruption
returns unavailable storage rather than a verified result. Status performs
bounded reads, not a claim of zero work. No new crate, dependency or service was
added; event-store schema 4 is a forward migration with one partial index.
No RSS, latency, live-model quality or zero-overhead result is claimed.
