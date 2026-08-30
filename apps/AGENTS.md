# Application adapter rules

- `apps/*` are thin ingress, transport, and presentation adapters.
- Business rules, event authority, policy, and persistence belong in crates.
- Public endpoints accept typed commands, never arbitrary trusted events.
- A non-loopback listener requires authentication or an explicit unsafe escape
  hatch whose name makes the risk obvious.
- Streaming clients must recover from durable storage and deduplicate by the
  daemon-issued global sequence.
- Add adapter tests for protocol translation and boundary failures; do not
  duplicate domain tests here.
