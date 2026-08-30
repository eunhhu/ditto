# Capability manifest rules

- A manifest is executable metadata, not decorative documentation.
- IDs are stable, lowercase, dotted names; namespace matches the first segment.
- Runtime placement and prerequisites are explicit and fail closed at runtime.
- `effects.minimum` controls safe retrieval eligibility;
  `effects.maximum` documents the outer implementation boundary.
- Actual invocation effects are derived from normalized arguments by the
  capability implementation. The model does not self-authorize by declaring a
  smaller effect.
- Complements must resolve to installed manifests. Remove speculative links
  rather than silently ignoring them.
- Discovery must not start the implementation process.
