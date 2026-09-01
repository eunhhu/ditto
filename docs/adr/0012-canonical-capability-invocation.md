# ADR 0012: Canonical capability invocation and invocation-bound authority

## Status

Accepted.

## Context

ADR 0005 established orthogonal effect dimensions and required a capability
implementation to derive an invocation's exact effects from normalized
arguments. The initial policy crate did not enforce that direction: its
`CanonicalInvocation` was deserializable, all fields were public, and a caller
could choose a lease handle, effects, device, program, and string resources.
ADR 0009 then introduced a deliberately narrow, lease-free `artifact.read`
exception before a canonical invocation lifecycle existed.

That shape lets untrusted or merely mistaken callers skip capability revision
resolution, JSON Schema instance validation, deterministic normalization, and
resource derivation. It also makes authorization a mutable method on a lease,
with no invocation-bound permit or cross-retry conflict record. A later worker
could therefore receive an apparently authorized value that was never derived
from the exact capability shown in the execution epoch.

This decision creates a new authority boundary. It supersedes ADR 0005's
invocation/lease direction and ADR 0009's lease-free wording for
`artifact.read`; it preserves ADR 0009's exact execution, durable event, and
replay semantics. It does not amend or enlarge ADR 0011.

## Decision

### Fixed authority pipeline

The only accepted live invocation pipeline is:

```text
UntrustedToolCall
→ exact execution-epoch and capability-revision resolution
→ Draft 2020-12 JSON Schema instance validation
→ bounded capability-specific argument normalization
→ normalized-argument schema revalidation
→ bounded capability-specific effect and typed-resource derivation
→ sealed CanonicalInvocation
→ policy authorization or approval-required outcome
→ sealed invocation-bound InvocationPermit
```

The model-facing and serialized tool-call input contains exactly a source call
ID, capability ID, and raw JSON arguments. Unknown fields are rejected. It has
no effect, resource, device, program, placement, lease, approval, idempotency,
verification, credential, or evidence field. Provider call IDs remain
untrusted correlation values; the harness derives the invocation identity and
idempotency key from the execution epoch plus that source ID.

`CanonicalInvocation` and `InvocationPermit` have private fields, expose no
unchecked public constructor or struct-literal path, and do not implement
`Deserialize`. They may expose read-only getters and bounded serializable
evidence projections, but serialization never becomes an invocation ingress.
Compile-fail tests fix both properties.

### Exact epoch and revision binding

An invocable epoch entry binds all of these values:

- execution epoch ID;
- capability ID and semantic version;
- SHA-256 digest of the complete canonical manifest value;
- SHA-256 digest of the complete provider-neutral capability schema;
- stable capability-specific deriver revision.

Canonical manifest and schema digests are computed from their deterministic
canonical JSON projections, not filesystem paths or TOML formatting. An epoch
may retain legacy discovery-only cards, but a card without a complete revision
binding is not invocable. Resolution compares the epoch binding with the
currently installed manifest, exact level-2 schema, and deriver before any
argument normalization. It checks the deriver revision again after derivation.
A changed ID, version, manifest digest, schema digest, deriver revision, absent
epoch entry, retired capability, or partially bound epoch fails closed.

The revision binding is serialized as additive execution-epoch evidence so a
recorded selection can be inspected. Legacy Task 003 epochs without that
additive evidence remain replayable but cannot authorize a new live invocation.

### Instance validation and deterministic derivation

The capability boundary structurally validates the declared Draft 2020-12
schema and then evaluates raw arguments as an instance before the deriver runs.
Evaluation has fixed schema-size, instance-size, depth, branch, and node-work
bounds. Unsupported reference or regular-expression behavior fails closed;
unknown annotation/extension keywords remain non-authoritative. No schema
failure reaches policy.

Only registered harness code implements a capability deriver. A deriver is
versioned, deterministic for the exact normalized input, has a fixed work and
output envelope, and performs no filesystem, network, process, credential,
clock, randomness, or other I/O. It returns normalized JSON, an exact
orthogonal effect profile, and typed canonical resources. The boundary
revalidates normalized JSON against the same schema and rejects oversized
output or resource count before sealing an invocation.

The derived effect must be both at least the manifest minimum and at most its
maximum in every orthogonal dimension:

```text
derived permits minimum && maximum permits derived
```

A value below the minimum is a manifest/deriver contract mismatch, not a
harmless claim. A value above the maximum is denied. Derived resources must
match a declared capability resource family. Task 005 supports the exact
content-addressed artifact family; future device, process, credential, network,
or path capabilities require their own explicit deriver and policy slice.

Canonical resources are typed values, never policy-significant raw strings.
Artifact resources contain a validated lowercase SHA-256 identity. The path
resource primitive is lexical and I/O-free: it requires NFC, rejects control
characters, backslashes, empty/dot/parent components and absolute/relative
ambiguity, and compares path components rather than string prefixes. This
prevents traversal, sibling-prefix matches, and Unicode aliases. It does not
claim symlink-safe filesystem resolution; no path executor is introduced here.

Task 005 resolves only the installed local builtin placement for
`artifact.read`. Device selection, program selection, process placement, SSH,
and runtime loading remain unsupported rather than being represented by caller
strings.

### Atomic, idempotent authorization

