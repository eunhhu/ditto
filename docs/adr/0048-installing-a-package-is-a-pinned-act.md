# ADR 0048: Installing a capability package is a pinned act

Status: accepted (2026-11-11). First slice of P4 (plugin manifests) in
`docs/design/platform-surface.md`.

## Context

A registry root is a directory of packages: one directory per capability id,
holding `capability.toml` and the `capability.header.json` that pins it. Nothing
in the repository writes into such a root today - packages are copied in by hand
- so "install" has no contract at all, and the three decisions that matter are
made implicitly by whoever runs `cp`:

- what the caller is agreeing to (the package's bytes, or just its name);
- what happens when a package is installed twice at different contents;
- what a removal is about, if the package changed since the caller looked.

Any one of those answered implicitly is a supply-chain hole: a name is not an
identity, and a mutable directory a caller trusts is a mutable dependency.

## Decision

`capability::install::install(root, manifest_bytes, expected_digest)` writes a
package only when its manifest hashes to the digest the caller named. The
manifest bytes are written verbatim, so the digest the caller pinned is the
digest the registry reads back. The header is generated from those bytes and
written beside them; nothing else is written, and no package code is executed or
read.

- **A digest mismatch writes nothing.** The caller's named digest is the
  identity; a package that hashes to something else is not the package the
  caller asked for.
- **Re-installing the same digest is the same package.** Idempotent, no write.
- **A different digest for an installed id is a conflict, not an upgrade.** An
  upgrade changes what a running agent can be granted, so it goes through its own
  review. Re-running an install must never silently move a package.
- **`uninstall(root, id, expected_digest)` removes only the named digest.** A
  package that moved on since the caller looked is refused rather than removed:
  the removal is about the bytes the caller saw.

The digest is the manifest's content digest (SHA-256 over the canonical manifest
bytes), the same value `CapabilityHeader::from_manifest_bytes` computes and
`package::load` verifies before paging a manifest. There is one identity for a
package, not a second one for installation.

## Consequences

The install path is a library act, not a daemon route yet: a caller that wants
it over the wire needs an event and a lease, which is the rest of P4. What this
slice fixes is that the act cannot be ambiguous - the pin is required, conflicts
are refused, and removal is scoped to bytes.

Making the pin required means a caller who does not know the digest must read the
header first. That is the point: the digest is what the caller is agreeing to,
and a caller who cannot name it has not decided yet.
