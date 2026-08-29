# ADR 0003: SSH is transport, not a model tool

- Status: accepted
- Date: 2026-08-29

## Context

A raw `ssh(host, command)` tool forces the model to handle credentials, shell quoting, placement, sudo, timeouts, process lifetime, and policy. It also makes local and remote execution semantically inconsistent.

## Decision

The model invokes a structured capability against a registered device. The harness selects local, SSH, container, or remote transport. Credentials remain opaque. Every invocation carries an effect claim and bounded lease.

## Consequences

Capability semantics remain placement-independent. Device and secret policy live in one firewall. Raw shell may exist only as a high-risk escape hatch with preview, timeout, process-group cancellation, and explicit scope.
