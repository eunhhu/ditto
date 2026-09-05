# Capability Manifest

A capability is metadata plus an isolated implementation. Discovering a
manifest must not start its runtime.

## Package headers and manifest paging

On Linux/macOS, a package can include generated `capability.header.json` beside
`capability.toml`. The daemon indexes bounded headers and reads a full manifest
only when selected for execution. Headers carry discovery/card fields,
retrieval metadata, candidate-byte accounting, and the exact manifest-byte
SHA-256 digest. They carry no runtime command, resource templates, policy, or
verification implementation and grant no invocation authority.

Generate a header after editing a manifest:

```bash
cargo run -p ditto-capability --example package_header -- \
  capabilities/core/artifact-read/capability.toml \
  > capabilities/core/artifact-read/capability.header.json
```

The generator writes JSON to stdout; the catalogue never writes package files.
Bundled-package tests detect generator/header drift. Reload the catalogue after
regenerating headers. A selected manifest must match both the header's digest
and complete metadata projection before existing schema/live-binding checks.
Changed or unavailable bodies fail closed; no stale full-manifest cache is used.

Headerless packages remain supported through a bounded legacy startup read:
derive the header/digest once and discard the full manifest. Metrics distinguish
legacy reads from cold header-backed loading. Inactive header-backed packages
never parse their bodies or validate their inactive complement links. Active
complements must resolve to installed headers; lifecycle/runtime filters still
apply before selection. Package metadata is trusted installation input; signing
and hot reload are deferred.

The Rust 0.2 catalogue exposes `header` for discovery, `page_manifest` for
fallible owned full-manifest access, and `load_metrics` for inspectable work.
The explicit in-memory `insert` path retains the caller's manifest; its header
has an empty file digest and cannot serialize as an installed package header.
Headered startup and retrieval use no model/provider calls or runtime process.
See [ADR 0014](../adr/0014-capability-package-headers.md) for the exact file,
traversal, package-count, startup-byte and retained-data envelopes and platform
boundaries. Serialized capability and event versions remain unchanged.

```toml
id = "device.process.run"
version = "0.1.0"
namespace = "device"
kind = "tool"
summary = "Run a structured process on a registered device."

[runtime]
type = "process"
command = "ditto-device-runner"
lazy = true
idle_ttl_ms = 30000

[placement]
modes = ["local", "ssh"]
requires = ["process"]

[retrieval]
intents = ["restart a service on the home server"]
negative_examples = ["reboot the entire machine"]
aliases = ["remote command"]
complements = ["artifact.read"]

[effects]
resources = ["device:{device_id}", "path:{cwd}/**"]

[effects.minimum]
access = "metadata"
mutation = "none"
externality = "local"
privilege = "user"

[effects.maximum]
access = "credentials"
mutation = "irreversible"
externality = "network"
privilege = "elevated"

[policy]
approval = "risk-based"
secret_handles = ["device-credential:{device_id}"]

[verification]
default = "exit-code-and-expected-output"
```

## Effect profile

Effects are orthogonal dimensions, not one numeric danger rank.

```text
access:       none | metadata | content | credentials
mutation:     none | reversible | irreversible
externality:  local | network | human-communication
privilege:    user | elevated
```

`minimum` controls runtime retrieval eligibility. `maximum` documents the outer
implementation boundary. Neither authorizes a call. A capability-specific
normalizer derives the exact invocation effect from validated arguments before
policy runs.

## Retrieval contract

Catalogue search may inspect incomplete metadata. Runtime search is fail closed:
installed placement, prerequisites, allowed capability IDs, and an effect ceiling
must all permit the manifest's minimum effect.

Available placements are a set, not one global location. A remote primary tool
may therefore compose with a local artifact reader. Active complement references
are resolved from headers at catalogue load and deduplicated across ranked roots
and expansions; full runtime metadata is validated on selected manifest paging.

