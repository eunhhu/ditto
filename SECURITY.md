# Security Policy

Ditto is an effectful agent runtime. Treat every model-generated action and every third-party capability as untrusted input.

## Reporting

Please report vulnerabilities privately through GitHub's security advisory flow for this repository. Do not open a public issue for credential exposure, sandbox escapes, lease bypasses, command injection, path-scope bypasses, or remote execution vulnerabilities.

## Current security boundary

The current scaffold records events and loads capability metadata. It does not yet execute capability workers or SSH commands. Future execution features must preserve these rules:

- secrets are resolved by opaque handles outside model context;
- capabilities run out of process;
- every side effect carries a typed effect claim;
- privileged actions require a bounded, expiring lease;
- transport implementations revalidate canonical resources at execution time;
- the append-only audit stream never stores raw credentials.
