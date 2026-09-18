# Task 016 verification and offline corpus evidence

## Contract and implementation

[Contract](016-personal-task-corpus.md),
[harness](../../../scripts/personal-quality.py),
[tests](../../../scripts/test-personal-quality.py),
[raw default report](016-personal-task-corpus.json).

Schema 2 extends Task 015's harness with the exact seven seeds, five queries,
ordered expectations and exclusions frozen in the task contract. Samples now mean
repetitions **per query**. The real-process regression asserts literal query
strings, exact durable input/conversation and literal expected summaries/order
independently of the harness's corpus constant. Actual CLI save/correct, kill,
restart, paginated listing and run are used throughout. No Rust or production
surface changed. Task 015's observer and independent call log are unchanged.

Matching requires exact metadata, original user-input provenance and absent
validity bounds. Capsules are compared both as parsed values and exact serialized
bytes using the ContextCapsule struct's field order. Durable input, query,
session, task, turn, event identity/sequence/causal chain, request index, model
request, call log and observation must reconcile. Cardinality/identity checks
precede pairing. Individual requests must pass; aggregation cannot hide a
failure. Raw metrics retain numerators, denominators, null reasons and
contributors. No combined agent-quality score is produced.

The report retains all seed input/memory events, expected items, post-restart
events, each corresponding durable model/input event, independent call IDs and
exact observed capsules. A save response's event ID identifies its memory
record; provenance separately resolves the original input ID. These identities
remain inspectable after store cleanup.

## TDD evidence

Before implementation, with offline build settings below:

| Command | Exit and observed RED |
| --- | --- |
| `python3 scripts/test-personal-quality.py AssessmentTests` | 1; six tests, 26 subtest errors: the generalized three-argument assessment API and `reconcile_observations` did not exist |
| `python3 scripts/test-personal-quality.py WorkflowTests.test_corpus_workload_replays_all_five_queries_after_restart` | 1; actual saves, correction, restart and runs completed, then literal request-count assertion failed: `2 != 10` |

Logs are `target/task016/red-assessment.log` and `red-workflow.log`. The initial
workflow attempt was blocked by sandbox socket creation (`PermissionError`),
preserved separately as `red-workflow-sandbox.log`. The same offline command was
rerun with approved local loopback access, producing the coverage RED above.
The failures establish missing corpus measurement, not a runtime retrieval bug.

Supplemental review regressions exposed malformed-record diagnostics
(`red-hardening.log`; that iteration also corrected a test exception-context
error), then an ignored extra post-restart input and absent structured
publication-failure diagnostic (`red-final-review.log`, exit 1: one failure and
one error). A separate real-process RED then found repeated client IDs across
fresh profiles (`red-profile-identities.log`, exit 1: `10 != 20`). IDs now include
the noise-profile discriminator, and a regression requires twenty distinct IDs
across the two smoke profiles. All are covered by the final GREEN suite.
A transient syntax error
while editing the diagnostic branch was corrected before the final passing runs.

An independent Codex review then rejected Python's permissive equality for
boolean/float version fields. The regression run reproduced both malformed
version acceptance and a locally identified complete-case omission gap before
exact integer and frozen-case-round checks closed them. The full report below
was regenerated from the repaired source before final gates and review.

## Reproduction and checks

From the repository root, using only installed toolchains/cached dependencies:

```bash
mkdir -p target/task016/tmp
export CARGO_NET_OFFLINE=true
export CARGO_TARGET_DIR="$PWD/target"
export TMPDIR="$PWD/target/task016/tmp"
export PYTHONDONTWRITEBYTECODE=1
python3 scripts/test-personal-quality.py AssessmentTests
python3 scripts/test-personal-quality.py
python3 scripts/test-personal-baseline.py
python3 scripts/personal-quality.py --history-size 4 --samples 2 --output target/task016/smoke.json
./scripts/agent-check.sh
cargo +1.88.0 check --offline --locked --workspace --all-targets
python3 scripts/personal-quality.py --output docs/agent/tasks/016-personal-task-corpus.json
python3 target/task016/audit.py docs/agent/tasks/016-personal-task-corpus.json
git diff --check
sha256sum docs/agent/tasks/016-personal-task-corpus.json
```