Descriptions, intents, aliases, negative examples, prerequisites, and
complements may influence the historical catalogue ranking. Health and observed
latency are legacy or future ranking signals only; neither is part of the V2
ranking tuple. Embedding similarity only reranks already eligible candidates; it
never bypasses hard filters or policy.

The shared-query V2 path is separately bounded and typed; the historical
raw-string search remains unchanged. V2 counts lifecycle-active headers before
filters, rejects catalogue candidate 10,001, accepts 1 through 256 ranked roots,
and accepts 1 through 512 expanded cards. It consumes only retrieval-owned
normalization and tokenization. Entity/resource exact terms may select a whole
capability ID or alias; all other query fields are lexical-only. Negative
examples are whole normalized phrase denials for both roots and complements.

V2 ranks eligible roots by exactness, optional embedding similarity, lexical
overlap, preferred placement, and finally capability ID. Every active,
positively eligible root, including an exact root, receives one document
embedding when a provider is configured; complements are never embedded. It
then emits each root followed by its direct, runtime-eligible, non-denied
complements in manifest order, deduplicating IDs and respecting the expanded
capacity.

The V2 path consumes the one validated retrieval query constructed for a joint
working set. Context and capability retrieval share its normalized terms and,
when configured, its query embedding and descriptor. Retrieval is read-only:
capability ranking does not append events, mutate context, persist vectors, or
invoke a model.
The exact manifest-document grammar and provider/error ordering are frozen in
ADR 0010.

## Runtime contract

Runtime types are `builtin`, `process`, `wasi`, `mcp`, and `remote`. Non-builtin
implementations run outside the daemon. An invocable execution-epoch entry
binds the exact capability ID/version, canonical manifest digest, complete
schema digest, and capability-specific deriver revision. An authority-free
model call is instance-validated against that schema, normalized and
revalidated, then deterministic harness code derives the exact effect, typed
resources, and placement. The resulting sealed canonical invocation contains
no model-selected lease handle. Policy selects trusted static policy or a
harness-side lease and may issue only a sealed, expiring permit bound to that
invocation digest. Worker execution remains a separate boundary.

### Bounded builtin artifact read

`artifact.read` is the first canonical derivation and static-policy reference.
The
`ditto-artifact-read` crate owns its exact installed manifest and level-2 schema.
Arguments are exactly `reference`, `offset`, and `length`: the reference must be
canonical SHA-256 form, offsets are non-negative, and one read is limited to
16 KiB. Results deterministically report the requested and returned range, total
size, EOF state, and base64 bytes; validation, authorization, range,
availability, and integrity failures are stable structured error results.

The builtin can inspect local artifact metadata and return bytes verified through
the same storage read. It has no path, process, network, credential, mutation,
approval, or secret-handle surface. It derives the exact local content-read
effect and typed artifact resource. Authorization requires an actor=`system`
`artifact.created` event for the exact reference in compatible session/task
scope at or before the execution-start cutoff, then issues a sealed no-approval
static-policy permit bound to the canonical invocation. Selection and replay
validate the complete manifest/card/schema relationship, not only capability ID
and version.

## Disclosure levels

- L0 namespace map: stable and tiny.
- L1 capability card: ID, purpose, placements, and minimum/maximum effects.
- L2 full input/output schema: paged into one execution epoch.
- L3 runtime: started immediately before first invocation and stopped after idle
  TTL.

Level-2 disclosure uses the provider-neutral `CapabilitySchema` record: stable
capability ID and version, summary, and complete input/output JSON Schemas. The
canonical dialect is JSON Schema Draft 2020-12. When `$schema` is omitted it is
interpreted as Draft 2020-12; when present it must be the canonical Draft
2020-12 URI. The capability boundary structurally validates recognized keywords
recursively while retaining unknown extension keywords without applying legacy
dialect rules to them. This validation does not claim that a particular
provider accepts every valid schema; adapters must apply their own compatibility
checks. Model requests preserve the epoch's schema order and do not load any
capability implementation.
