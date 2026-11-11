# ADR 0047: the sort permission states whether the user asked for deduplication

Status: accepted (2026-11-11). Amends ADR 0041.

## Context

ADR 0041 lets a turn run the sort its permission fixes before the first model
request, so the model reads the ordered lines instead of spending a round trip
asking for them. That prefetch fired only when `allow_deduplicate` was false:
with deduplication permitted, the harness could not tell a user who wanted
repeats kept from one who wanted them removed, and spending the one-shot lease
on a guess would leave the model unable to correct it.

The suite priced that caution. On the 130-attempt campaign every request above
one per turn sat in one of three groups: memory verification (21 turns),
attachment sorts whose permission allowed deduplication (19 turns), and one
attachment read. The deduplication group's user messages say it outright -
"remove duplicates", "drop repeated tags", "drop repeats" - so the harness was
declining to act on information the user had already given.

## Decision

`AgentSortPermission` gains `deduplicate` (default false, so journals written
before this ADR replay unchanged):

- `deduplicate: true` - the user chose the deduplicated order. The turn
  prefetches `unique: true`.
- `allow_deduplicate: true, deduplicate: false` - the user permits either order.
  The turn prefetches nothing and the model decides, exactly as before.
- `allow_deduplicate: false` - the user forbade it. The turn prefetches
  `unique: false`.

`deduplicate` without `allow_deduplicate` is rejected at admission: a choice the
permission forbids is not a permission. `SortGrant` carries the choice, so the
prefetched arguments stay a property of the journaled input, and
`permission_matches` compares it, so a retry that changes the choice conflicts
instead of silently reusing the stored artifact.

The prefetch still fixes arguments rather than inferring them: the client owns
the reading of "remove duplicates", and the kernel only acts on the flag it was
given. The evaluation suite declares `deduplicate_requested` per case, and the
harness passes it as the permission - the same thing a client that read the
message would send.

The turn also tells the model when the sort already ran: the permission blob in
the user message carries `already_ran`, `calls_used: 1` and the id of the
context node holding the ordered lines, so a model that would have called
`artifact.sort` again knows the lease is spent before it spends a request
learning it.

## Consequences

A "remove duplicates" turn now ends in one model request instead of two, and its
replay still derives the prefetch from the recorded permission. A turn whose
permission allows either order keeps the old cost and the old freedom: the model
chooses, which is the one case where the choice genuinely is the model's.

The suite reports `deduplicate_requested` as a required case field; a case that
allows deduplication must state whether its request asks for it, so the
permission under measurement is never implied by a request string the kernel
cannot see.
