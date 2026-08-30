# Capability Manifest

A capability is metadata plus an isolated implementation. Discovering a
manifest must not start its runtime.

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
may therefore compose with a local artifact reader. Complements are validated at
catalogue load and deduplicated across ranked roots and expansions.

Descriptions, intents, aliases, negative examples, prerequisites, complements,
health, and observed latency may influence ranking. Embedding similarity only
narrows candidates; it never bypasses hard filters or policy.

## Runtime contract

Runtime types are `builtin`, `process`, `wasi`, `mcp`, and `remote`. Non-builtin
implementations run outside the daemon. The canonical invocation will include a
run ID, capability ID and version, normalized arguments, resolved placement,
derived effect profile, lease handle, timeout, resource limits, idempotency key,
and expected evidence.

## Disclosure levels

1. Namespace map: stable and tiny.
2. Capability card: ID, purpose, placements, and minimum/maximum effects.
3. Full input/output schema: paged into one execution epoch.
4. Runtime: started immediately before first invocation and stopped after idle TTL.
