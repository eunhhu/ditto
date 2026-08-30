# ADR 0005: Trusted command ingress and orthogonal effects

## Status

Accepted.

## Context

An append-only database prevents history from being rewritten, but it does not
prevent a client from initially inserting a false `system`, `policy`, or
`task.completed` event. A single numeric effect rank also makes elevated access
accidentally imply unrelated authority such as irreversible writes, credentials,
or human communication.

## Decision

Public network mutation endpoints accept narrow typed commands. The kernel
assigns event actor and kind. Arbitrary event insertion remains an internal
kernel operation and is not exposed by the default daemon.

Effects are represented as an orthogonal profile with data access, mutation,
externality, and privilege dimensions. A lease must permit every dimension of a
canonical invocation. Missing device, program, or resource fields fail closed
when the lease scopes that dimension.

Capability search uses the minimum supported effect for eligibility; the maximum
effect documents the implementation's outer boundary. Actual invocation effects
will be derived from normalized arguments by capability-specific code.

## Consequences

- Public clients cannot manufacture trusted audit history.
- Elevated read authority does not imply deletion, credential access, or messaging.
- Existing capability manifests migrate from one `maximum` enum to nested
  `minimum` and `maximum` profiles.
- Model-facing calls cannot be sent directly to policy or execution.