The audit script and logs are retained as ignored local evidence in
`target/task016`; they are not runtime dependencies. The independent audit never
imports or executes the measured harness. It separately spells out all seed
texts, queries and expected orders, resolves seed input provenance, reconstructs
raw metric numerators/denominators, checks individual and aggregate values/nulls,
reconciles event/input/query/task/turn/call identities, compares exact capsule
bytes, recomputes nearest-rank latency summaries, and verifies the complete
source-file set, three artifacts and corpus digest against source AST and report.

The original Task 015 gate integration command is unchanged. The five-case smoke
is N=4, samples=2, twenty runs total. The default is N=1000, samples=5, fifty
runs total, generated only after the implementation and gates pass. The first
passing report/gate preceded final reporting/identity fixes; final evidence below
uses the regenerated report and the gates that exercised the final code.

Runtime environments are cleared; proxy discovery is disabled for loopback
HTTP. Builds use the exact Cargo JSON artifact resolver with `--offline --locked`.
The CLI and cfg(test) fixture executable are executed; the production daemon is
built and hashed but not executed by the quality corpus. The measured workload
uses no live provider, credential, external network or dependency download.
Repository delivery follows only after the measured source passes gates and
independent review. The cached Rust 1.88 toolchain was present.

## Results and measurements

All final required commands exited **0** on 2026-09-18. The focused assessor
suite passed **10 tests**, the complete quality suite **18 tests**, and Task 014
compatibility **8 tests**. The smoke passed **20 runs**. The canonical gate
passed canaries, formatting, strict Clippy, **490 Rust tests across 48 suite
summaries** (460 unit/integration, 25 compile-fail doctests, five actual-CLI
scenarios), both Python suites and both smoke workloads. The cached
`cargo +1.88.0 check --offline --locked --workspace --all-targets` passed.
The full default report passed **50 runs**, followed by the independent audit.

The recorded UTC is `2026-09-18T04:20:09.386573+00:00`; base HEAD is
`5e9d5345a666bddcc85c482bd0ed5cfa9d313ed2`. Source hashes bind the uncommitted implementation.
Host: `Linux-6.18.39+rpt-rpi-2712-aarch64-with-glibc2.41`, 4 CPUs, CPU description
`0xd0b`, 8454029312 bytes
system RAM, Python 3.14.7, Rust 1.88.0, debug/default-feature artifacts.
Build overrides are `{'CARGO_BUILD_JOBS': None, 'CARGO_BUILD_TARGET': None, 'RUSTFLAGS': None}`; the corpus uses three installed
capability packages. No controlled-host performance inference is made.

Every profile has exact set/order **25/25**, nontrivial order **10/10**,
micro Recall@2 and returned precision **30/30**, and identity reconciliation
**25/25**. Each has 25 planned, durable-input, durable-model, call-log and
observation records; missing/duplicate/unmatched counts are all zero. All fifty
client request IDs are distinct across the two stores. Stale/scope leaks are
**0/25**, irrelevant leaks **0/95**; noise leaks are **0/0 (null)** in minimal
history and **0/25000** in longer history. No-match Recall@2 and precision are
null because their expected/output denominators are zero. No failing individual
case is concealed by a micro aggregate.

In fixed case order, selected nodes are **1, 2, 2, 1, 0** and serialized capsule
sizes are **227, 470, 470, 237, 12 bytes**, identical in each repetition/profile.
Seed, restart and listing make zero model/tool calls and no observer records.
Listing recovers all 5+N active personal memories and the one elsewhere memory,
with OLD excluded and no appended events. Fixture answers remain unverified.

Timing values below are rounded to three decimal places; the JSON retains exact
raw floats, per-request samples, all checkpoints and file/WAL/SHM sizes.

| Measurement | Minimal history | Longer history |
| --- | ---: | ---: |
| Noise memories | 0 | 1000 |
| Successful saves, including OLD | 7 | 1007 |
| Runs after restart | 25 | 25 |
| Seed duration, ms | 67.729 | 9624.529 |
| Initial readiness, ms | 14.987 | 11.944 |
| Restart readiness, ms | 22.321 | 812.625 |
| Recovered listing, ms | 13.084 | 907.725 |
| RSS after seed, bytes | 19300352 | 21676032 |
| RSS after last query, bytes | 21905408 | 26263552 |
| Restarted process high-water RSS after last query, bytes | 21905408 | 26820608 |
| Durable events: seed / final | 14 / 189 | 2014 / 2189 |
| Storage after seed, bytes | 1103792 | 10337744 |
| Final storage including fixture logs, bytes | 5352523 | 12308099 |
| Final storage excluding fixture logs, bytes | 5341568 | 12297144 |
| Final fixture logs, bytes | 10955 | 10955 |
| Durable model requests / independent calls | 25 / 25 | 25 / 25 |
| Tool dispatches / task.completed | 0 / 0 | 0 / 0 |
| Pooled corpus latency p50 / p95, ms | 66.058 / 75.724 | 186.184 / 190.194 |

