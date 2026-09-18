# Quality gates

Run the canonical gate from the repository root:

```bash
./scripts/agent-check.sh
```

The script checks tracked-artifact, developer-path, and credential-shaped
canaries before formatting, strict Clippy, workspace tests, and required agent
control files. It also runs the five existing actual-CLI fixture scenarios,
baseline accounting regressions and a small offline baseline scenario (two
repeated requests per server), plus context-assessment/process regressions and
a five-case offline corpus smoke (zero versus four unrelated memories, two
repetitions per query: ten runs per profile, twenty total).
Python 3 uses only its standard library; the measurement scripts build with
cached dependencies via `--offline --locked`.
Loopback binding and the existing `/usr/bin/sort` profile must be available.
CI additionally verifies the declared MSRV.

## Evidence by change type

| Change | Minimum evidence |
| --- | --- |
| Event storage or streaming | snapshot pagination, concurrent boundary or gap recovery, reconnect cursor behavior |
| Public ingress | negative test proving clients cannot choose trusted actor/kind |
| Context selection | relevant inclusion, irrelevant exclusion, provenance rejection, required-context failure |
| Capability retrieval | hard-filter failure, complement resolution, stable bounded epoch, large synthetic catalogue |
| Policy | missing-scope rejection, orthogonal effect rejection, lease expiry/call budget |
| Artifact store | deduplication, size limit, tamper detection, symlink/no-follow behavior, range read |
| Model driver | every emitted event variant, malformed stream, usage, tool calls, continuation, provider cancellation |
| Completion | verifier-specific positive and negative evidence; stream closure is insufficient |
| Human task views | default JSON compatibility, independent model/sort/parent/child outcomes, unverified answers, requested/terminal cancellation, exhausted/missed repeats, recoverable identity and terminal-control escaping |
| Personal-agent baseline | separate production-disabled and injected-fixture results; raw samples/settings/source identity; idle/repeated RSS and latency; model/tool accounting; known versus unavailable cost; exact retry and restart evidence |
| Personal-task context corpus | actual CLI saves/correction/restart/list/five-query rounds; frozen expectations independent of observations; exact nodes/order/metadata/provenance; durable input/query/task/turn/event/call/observation reconciliation; raw metrics with null empty denominators; leak adversaries and signal/failure cleanup; source/artifact/corpus hashes; no fixture-answer quality inference |

## Offline baseline evidence

`python3 scripts/personal-baseline.py --output /tmp/ditto-baseline.json` runs the
full default 12-request sample. `--samples` (2–100) and `--idle-seconds` (.05–60)
are explicit workload parameters. The canonical gate uses a disposable report;
recorded baseline evidence belongs with the task evidence, not an unlabelled
performance claim. There are no machine-dependent latency/RAM pass thresholds.

The production daemon always has its provider disabled; fixture execution is
compiled only into the daemon test executable. Both use the existing HTTP
surface and real CLI. No live provider, download, pricing lookup or quality
inference is permitted. Offline external provider spend is known zero, while
unavailable usage, live-equivalent cost and isolated timing are null with reasons.
Server RSS excludes CLI/sort children and a short fixed-catalogue sample is not
evidence of long-use quality or a comparison with another agent.

## Offline personal-task corpus evidence

```bash
python3 scripts/test-personal-quality.py
python3 scripts/test-personal-baseline.py
python3 scripts/personal-quality.py --history-size 4 --samples 2 --output target/task016/smoke.json
./scripts/agent-check.sh
cargo +1.88.0 check --offline --locked --workspace --all-targets
python3 scripts/personal-quality.py --output docs/agent/tasks/016-personal-task-corpus.json
```

Use cached dependencies/toolchains only. Set `CARGO_NET_OFFLINE=true`,
`CARGO_TARGET_DIR="$PWD/target"`, `TMPDIR="$PWD/target/task016/tmp"` and
`PYTHONDONTWRITEBYTECODE=1`; create the temporary directory first. Local loopback
must be available. Python optimization (`-O`) is rejected because shared
measurement helpers use assertions.

The [Task 016 contract](tasks/016-personal-task-corpus.md) freezes seven seeds
and five lexical queries before observation. Schema 2 embeds that definition and
its digest. `--history-size` (1..5000, default 1000) selects the longer profile's
noise; zero-noise is always included. `--samples` (2..100, default 5) means
**repetitions per query**: the default has 25 runs per profile, 50 total. The
unchanged canonical integration uses N=4 and samples=2, now 20 total runs.

Exact set/order, nontrivial ordering, Recall@2, returned precision, all four leak
categories and durable identity reconciliation must pass each request. Keep raw
numerators/denominators/contributors; zero denominators are null with reasons.
Retain exact capsules, durable input/request and source provenance evidence.
Check actual query, session/task/turn/event identity and independent calls;
reject malformed, missing, extra, duplicate, reordered or swapped records.
Fixture answers are never assessed. Normal, failure, SIGTERM and SIGINT cleanup
must reap children and remove temporary stores. Publish atomically after final
source/artifact hash verification; independently audit the recorded report.

[Task 016 evidence](tasks/016-evidence.md) supports only five-case synthetic
lexical ContextCapsule conformance after restart. Model-answer quality, task
completion, semantic recall, tool-task success, live cost, first useful progress,
general agent quality and v0.1 readiness remain unavailable/open.
Task 015's [report and commands](tasks/015-evidence.md) are historical and are
**not reproduced** by the new corpus/sample semantics. Task 014 compatibility
remains tested; no production observer or runtime change is introduced here.

## Review questions

- Can untrusted input choose its own authority, effect, resource, or evidence?
- Is an in-memory object being described as durable?
- Can a missing field make a policy check disappear?
- Does a limit bound one page or silently truncate the whole logical result?
- Can a “maximum capability” incorrectly hide a safe minimum use?
- Does context cost come from trusted computation rather than supplied metadata?
- Are tool ordering and stable prompt prefixes preserved within an epoch?
- Is a deferred subsystem honestly reported as deferred?

## Failure reporting

When a command fails, preserve the first actionable error, fix the cause, and
re-run both the focused check and the canonical gate. If environment limitations
prevent a check, record the exact unrun command and risk in `HANDOFF.md`.
