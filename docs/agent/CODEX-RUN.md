# Codex run prompt

Select the desired frontier model and reasoning effort in the Codex client, then
start with this compact prompt:

```text
Read AGENTS.md and docs/agent/README.md. Continue from docs/agent/NEXT.md in
priority order. Work autonomously through complete vertical slices while the
scope remains inside Ditto. Keep main green, run ./scripts/agent-check.sh after
each slice, update HANDOFF.md and NEXT.md only with verified facts, and commit
coherent changes. Do not implement fake success paths or stop for routine
reversible engineering choices. Stop only at a destructive/external boundary,
a material scope expansion, or an unresolved public-contract ambiguity.
```

After context compaction or a resumed session, use:

```text
Recover from AGENTS.md, docs/agent/HANDOFF.md, docs/agent/NEXT.md, the active task
file, git status, recent commits, and failing checks. Continue from repository
evidence rather than remembered chat history.
```
