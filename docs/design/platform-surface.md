# The platform surface

What makes Ditto a platform rather than one assistant: the parts a user can set
once and rely on across every conversation, and the parts a second person can
install without touching the kernel. Written 2026-11-09 from the user's list -
memory, instructions, skills, the immutable and variable ranges of context,
multi-profile, factory, plugins, and one provider contract.

The kernel stays what it is: context compiled, capabilities paged, effects
leased, improvements promoted. Everything here is a *document with a scope* plus a
boundary, not a new engine.

## 1. Documents, their scope, and what happens after the turn

Six scopes already exist in the context model: `turn`, `session`, `task`,
`project`, `device`, `global`. Every durable document - an instruction, a memory,
a skill, a capability policy - names one of them, and the compiler selects what a
turn may see from the scopes the turn sits inside.

Scope answers *who can see it*. It does not answer *whether it should exist
tomorrow*, and those are different questions (ADR 0045). Three kinds of state, and
the kind is a contract:

| kind | lives | examples | never |
| --- | --- | --- | --- |
| durable document | until superseded or forgotten | memory, instruction, skill, plugin, schedule, conversation turn | changes in place inside a turn |
| session state | one session window, rebuilt from the journal | run history, in-flight runs, session index | cited as a fact |
| turn state | one turn, then only replay | compiled capsule, turn notes, tool results, prefetches | read by a later turn |

Inside one turn a durable document is either **pinned** - compiled before the
first request, byte-stable, budgeted (the instruction block, `DITTO_CONTEXT_V1`)
- or **recallable** - selected by relevance, searchable, writable (the memory
store). A pinned document the model wants to change becomes a proposal: the write
lands for the *next* turn, so this turn's prefix never moves.

## 2. Profiles

A profile is the isolation boundary, and it owns everything a person would call
"their Ditto": the journal, memories, instruction layers, skills, installed
capabilities, provider configuration and schedules. Nothing crosses a profile
boundary except an explicit export.

One daemon serves one profile. That is deliberately dumber than a multi-tenant
server: it keeps the journal single-writer, keeps replay exact, and makes the
boundary a directory rather than a query predicate that every read must remember
to apply. Running two profiles means two daemons on two ports, which is what the
user already does with Hermes profiles.

## 3. Instruction layers

Three layers, each with an owner and a budget:

| layer | owner | mutability | budget |
| --- | --- | --- | --- |
| core | kernel (`INSTRUCTIONS`) | pinned, versioned by ADR | fixed |
| profile | user | pinned, editable between turns | ~2k chars |
| turn notes | kernel | pinned, per turn | tens of chars |

The core layer is not user-editable, because the guarantees in it (never claim an
unrun action, batch lookups, answer in the user's language) are what make the
product honest. The profile layer is where a person writes "I live in Pocheon,
call me 선우, answer in Korean, never book anything without asking". Both are
pinned, so both sit inside the cacheable prefix and neither costs a request.

## 4. Skills

A skill is a directory with `SKILL.md` (name, one-line trigger description, body)
plus optional `references/` and `scripts/`. It is *data*, not code: installing one
grants no new effect.

Progressive disclosure follows the capability rule that already exists - the
header catalogue carries a skill's name and trigger line only, and the body is
read on demand through `skill.read`. A skill whose body is never read costs the
same as a capability whose implementation is never loaded: nothing.

## 5. Plugins

A plugin is one or more capability packages plus an installable manifest:
identifier, version, digest, the effects it may request, and the scopes it
services. The existing `capability.toml` is the inner half of that manifest; the
plugin manifest adds identity, provenance and uninstall.

Install is a digest-pinned copy into the profile, never a fetch at run time:
a capability that changes under a running journal would make replay a lie.
Disclosure stays header-only, and an uninstalled plugin's capabilities leave the
catalogue with it.

## 6. One provider contract

Every provider adapter answers the same three questions in the same shape:
what a request is (`ModelRequest`), what a stream emits (`ModelEvent`), and what
a failure means (`TransportError` with `empty`, `refused`, `rate_limited`,
`context_window`). Adapters additionally declare capabilities: tool calls,
streaming, image input, prompt caching, and structured output.

A conformance suite replays golden fixtures per provider *shape* - chat
completions, responses, local llama.cpp - so a new adapter is correct before it is
useful. The router picks an adapter by profile configuration, not by model name
sniffing.

## 7. Factory

`ditto init <dir>` writes a profile skeleton: provider config, instruction layer,
default skills, empty journal, and a manifest naming the versions it was made
from. `ditto profile clone <from> <to>` copies a profile without its journal, so a
second person starts from the same instructions and skills but their own memory.
Elevation, not configuration, is the scaling story.

## Slice order

Each slice lands with its ADR, its tests, and a measurement where a number
applies.

1. **P1 - the profile boundary.** Data root per profile, `--profile` on the
   daemon and CLI, `ditto profile list`, and a test that two profiles cannot see
   each other's memories. ADR 0044.
2. **P2 - scope and the two mutability classes.** Pinned profile instructions
   with a character budget, and a model write that lands on the next turn.
   *Done for the pinned half:* `instructions.recorded`, `GET/PUT
   /v1/instructions`, `ditto instructions`, and the text recorded inside each
   turn so replay never reads the profile.
3. **P3 - skills.** (done) `SKILL.md` discovery by header, `skill.read`, the
   catalogue as a pinned prefix segment, and its 4,000-character budget
   (descriptions are dropped before names, and a catalogue that cannot fit as
   names refuses the turn).
4. **P4 - plugin manifests.** Install, digest pinning, uninstall, and the effect
   request review.
5. **P5 - provider conformance.** Adapter registry and golden fixtures per shape.
6. **P6 - the native shell.** Tauri over the existing HTTP and streaming API:
   conversation, memory, schedules, approvals, and profile switching.

P6 is last on purpose: it is the surface the user touches, so it should be built
on boundaries that already hold rather than on ones still moving.
