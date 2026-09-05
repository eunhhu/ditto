# Task 011: Model-directed, explicitly permitted local work

## Observable contract

`ditto run REQUEST --sort-file FILE [--allow-deduplicate]` lets the model sort
one exact attached artifact once and continue from its verified result. The
user can inspect both the model answer and independent sort outcome after
failure or restart. No attachment means no process authority. See
[ADR 0018](../../adr/0018-user-scoped-model-sort.md).

## Failure model

Request text/memory/model arguments must not mint permission. Another artifact,
unapproved deduplication or repeated dispatch must not execute. Retries must
bind the file and permission, and recovery must not rerun effects. Cancellation
must reach the child. A later model failure must not hide an already verified
artifact. Replay must not recreate authority or call a worker/provider.

## Exit criteria

1. Explicit bounded attachment/permission through CLI and typed run ingress.
2. Conditional schema paging, exact-resource one-call lease/claim, real worker
   dispatch and model continuation within the existing bounded loop.
3. Durable causal evidence, independent result status and backward-compatible
   pure replay, with a bounded indexed lookup rather than transcript scanning.
4. Positive and negative model/process, authority, retry/recovery, corruption,
   cancellation and CLI/HTTP scenarios without paid model calls.
5. Canonical gate, MSRV, inspectable evidence, documentation and coherent commit.

## Completion

All exit criteria passed locally; [evidence](011-evidence.md) records source
identity, checks and scope limits. Linux CI and merge identity are recorded by
the associated PR.