| Query | Minimal p50 / p95, ms | Longer p50 / p95, ms |
| --- | ---: | ---: |
| `cedar timezone` | 62.055 / 84.247 | 186.185 / 188.018 |
| `harbor packing` | 66.350 / 71.456 | 186.217 / 190.194 |
| `harbor train` | 66.107 / 69.988 | 186.380 / 186.706 |
| `supper preference` | 62.365 / 69.913 | 185.853 / 186.299 |
| `observatory telescope` | 65.553 / 75.724 | 185.918 / 202.187 |

Nearest-rank per-query p50 is the third sorted sample and p95 the maximum of
five. The pooled 25-sample summaries are explicitly separate. There is no
latency or RAM pass threshold.

## Identity and local evidence digests

Report SHA-256: `14aa0b239efd7a2c9ebfd5c08a0a5b2652482aca9d0c120a813b43bd6c1cbbec`.

Corpus SHA-256: `ade71a7e1d4e4fd9dd2bf5325248f3f35313966c22266a2a0ea2aaf0428e2e39`.

All **113 source files** and **three exact Cargo artifacts** were
independently rehashed and matched after measurement. The complete source map
is in the report; documentation/reports are excluded to avoid recursive hashes.
The source set includes uncommitted measured source.

| Artifact | SHA-256 |
| --- | --- |
| cli | `cef0664c5ac84e53d6331035ff25e8e483f87596dced23bccdc26176d1a3676d` |
| fixture_test_executable | `883fd1746e4f5b54eedb4a604b1ab97e41da8434fd53202043ed9d8a1db8d947` |
| production_daemon | `a76bb95855912e074e958abe94eccafddc6c8ed77ee5b131c5af9de0e7ce09e5` |

Local RED/GREEN evidence is retained under `target/task016` (ignored, not
committed). These hashes preserve the exact observed transcripts and independent
audit code; the early blocked RED and intermediate diagnostics are retained as
well. The task-specific final scope/link/cleanup audit is recorded separately in
`final-checks.log`. Logs without a `-final` suffix are historical iteration
evidence and may contain the pre-review 9/17-test counts or previous report
digest; they are not cited as final closure. Current closure is bound by the
`*-final.log` rows and current `report-sha256.log` below. There are no remaining
blockers or required unrun checks.

