# ADR 0044: a profile is the isolation boundary

Date: 2026-11-09
Status: accepted

## Context

The user wants multi-profile support: several assistants on one machine, each
with its own memories, instructions, skills, schedules and provider, the way
Hermes profiles already work for them ("기본"/"민채"/"h1").

The journal is the source of truth, single-writer, and replay must reproduce a
turn from it exactly. Two ways to serve several profiles exist:

1. one daemon, several profiles inside it, every read filtered by a profile
   column;
2. one daemon per profile, each with its own data root.

Filtering inside one process makes the boundary a predicate that every future
read must remember: one forgotten `where profile_id = ?` silently mixes two
people's memories, and replay of a mixed journal cannot tell which turn belonged
to whom. It also puts several writers behind one lock for no gain.

## Decision

A profile is a directory. The daemon takes `--profile <name>`, resolves it to a
data root, and serves exactly that profile; the journal, memory store, skills,
installed capabilities, instruction layer and schedules all live under it. Nothing
reads across profiles, because nothing can address across them.

A profile name is `[a-z0-9][a-z0-9_-]{0,31}`. The default profile is `default`,
so an existing installation keeps its data root untouched. `ditto profile list`
reports the profiles it can see, and two profiles run as two daemons on two ports
- the same shape the user already runs for other assistants.

Exporting between profiles is an explicit command (`ditto profile export`), not a
query, so a copy is always a decision.

## Consequences

- Replay stays exact: one journal, one writer, one profile.
- The boundary is testable by inspection - two data roots cannot intersect - and a
  test asserts that a memory saved in one profile is absent in the other.
- Running many profiles costs one process each, which is honest for a
  personal-computer agent and keeps failures local.
- Anything genuinely shared between profiles (a downloaded model, a provider
  credential) has to be named as shared on purpose, in a later slice, rather than
  becoming shared by accident.
