# Security Policy

Ditto is an effectful agent runtime. Treat model output, public commands, remote
responses, and third-party capabilities as untrusted input.

## Reporting

Report vulnerabilities privately through GitHub's security advisory flow. Do
not open a public issue for credential exposure, sandbox escapes, lease bypasses,
command injection, path-scope bypasses, audit forgery, or remote execution bugs.

## Current boundary

The current runtime records trusted user-input commands, streams durable events,
loads capability metadata, and stores content-addressed artifacts. It does not
yet execute capability workers or SSH commands.

- Public clients cannot choose event actor or internal event kind.
- The default daemon refuses non-loopback binding unless an explicitly named
  unsafe escape hatch is supplied. That flag is not authentication.
- Artifact object directories are private on Unix; objects are opened with
  no-follow behavior and verified through the same descriptor used for reads.
- Context pinning and policy-required inclusion are trusted compiler directives,
  not durable model-authored fields.
- Effects are orthogonal profiles. Elevated privilege does not imply mutation,
  communication, or credential authority.
- A scoped lease rejects omitted device, program, or resource fields.

## Future execution requirements

- secrets are resolved by opaque handles outside model context;
- capabilities run out of process;
- arguments are validated and canonicalized before effect derivation;
- policy consumes canonical invocations, never raw model claims;
- privileged actions require bounded, expiring leases;
- transports revalidate canonical resources at execution time;
- process groups support cancellation and resource limits;
- the append-only audit stream never stores raw credentials;
- provider or process completion is not task verification.
