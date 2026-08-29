# Lease boundary checks

Current automated checks cover:

- effect claim cannot exceed lease ceiling;
- program must match explicit allowlist;
- device and resource scope must match;
- opaque lease handle must match;
- expiry and maximum call count deny execution;
- executor never invokes a shell string.

Remote credentials, sudo scope, path canonicalization, and preimage snapshots
must be tested before remote write placement is enabled.