Policy consumes only a sealed `CanonicalInvocation`. It selects a trusted
static policy or a harness-selected lease; the invocation and model call carry
no lease selector. The result is one of:

- a sealed `InvocationPermit` bound to the invocation digest;
- a sealed approval-required request bound to that digest; or
- a typed denial.

Permit evidence includes a policy-generated permit ID, invocation digest,
authorization source, grant instant, and expiry. A permit can be checked only
against the matching canonical invocation and its validity window. It cannot be
rebound, deserialized, or used as a public struct literal.

One process-local authorization ledger serializes invocation-ID binding,
idempotency lookup, all lease checks, lease consumption, and permit insertion
under one mutex. The first attempt permanently binds an invocation ID to its
digest. A different digest for that ID fails closed even if the earlier attempt
was denied. A failed check or approval-required result consumes no call. A
successful lease authorization decrements at most once and stores the permit in
the same critical section. An identical retry returns the same permit without
another decrement. Therefore concurrent authorization against a lease with one
remaining call can issue at most one new permit.

Approval fulfillment, durable policy storage, and cross-process authorization
coordination are deferred. An approval-required outcome is not a permit and
cannot execute anything.

### `artifact.read` migration boundary

`artifact.read` is the reference deriver. Its exact arguments are schema-
validated, normalized to the existing checked reference/range value, and
revalidated. It derives exactly:

```text
effect    = content / none / local / user
resource  = artifact:sha256:<lowercase digest>
placement = local builtin
```

The existing same-session/task `artifact.created` high-water check supplies
the static policy's exact resource scope. Policy approval remains `never`, but
the old lease-free exception is replaced by a sealed, expiring static-policy
permit bound to this invocation. A missing root produces the same stable
`unauthorized_reference` tool result and no permit.

The kernel must possess and match both the canonical invocation and permit
before calling the existing bounded artifact authority. Task 003 keeps its
event kinds, payload version, ordering, authorization high-water, range
semantics, model continuation, replay projection, and explicitly unverified
terminal. Invocation and permit are ephemeral Task 005 authority objects; this
slice does not persist a new event or reinterpret old replay data.

### Explicit stopping point

This decision ends at canonical invocation plus permit issuance and the guarded
use of the already existing bounded `artifact.read` executor. It adds no worker,
runtime loader, subprocess, network call, SSH, credential or secret resolution,
file mutation, device/program selector, approval fulfillment, verifier, or
`task.completed` event.

## Rejected alternatives

- Expanding ADR 0011 was rejected because retrieval resource accounting and
  invocation authority are separate architecture boundaries.
- Keeping a deserializable "canonical" value and validating it in policy was
  rejected because policy would still consume caller-selected authority.
- Letting the model choose a lease and checking only that opaque handle was
  rejected because authority selection belongs to the harness.
- Treating a maximum effect as the derived effect was rejected because it
  over-authorizes safe argument shapes; accepting a value below the minimum
  was rejected because it hides a manifest/deriver mismatch.
- Authorizing string path prefixes was rejected because traversal,
  sibling-prefix, and Unicode aliases can make strings denote unintended
  resources.
- Consuming a lease before all checks or implementing idempotency outside the
  lease mutation critical section was rejected because failures and races can
  lose authority budget or issue duplicate permits.
- Persisting permits or adding an approval/worker protocol was rejected because
  neither is needed to close the current model-to-policy authority hole.
- Rewriting Task 003 events to record permits was rejected because replay does
  not re-authorize or re-execute and its existing durable semantics are already
  fixed by ADR 0009.

## Compatibility and migration

The old public-field `ditto-policy::CanonicalInvocation`, mutable lease
authorization method, and `LeaseGrant` are replaced. This is an intentional
Rust API break at an internal pre-1.0 authority boundary; no public HTTP route
or durable event uses those types. Callers migrate to capability
canonicalization followed by the policy authorizer and sealed permit.

Execution-epoch revision evidence is additive. Existing serialized epochs and
Task 003 version-1 events continue to deserialize and replay, but only newly
bound live epochs are invocable. `artifact.read` keeps capability version
`0.1.0`, its argument/result schemas, range behavior, and durable turn payload
version.

Rollback may remove the additive epoch evidence and new invocation/policy
types. It must retain the ability to read Task 003 version-1 events. Once a
future durable protocol records invocation or permit evidence, rollback will
require an explicit compatibility reader rather than silently treating that
evidence as caller input.

## Measurable consequences

Tests must prove:

- the untrusted wire rejects authority fields;
- private fields, missing `Deserialize`, and absent public struct literals for
  `CanonicalInvocation` and `InvocationPermit` through compile-fail cases;
- rejection of absent epoch entries and every ID/version/manifest/schema/
  deriver mismatch;
- raw and normalized schema-instance rejection before policy;
- minimum and maximum effect mismatch rejection;
- typed artifact resource derivation plus traversal, sibling-prefix, and
  non-NFC path rejection;
- failed authorization leaves lease calls unchanged, one success consumes
  once, an identical retry returns the same permit, a conflicting digest is
  denied, and a one-call concurrent race issues at most one permit;
- an approval-required outcome issues no permit and consumes no lease;
- `artifact.read` uses a matching static-policy permit while retaining exact
  Task 003 live and replay outcomes; and
- no worker/process/network/credential/SSH/file-mutation or `task.completed`
  path is introduced.

