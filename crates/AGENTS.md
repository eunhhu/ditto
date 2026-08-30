# Core crate rules

- Crates must not depend on `apps/*`.
- Keep domain types close to the crate that enforces their invariant.
- The kernel composes components; it must not absorb every implementation.
- Durable state enters the event store or content-addressed store before being
  advertised as durable.
- Security decisions consume canonicalized, derived data, never raw model
  claims.
- Prefer deterministic unit tests and event replay fixtures. Every public trust
  boundary needs at least one negative test.
- Split a new crate only when it creates a real ownership, runtime, or reuse
  boundary. A future box in the architecture diagram is not enough.
