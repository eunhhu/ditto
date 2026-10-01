# ADR 0030: Capability selection recorded by reference

Status: accepted on 2026-10-01 for Task 030. Turn payload version 9.

## Context

After [ADR 0028](0028-thin-realtime-harness.md) Phase C journaled each turn
fact once, one event still repeated itself every turn. `capabilities.selected`
recorded each offered builtin manifest in full, beside the card and revision
derived from it: 4,910 of the 8,837 journal bytes of a short agent turn
(Task 029). Replay already required the sort, fetch and search manifests to
equal their packages and checked every field of `artifact.read` but one, so
the recorded manifests restated what the code holds. Phase C's target of three
journal bytes per answer byte was unmet.

## Decision

- **Durable form.** From turn payload version 9, `capabilities.selected`
  records `epoch_id` and `contracts`: the execution epoch's identity and, in
  page order, each bound contract's revision (capability ID and version,
  manifest and schema digests, deriver revision). Manifests, cards, the
  working-set size and schemas are not recorded.
- **Packages are the store.** Every builtin manifest must equal its package.
  `artifact.read` joins the other builtins in this rule: after its field
  checks, its manifest digest must equal the package's (`lifecycle` was the
  field left unchecked). A differing package fails the turn as before
  (`artifact_read_manifest_mismatch`) or withdraws an optional tool.
- **Replay.** Replay rebuilds the full selection from the packaged contracts
  in the recorded order, then applies the existing checks, which compare each
  recorded revision with the package's. An unknown, duplicate or reordered
  contract, a missing `artifact.read`, or a differing digest or deriver is
  rejected. The replayed turn presents the rebuilt full selection, as
  version 7 presents rebuilt requests.
- **Versions.** Versions 1 to 8 keep the full form. Each version accepts only
  its own form.

## Consequences

Each offered tool costs the journal about 300 bytes per turn instead of about
1,600: a short agent turn with three tools journals 4,879 bytes instead of
8,837 (Task 030). A change to a packaged manifest makes earlier turns'
digests differ from the package, as the recorded manifests already did;
replaying those turns needs the earlier package, as before.

## Alternatives

- **Capability IDs only.** The digests keep a verifiable reference to the
  exact contract for a few hundred bytes; without them a package change would
  be indistinguishable from the contract a turn used.
- **A blob store for manifests.** ADR 0028 Phase C replaced blobs; the
  packages already are content-addressed by these digests.
- **Keeping the cards.** They derive from the manifests.
