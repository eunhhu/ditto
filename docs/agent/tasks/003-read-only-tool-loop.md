# Task 003 — Read-only tool continuation loop

## Objective

Complete the first real agent turn with one builtin, read-only capability:
`artifact.read`.

## Flow

```text
trusted input command
→ context capsule
→ execution epoch
→ model request with full artifact.read schema
→ structured tool call
→ argument validation and canonical resource
→ bounded artifact range read
→ deterministic result projection
→ model continuation
→ final response with unverified task status
```

## Constraints

- capability cards are insufficient at invocation time; page the full schema;
- reject malformed references, negative offsets, excessive lengths, and unknown
  call IDs;
- large results remain artifacts or bounded projections;
- no arbitrary file read and no process execution;
- provider stream closure is not task evidence;
- every externally visible transition is durable before presentation.

## Acceptance tests

Cover a successful continuation, malformed arguments, range bounds, artifact
integrity failure, duplicate call ID, cancellation between call and result, and
replay of the complete turn from events.
