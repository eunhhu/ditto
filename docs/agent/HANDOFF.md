# Verified handoff

## Canonical state

- Branch: `main`.
- Runtime: Rust daemon and CLI.
- Durable stores: SQLite event spine plus local SHA-256 artifact objects.
- Public mutation ingress: typed user-input command only; arbitrary event append
  is not a public route.
- Streaming design: subscribe-first, high-water-bounded, paginated durable
  replay with sequence-gap and lag recovery.
- Capability state: file-backed manifests, validated complements, strict runtime
  hard filters, and append-only bounded execution epochs.
- Context state: typed provenance graph and deterministic compiler. Pinning and
  policy-required inclusion are trusted ephemeral directives; token cost is
  derived locally.
- Policy state: leases authorize canonical invocations against orthogonal effect
  dimensions. No executor is connected yet.
- Model state: no production model driver or turn loop exists yet.

## Intentionally deferred

- provider-neutral model event IR and provider adapters;
- full input/output schemas at capability disclosure level 2;
- capability worker protocol and lifecycle;
- device registry, local process runner, SSH transport, and secrets;
- persistent context projections and embeddings;
- completion verifiers and improvement compiler;
- authenticated remote gateway and web inspector.

## Known engineering debt

- SQLite calls are synchronous and will need a measured async boundary before
  high-concurrency gateways.
- Artifact range reads verify the whole object for integrity; optimize only with
  a design that preserves immutable-object trust.
- Context graph edges are validated but not yet used in ranking.
- Search remains lexical until the embedding worker slice.

Update this file only after code and checks establish a new fact.
