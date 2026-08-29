# Capability Manifest

A capability is metadata plus an isolated implementation. Merely discovering a manifest must not start its runtime.

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
maximum = "privileged"
resources = ["device:{device_id}", "path:{cwd}/**"]

[policy]
approval = "risk-based"
secret_handles = ["device-credential:{device_id}"]

[verification]
default = "exit-code-and-expected-output"
```

## Retrieval contract

Descriptions, intents, aliases, negative examples, prerequisites, complements, effect metadata, placement, health, and observed latency may influence ranking. Embedding similarity only narrows candidates; it never bypasses hard filters or policy.

## Runtime contract

Runtime types are `builtin`, `process`, `wasi`, `mcp`, and `remote`. Non-builtin implementations run outside the daemon. The invocation envelope will include a run ID, capability ID and version, normalized arguments, placement, effect claim, lease handle, timeout, resource limits, idempotency key, and expected evidence.

## Disclosure levels

1. Namespace map: stable, tiny, normally present.
2. Capability card: ID, purpose, placement, and maximum effect.
3. Full input/output schema: paged into the current execution epoch.
4. Runtime: started immediately before the first invocation and stopped after its idle TTL.
