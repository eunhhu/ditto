# ADR 0046: A skill is a profile document, disclosed by its header

Status: accepted (Task 044 P3, first half)

## Context

The model needs to know what procedures this installation knows before it can
follow one, but the capability rule is that no full catalogue and no
implementation sits in the request. Hermes solves this with a flat list of skill
names and trigger lines in context and a read tool for the body, and that shape
has held up in daily use.

Ditto already has three places a procedure could live: standing instructions
(pinned, always in force), memories (facts with provenance), and capabilities
(narrow tools with effect profiles). None of them is a procedure: instructions
are constant, memories are facts, and a capability is a thing you invoke rather
than a way of working.

## Decision

A skill is data in the profile's own directory:

```
<profile>/skills/<name>/SKILL.md
---
name: photo-triage
description: Sort a folder of photos into dated albums.
---
# Photo triage
...
```

- **The name is validated, not sanitized.** Lowercase letters, digits, `-`, `_`,
  first character a letter or digit, at most 64. A name that fails is refused by
  name, so `../etc` cannot resolve to anything.
- **A malformed skill is reported, not skipped.** A skill that silently fails to
  load is worse than one that says which line is wrong. The declaration must also
  match its directory, so a copied folder cannot quietly answer to another name.
- **The header is what the model may see before it asks**: name and one line of
  description, at most 200 characters. The body is at most 64 KiB; anything
  larger belongs in a reference file the body names.
- **Reading a body is a read, not an effect.** Installing a skill grants no
  effect, so no lease bounds it. What a skill *does* is still whatever
  capabilities the model then invokes, each with its own profile.
- **A skill is scoped to one profile.** It lives under that profile's root, so
  two profiles cannot see each other's skills any more than their journals.

## Consequences

- The header catalogue is a pinned segment, ordered by name, so its bytes are
  stable while the installation does not change. A skill installed mid-session
  appears at the next turn, which is the same rule as standing instructions.
- Replay does not read the skills directory: a turn records the header text it
  was given, exactly as it records standing instructions (ADR 0045).
- `ditto skills list|show --profile --data-dir` answers the filesystem question
  without a daemon, like `ditto profile`.
- The read tool is a builtin capability in the next slice; this ADR fixes the
  document format and the disclosure level first, because those are the parts a
  model-written skill would have to obey.
