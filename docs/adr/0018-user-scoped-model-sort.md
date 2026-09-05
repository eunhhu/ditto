# ADR 0018: Model-directed sort within explicit per-run permission

## Status

Accepted for Task 011. Extends ADRs 0016 and 0017; legacy artifact-read turns
and explicit provider-free sort commands retain their contracts.

## Decision

`run` optionally accepts one bounded UTF-8 sort attachment. CLI `--sort-file`
allows one sort of that exact input; `--allow-deduplicate` separately permits
removing exact duplicate lines. Without the attachment, artifact.sort is absent
from the execution epoch. Ordinary request text, model arguments, and saved
memory cannot create or widen this permission. Permission expires with the
accepted turn (at most five minutes), and a process still has its five-second
owned deadline. The existing one-call lease and affine execution claim enforce
dispatch. Invalid arguments, another artifact, or unapproved deduplication fail
before consuming that lease. A second dispatch cannot occur in the same run.

The kernel stores attached content as a task-scoped artifact before admission.
Version-2 agent-run metadata records its source event, hash and allowed operation;
version 1 remains the no-attachment contract. Retry identity includes exact file
bytes and the permission bit in addition to normalized request text. No new
provider call, queue, or permission is created on retry or restart.

The existing bounded model continuation loop pages artifact.read and, only when
allowed, the exact artifact.sort schema. Initial user content includes structured
attachment/permission metadata derived from the user's explicit command. It
contains no file body or local filename. This metadata represents actual user
authorization, never inferred model intent. The stable system prefix remains
unchanged. A model may read the bounded input, choose an allowed sort, inspect
the resulting artifact, and answer within the existing eight-request ceiling.

Sort tool requests remain model-authored. Separate version-1 `agent.sort.requested`,
`agent.sort.started`, and `agent.sort.output` events preserve the turn's causal
chain and record normalization, consumed authority and typed results. Only the
registered worker's independently verified output can become a successful tool
result. Its artifact root records the dispatch as producer. The model answer
remains unverified; no model `run_*` task.completed event is introduced.

Run status reports permission and sort progress independently from the model
answer, including a verified output artifact after model failure or interruption.
Reading that status rechecks exact causal roots, bounded content hashes and the
line contract; structural replay alone is not content verification. A small
schema-4 partial event index contains only sort start/output events, allowing
bounded exact-turn lookup without scanning transcripts or retaining results in
daemon memory. At most one start and seven outputs can exist per bounded turn.

The existing pure replay projector gains additive sort projections. It validates
permission provenance, selected schemas/revisions, argument normalization,
grant scope, one-shot use, causal dispatch/output and artifact-root evidence,
and exact model continuation without provider, process or artifact I/O. Legacy
traces remain readable. Old readers fail closed on version-2 run metadata.

Cancellation reaches the real child, with the existing process cleanup contract.
Result persistence and cancellation use the shared slot gate. Any successful
effect already recorded stays inspectable even when the following model request
fails. Recovery never reissues model or worker execution.

## Alternatives and scope

Natural-language permission inference would let the model authorize itself.
A global allow-sort switch would grant unrelated artifacts. A separate approval
queue or second model loop would add state and duplicate lifecycle rules for a
permission that can be expressed at submission. This slice uses explicit scoped
permission, not pending-approval fulfillment, arbitrary programs or scheduling.
No new crate, service, dependency or paid validation call is needed.

## Compatibility, evidence and rollback

Commands/responses gain optional fields; existing clients keep working. Only
attachment runs use metadata version 2. Capability selection adds an optional
sort manifest while preserving existing fields. Event-store schema 4 adds one
small partial index and no payload rewrite; rollback requires a schema-4-aware
binary, with attachment ingress disabled. Required evidence includes actual
fixture-model → process → verified artifact → model continuation, unauthorized
input/deduplication/repeated-call rejection, retry conflict, cancellation,
post-effect failure/restart inspection, corruption rejection, pure replay and
real CLI/HTTP translation, plus canonical and MSRV gates.
