# Decision protocol

Use an ADR when a change alters a durable or public boundary rather than a local
implementation detail.

An ADR must contain:

- context and the failure being prevented;
- decision and ownership boundary;
- rejected alternatives and why;
- compatibility or migration impact;
- measurable consequences and rollback strategy.

Do not add an ADR for routine refactoring. Do not silently contradict an
accepted ADR; supersede it explicitly.

Before introducing a framework, database, daemon, runtime language, protocol,
or mandatory service, demonstrate why the current one-binary/local-storage
constraint cannot meet the measured requirement.
