# ADR 0046: A skill is a profile document, disclosed by its header

Status: accepted (Task 044 P3, both halves; amended 2026-11-13: misnamed
directories and the bounded read)

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
  A directory under `skills/` whose name the rule refuses (`Photo Triage`) is a
  skill that fails to load and is reported the same way; only hidden entries
  (`.git`, an editor's folder) and plain files are not skills. Until 2026-11-13
  a misnamed directory was silently left out of the catalogue.
- **A document is read once and no further than 68 KiB** (the body limit plus
  4 KiB of frontmatter), and only when it is a regular file, because the
  catalogue is read on every turn: an oversized file or a FIFO named `SKILL.md`
  would otherwise cost each turn its whole size or stall it.
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
  stable while the installation does not change. It is capped at 4,000
  characters: past that, descriptions are dropped before any name, and a
  catalogue that cannot fit even as names refuses the turn (a model shown half a
  catalogue cannot know what it was not shown). A skill installed mid-session
  appears at the next turn, which is the same rule as standing instructions.
- Replay does not read the skills directory: a turn records the header text it
  was given, exactly as it records standing instructions (ADR 0045).
- `ditto skills list|show --profile --data-dir` answers the filesystem question
  without a daemon, like `ditto profile`.
- `skill.read` is a builtin capability (`capabilities/core/skill-read`) offered
  only when the profile carries at least one skill: an empty catalogue costs no
  tool surface. Its contract is content access, no mutation, local, and it names
  no resource, so the static no-approval policy can permit it without a lease.
- A refusal is a tool result, not a turn failure: `unknown_skill` and
  `invalid_arguments` reach the model as `{"error": ...}` with `is_error` set, so
  a model that guessed a name can recover in the same turn.
- The result has one wire shape and one model view: replay rebuilds the
  conversation from the journal, so two spellings of the same result would make
  every recorded turn disagree with itself.
- Replay checks a recorded read against its arguments and the format limits
  rather than re-reading the directory: a skill body is external mutable state
  that may have changed since the turn, so the journal, not the filesystem, is
  the record of what the document said.
