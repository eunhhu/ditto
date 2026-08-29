# Isolated capability workers

Workers start lazily and communicate through the contracts in
`ditto-capability-runtime`. They do not load into the daemon process. Idle TTL,
health state, cancellation, and resource handles remain kernel-owned.
