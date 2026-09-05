# ADR 0014: Bounded capability package headers and cold manifest paging

## Status

Accepted for Task 007. Supersedes eager file-catalogue loading in the capability
manifest specification; invocation, retrieval ordering, and event versions stay
unchanged.

## Context

Catalogue startup currently recursively follows directory paths, reads every
full TOML manifest, and retains all runtime/policy/resource metadata. Retiring
or quarantining a capability only affects retrieval. This makes unused packages
add startup work and resident data, contrary to the product's zero-overhead goal.

## Decision and ownership

A package can contain a generated `capability.header.json` beside its existing
`capability.toml`. Version-1 headers contain only identity, lifecycle, card and
retrieval metadata, the historical candidate-byte accounting value, and a
SHA-256 digest of the exact manifest bytes. A header is discovery metadata, not
an invocable manifest or authorization evidence. Header generation is explicit
local packaging work; the daemon never writes headers or launches a runtime.

The catalogue owns compact headers and file locations. Searches and complement
resolution use headers without reading full manifests. The fallible owned
`page_manifest` operation reads only the selected active package, checks size,
digest, full manifest validation, and exact header projection, and returns a
manifest only after every check passes. Existing live binding/schema validation
then applies. No full-manifest cache grows with historical selections.

The local package directory remains trusted installation input. Header edits
can change discovery results; a stale or contradictory header cannot authorize
execution. This is not package signing, hot reload, or an integrity audit of
every unused body. Reload explicitly after regenerating changed packages.

Headerless legacy packages have a bounded compatibility path: read and validate
each full manifest once, derive its header and digest, discard its runtime
payload, then page it with the same checks on use. Metrics distinguish those
startup reads from header reads. In-memory `insert` remains an explicit caller-
owned manifest path: it retains that manifest and exposes an empty byte digest
because there is no source file. That projection cannot serialize as a package
header. The two bundled packages migrate to generated headers.

## Resource and filesystem contract

Fixed maxima: 16 directory levels, 65,536 discovery entries, 16,384 packages,
64 KiB per header file, 1 MiB per full manifest, 64 MiB aggregate startup header
or legacy-manifest bytes, and 16 MiB accounted retained header data. Accounting
includes owned header fields and capacities, source paths, and ID-index entries;
outer collection capacity and allocator overhead are not an RSS measurement.
Checked limits reject the next operation rather than return a partial catalogue. Directory
iteration is streaming; only bounded package paths and headers are retained.
V2's independent 10,000 active-candidate and cumulative work limits remain.

On Linux and macOS, descriptor-relative traversal opens every directory and
input file with no-follow semantics, checks regular files, and reads at most
limit+1 bytes.
The caller's configured root parent is resolved once, allowing platform aliases
such as macOS `/var`. The root itself and all descendants reject symlinks.
Discovery streams entries from open directory descriptors; paging uses resolved
paths and reopens through the same no-follow traversal, independent of cwd.
Platforms without the required descriptor primitives fail with an explicit
unsupported-filesystem error rather than claim race-safe traversal.

Counters report headers read, legacy bodies read, full page reads, input bytes,
and retained header bytes. They are deterministic work evidence, not RSS or
latency benchmarks. The fixed header limits bound parser input and allocation;
no network, model, provider, worker, new database, or background process is added.

## Compatibility and failure behavior

The Rust capability crate becomes version 0.2.0. Its borrowed `get` manifest
accessor is replaced by explicit `header` and fallible owned `page_manifest`.
Existing search signatures and
ranking remain; the public HTTP/event schemas and live invocation authority
types do not change. Header-only inactive packages need not parse or validate
their bodies; active complements must resolve to installed headers.

The read-only turn maps selected-package paging failures to one stable,
path-free `CapabilityContract` failure before model invocation. Replay accepts
that exact new failure message while preserving all historical version-1
records. A missing/inactive capability remains unavailable. No operational
filesystem error becomes a successful invocation or a fallback manifest.

## Alternatives and consequences

- A persistent generated cache would require invalidation/recovery machinery;
  an explicit package header has no daemon write or background lifetime.
- Keeping synthetic partial `CapabilityManifest` values would blur metadata
  and execution contracts. Headers are a distinct type.
- Eagerly hashing all bodies would defeat cold startup. Selection rechecks the
  digest and metadata before execution instead.
- Retaining every paged manifest would grow memory with usage; owned values
  leave the catalogue after the current invocation is finished.
- Manual duplicate metadata would create debt. One deterministic generator and
  a bundled-package drift test own header production.

## Evidence and rollback

Tests cover zero full-body reads for header-backed startup/search, one selected
page, stable legacy/V2 results, large catalogues, inactive malformed bodies,
changed/deleted/contradictory selected bodies, symlink and non-regular metadata,
exact limits, and unchanged Task 003 successful execution/replay. Run the
canonical gate and MSRV check. Rollback removes generated headers and restores
the prior loader; no event, artifact, projection, or credential migration occurs.
