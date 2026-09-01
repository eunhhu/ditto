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
→ sealed live-epoch capability binding resolution
→ Ditto Invocation Schema Profile V1 instance validation
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

### Task 005.1 pre-merge authority correction

Task 005.1 corrects four pre-merge gaps in the initial implementation. The
replayable epoch record is evidence, not live authority; invocation validation
uses an explicitly closed Ditto profile rather than claiming complete JSON
Schema evaluation; authorization state is owned by one live epoch rather than
the daemon; and any future effectful worker must consume a one-shot execution
claim rather than accepting a reusable permit.

### Exact live epoch and revision binding

`ExecutionEpochEvidence` is the bounded serializable and deserializable record
used by model disclosure, durable events, and replay. It cannot authorize a
live invocation. `LiveExecutionEpoch` is a separate sealed,
non-deserializable, non-serializable harness object. Only a live epoch may issue
an `InvocableCapabilityBinding`, whose fields are private and which has no
wire constructor.

An invocable live binding binds all of these values:

- execution epoch ID;
- capability ID and semantic version;
- SHA-256 digest of the complete canonical manifest value;
- SHA-256 digest of the complete provider-neutral capability schema;
- stable capability-specific deriver revision.

The binding owns the exact model-visible card, complete schema, manifest, and
revision selected into that live epoch. `InvocationCompiler` consumes the
sealed binding rather than an epoch ID or replay evidence and rederives their
complete relationship before normalization. Canonical manifest and schema
digests are computed from their deterministic
canonical JSON projections, not filesystem paths or TOML formatting. An epoch
may retain legacy discovery-only cards, but a card without a complete revision
binding is not invocable. Resolution compares the binding's model-visible card,
manifest, exact level-2 schema, revision, live epoch ID, and deriver before any
argument normalization. It checks the deriver revision again after derivation.
A changed ID, version, manifest digest, schema digest, deriver revision, absent
epoch entry, retired capability, or partially bound epoch fails closed.

The revision binding is projected into additive `ExecutionEpochEvidence` so a
recorded selection can be inspected. Deserializing that evidence never
reconstructs `LiveExecutionEpoch` or `InvocableCapabilityBinding`. Legacy Task
003 epochs without the additive evidence remain replayable but cannot authorize
a new live invocation.

### Ditto Invocation Schema Profile V1 and deterministic derivation

Provider-neutral level-2 schemas remain Draft 2020-12 disclosure documents, but
Ditto does not claim to implement the full Draft 2020-12 evaluator. A schema
must additionally fit the closed Ditto Invocation Schema Profile V1 before it
can enter a live binding. The profile supports boolean schemas and only the
enumerated annotation, type, equality, integer-bound, string, array, and object
keywords needed by the reference capability and its conformance suite.
Unknown keywords, references, combiners, floating-point `number`, and other
unimplemented semantics fail closed rather than being silently ignored.

Profile V1 treats a JSON integer as a syntactically integral `i64` or `u64`.
Thus `1` is an integer and `1.0` is not. Integer bounds and `multipleOf` use
exact signed 128-bit arithmetic over admitted `i64`/`u64` values, including
integers beyond 2^53. `const`, `enum`, and `uniqueItems` use recursive,
representation-sensitive JSON equality, so `1` and `1.0` are distinct. These
rules are intentional Ditto profile semantics and not a claim about full JSON
Schema numeric equivalence.

Before any recursive structural validation, an iterative preflight enforces
the fixed serialized-byte, JSON-depth, and node-work bounds for the complete
schema value. Raw and normalized instances receive the same bounded preflight
and profile evaluation. Regular expressions use the fixed Rust-regex subset
and size/work envelope. No profile failure reaches policy.

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

One live-epoch-scoped authorization ledger serializes invocation-ID binding,
idempotency lookup, all lease checks, lease consumption, and permit insertion
under one mutex. The first attempt binds an invocation ID to its digest for the
remaining live-epoch authority window. A different digest for that ID fails
closed even if the earlier attempt was denied. A failed check or approval-
required result consumes no call. A
successful lease authorization decrements at most once and stores the permit in
the same critical section. An identical retry returns the same permit without
another decrement. Therefore concurrent authorization against a lease with one
remaining call can issue at most one new permit.

The authorizer borrows the sealed live epoch and has its own fixed expiry. It
rejects invocations from any other epoch and caps permit or approval expiry at
the epoch boundary. The kernel constructs it inside the turn and drops it with
the live epoch. Denied ID bindings, expired permits, approval requests,
completed decisions, and claim markers therefore cannot accumulate in daemon-
lifetime state.

Approval fulfillment, durable policy storage, and cross-process authorization
coordination are deferred. An approval-required outcome is not a permit and
cannot execute anything.

### One-shot execution claim for future effectful workers

An `InvocationPermit` authorizes policy intent but is cloneable for idempotent
retry and inspection, so a future effectful worker must never accept a permit
directly. Before dispatch, the epoch authorizer must atomically consume the
permit's one claim slot and issue a sealed, non-cloneable,
non-deserializable `ExecutionClaim` bound to the same epoch, permit ID, and
invocation digest. A second claim attempt fails closed. A future worker API must
consume that claim by value exactly once.

Task 005.1 defines and tests claim issuance but adds no worker and does not route
the existing read-only `artifact.read` executor through the future effectful-
worker interface.

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

This decision ends at canonical invocation, permit issuance, and the one-shot
claim contract plus the guarded use of the already existing bounded
`artifact.read` executor. It adds no worker,
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
  neither is needed to close the current model-to-policy authority hole. The
  one-shot claim is only the mandatory future worker ingress token, not a worker
  protocol or execution implementation.
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
Task 003 version-1 events continue to deserialize as
`ExecutionEpochEvidence` and replay, but deserialization cannot create live
authority. Only a fresh `LiveExecutionEpoch` can issue an invocable binding.
`artifact.read` keeps capability version
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
- compile-time rejection of `LiveExecutionEpoch` and
  `InvocableCapabilityBinding` deserialization and public construction;
- proof that replay evidence cannot enter `InvocationCompiler`;
- raw and normalized schema-instance rejection before policy;
- profile conformance for representation-sensitive `const`, `enum`, and
  `uniqueItems`; exact integers beyond 2^53; exact integer `multipleOf`; bounded
  deeply nested schemas; and rejection of `artifact.read` `length = 1.0` with
  the existing Task 003 invalid-argument projection;
- minimum and maximum effect mismatch rejection;
- typed artifact resource derivation plus traversal, sibling-prefix, and
  non-NFC path rejection;
- failed authorization leaves lease calls unchanged, one success consumes
  once, an identical retry returns the same permit, a conflicting digest is
  denied, and a one-call concurrent race issues at most one permit;
- an approval-required outcome issues no permit and consumes no lease;
- epoch mismatch and expiry rejection, daemon-state absence, and one-shot
  `ExecutionClaim` issuance with a closed second claim;
- `artifact.read` uses a matching static-policy permit while retaining exact
  Task 003 live and replay outcomes; and
- no worker/process/network/credential/SSH/file-mutation or `task.completed`
  path is introduced.