| Local evidence file | SHA-256 |
| --- | --- |
| `red-assessment.log` | `dab1f0b61429306bb36118a9a2755f03a4d990166c5bdcb12d5eafb2dcb83b30` |
| `red-workflow-sandbox.log` | `9c58e31b795ef97e5096b827e66907606f8799d14731b28334fbc6183e5e0b11` |
| `red-workflow.log` | `8a992075d929ac7771bb2526215fcec30e6bbb9ac76c53ee7d3178d952b0159d` |
| `red-hardening.log` | `ac5e79762021e14559185016153a33e40ee41b3d264d5486e31e7f622b17b493` |
| `red-final-review.log` | `81c6e173ec0ddd19211a1a630ff2aa5d4512dfccfe545f08de3faf43fc632b8b` |
| `red-profile-identities.log` | `5b591f5a77e203317aa6f762e4749a0c7b2a87d4ddf769a1bc666e488853f5b3` |
| `review-initial.json` | `525226b45fffccbeab76989016acedd17b2f91b4109f8743cdfbd109b3f9c7d2` |
| `review-docs-blocked.json` | `33505d07abf85547f8ceb9cae34c21b27727c2dd08b8333f1a61d8394a785f73` |
| `green-assessment.log` | `3611dbbb0c625ea22582541aa655a4c5d97461a21d768dad7e1784d1d9fc2cb9` |
| `focused.log` | `af4497f8af9305ce5c58dcf51c0258dd7772ebcae8194180bb4979660695cf5e` |
| `focused-final.log` | `6f3fffaacb9254b1310e77873b777677e64b18c3cc27b2a39fe797cb136f1cdd` |
| `baseline-focused.log` | `4ab2f10cb6541a9813790e39c3d03a46c37f8fdb323e1b247024738f5ea72494` |
| `smoke.log` | `2f974d67b956ebb4bd6b9377d9f0a6e1a80be93cabb87bdf576b5733fee331b0` |
| `audit-smoke.log` | `70f8d2719c084ffa0accde1c5c6c72a523744003f8bcbdf786ad28e606c988db` |
| `gate.log` | `6521652e967bbff6b7a08ccfd40d5768a871b9cedadeb9663bd0a53eda3e2b87` |
| `gate-final.log` | `a13ea00b9441df0e6f1113ec3fdac1679ef7027de08d28bc57015475afb765ff` |
| `msrv.log` | `2b34696728dc82b03cbf6311d7e3a6a41658bbde097834c3010a78bc82842c1a` |
| `msrv-final.log` | `2bc683fb4e4f4f21ee9ee39e2b8ecce2efe20d3b1be57bda5316aa519ab3d30a` |
| `recorded.log` | `cea90688c0b5880afc525e8a2d4455060cb3ce5bca5153f93a4018cfdc9f4de5` |
| `audit.log` | `6f4c67e88571c4cfd79c412e6502259dc6ca6699745c8b8bfda15abdb9e5eda5` |
| `audit-final.log` | `da025cba4010b19282e8fb94fde5aa42f1ee49d5114dbd186d79f3df9b9b6d6f` |
| `audit.py` | `f2eb75c432e8f1dccd4fcfddb18a36da278f56fccd3e0d6930ebf0f86b7eb827` |
| `final-checks.log` | `3dee5d11cc4a9cf794108d429f6583a414293aac1e5f303f4c34c6c240d580fb` |
| `final-checks.py` | `e4087580330f8a0e1180ed1dad6ae2532bf85aab53b5c6250a194bdcf710a9ca` |
| `report-sha256.log` | `cfbcdb773dbfca04047f8308cfebebc2a79b8e0a8422747e853d8606f05cf1aa` |

## Interpretation and cleanup boundary

The result supports only **five-case synthetic lexical ContextCapsule
conformance after restart**. The trip queries test nontrivial reversed ranking;
the no-match query requires truly empty context. Empty expected/output/noise
opportunity denominators produce null values with explicit reasons.

Model-answer quality, task completion, semantic recall, tool-task success, live
cost, first useful progress, isolated model/tool/Ditto timing, general agent
quality and v0.1 readiness remain unavailable/open. Fixture answers remain
unverified and unassessed. Measured-workload external provider spend is zero;
fixture token usage, live-equivalent cost and total operating cost are unknown. Paraphrases,
ambiguity, token-budget pressure, scheduling, learning and growing catalogues
are outside these five cases.

Queries follow memory listing, so recovery is warmed. Observer serialization and
file I/O add overhead; CLI latency includes spawn, HTTP, kernel/storage, fixture
response and terminal output. No warmup is discarded. Profiles run sequentially
on an uncontrolled debug host and establish no latency superiority. RSS excludes
CLI children; high-water RSS resets on restart. Storage measures logical live
file sizes, including WAL/SHM, with fixture logs counted separately. Restart
covers abrupt process loss, not power loss.

Regression tests observe actual server PIDs and require reaping/store removal on
SIGTERM before/after restart and SIGINT after restart (exit 143/130). An injected
assessment failure occurs while the real server exists, requires current raw
call counters in structured stderr, reaps the child, removes its store and
preserves an older report without publishing success. Final source-hash failure
also preserves the prior report and removes its staging file. Successful output
uses same-directory atomic replacement only after all checks and final
source/artifact/corpus hash comparisons.

Task 015's report and old reproduction commands remain historical and are not
reproduced by Task 016's new corpus/sample semantics. Task 014 compatibility
remains tested; neither historical report is relabeled as a measurement of this
branch. Final pre-commit inspection found no measured executable processes,
temporary stores or pending publication files. Exactly the eight authorized
paths were staged with no unstaged changes; `agent-check.sh` was byte-identical
to HEAD, and local links/canaries plus `git diff --check` passed. Independent
review blockers were closed. Task 015 PR #20 is merged as squash
`5e9d5345a666bddcc85c482bd0ed5cfa9d313ed2`, the local base for this task.
