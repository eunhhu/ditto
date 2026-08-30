## Contract

What observable behavior or invariant changes?

## Evidence

- [ ] Focused regression tests added or updated
- [ ] `./scripts/agent-check.sh` passes
- [ ] Public contract/spec updated when applicable
- [ ] ADR added or superseded for an ownership-boundary change
- [ ] `docs/agent/HANDOFF.md` and `NEXT.md` remain factual

## Trust-boundary review

- [ ] Untrusted input cannot choose actor, authority, effect, resource, or evidence
- [ ] No credential or secret material enters prompts, events, logs, or fixtures
- [ ] No in-memory state is described as durable
- [ ] No stream/process/model completion is mislabeled as task verification
- [ ] Deferred behavior is not represented by a fake success path
