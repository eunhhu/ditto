# Quality gates

Run the canonical gate from the repository root:

```bash
./scripts/agent-check.sh
```

The script checks formatting, strict Clippy, workspace tests, and required agent
control files. CI additionally verifies the declared MSRV.

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
