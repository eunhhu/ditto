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
an offline history smoke (zero versus four unrelated memories, two queries each).
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
| Context/history workload | actual CLI saves/correction/restart/queries; exact driver capsules reconciled with durable request identities and independent calls; corrected inclusion and stale/irrelevant/noise/other-session exclusion for every request; raw measurements/source hashes; no fixture-answer quality inference |

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

## Offline context/history evidence

`python3 scripts/personal-quality.py --output docs/agent/tasks/015-quality-history.json`
runs fresh minimal and longer histories with zero and 1,000 unrelated memories
and five unique queries per profile. `--history-size` (1–5000) and `--samples`
(2–100) bound the workload. The canonical gate uses four unrelated memories and
two queries each, with a disposable report. Python optimization (`-O`) is
rejected because it would disable assertions. Set `TMPDIR` and `CARGO_TARGET_DIR`
inside the repository when all generated files must remain local to it.

The cfg(test) driver can opt into a separate structured capsule log. Every
request must retain the corrected fact and exclude stale, irrelevant, noise and
other-session values after restart. Observations must match durable request
IDs/payloads and the independent call log. Default fixture call counting and
production behavior remain unchanged. Report raw timing, RSS/storage/event/call
counts, selected capsule nodes/bytes and source/binary hashes. Recheck hashes
after recording; regenerate measurements if any measured source changes.
The [recorded evidence](tasks/015-evidence.md) is one synthetic V1 lexical
selection workload; broader quality and v0.1 criteria remain open.

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
