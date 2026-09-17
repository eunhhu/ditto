# Task 014: Human task inspection and offline personal-agent baseline

## Observable contract

Add the smallest pre-v0.1 presentation and measurement slice to the existing
CLI. `--human` opts run/sort/schedule/repeat start, status, cancel and list flows
into readable output; default stdout JSON and exit semantics remain unchanged.
No wire, event, kernel, policy, lease, or completion-verification contract changes
are required, so this slice needs no new architecture decision.

- Show model, sort, schedule and repeat outcomes independently. Answers are
  explicitly unverified. Sort verification covers only its exact line contract.
- Show pending reasons and times, requested versus terminal cancellation, and
  repeat claimed/missed counts. Timetable exhaustion is never successful work.
- Explain exact attached-file permission, separate deduplication permission,
  one-call lifetime and absence of a durable grant at submission and run inspection.
  Standalone sort status must disclose when input/mode are unavailable on its
  existing response rather than inventing them.
- Print recovery identity, session and an inspection command before submission;
  preserve independent child inspection and original evidence after restart.
- Escape terminal controls, including CR/LF, C1/ANSI/OSC and bidi formatting,
  in untrusted human text, identities, paths, permission references and errors.
- Provide one offline harness using the actual production-disabled daemon and a
  separately labelled injected-driver test server. Use disposable stores, fixed
  input, explicit sample counts, offline locked builds and no provider credentials.
- Record startup/recovery readiness, idle and repeated-use RSS/high-water RSS,
  raw CLI latency samples and nearest-rank p50/p95, journal/model/tool accounting,
  verified local results, cost-known/unknown and restart/recovery evidence.
  Count injected driver calls independently. Unavailable usage, quality, isolated
  model/tool timings and Ditto-only overhead stay null with reasons.
- Record platform, software/build settings, workload, source digests and scope.
  Production results and fixture results never share a performance label.

## Exit criteria

1. RED regressions demonstrate missing human behavior and baseline accounting.
2. Focused regressions, real CLI/daemon offline scenarios and canonical gate pass.
3. A reproducible measured report and one evidence document agree with code,
   documentation and these limits; no unmeasured claim promotes the full v0.1 goal.

## Deferred

General approval fulfillment, endpoints, durable grants, arbitrary execution,
web/TUI, notifications, providers, embeddings, self-improvement, comparative
benchmarks and live-model quality evaluation. The existing single-owner and
process-restart durability limits remain. Commit, push and PR handling are
repository-delivery steps outside this runtime contract.

## Verification state

Complete for this pre-v0.1 slice. Eleven CLI regressions, eight Python regressions,
the canonical gate and the full 12-request offline baseline passed. See
[evidence](014-evidence.md) and the [raw measurement report](014-baseline.json).
Live-agent quality and broader first-milestone criteria remain open.
