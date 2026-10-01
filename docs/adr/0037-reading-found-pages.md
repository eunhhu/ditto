# ADR 0037: Reading the pages a search found

Status: accepted on 2026-10-01 for Task 036. Turn contract 14 (replacing 13,
as [ADR 0034](0034-one-turn-contract.md) requires). Amends
[ADR 0027](0027-web-fetch-for-user-links.md), whose grant held only the links
in the user's message, [ADR 0033](0033-autonomous-web-search.md), which left
result pages unread, and the leases [ADR 0036](0036-two-agent-tools-one-lifecycle.md)
kept unchanged.

## Context

Since ADR 0033 Ditto searches the web on its own, but it reads only the
titles and snippets of the results: a page is read only when the user sends
its link. Answers about current or outside facts rest on snippets of at most
400 characters, or on the user pasting the link Ditto just found. Other
assistants read the pages their searches return.

ADR 0027 refused model-chosen destinations, because a model that reads
private memories and untrusted pages, and can choose a URL, can leak those
memories by writing them into the URL. That leak needs a destination the
model composes. A search result's URL is not composed by the model: the
operator's search service returned it, after the query had already gone to
that service.

## Decision

- **Found pages join the grant.** A turn's read grant is the canonical URLs
  of the user's message (ADR 0027) followed by the URLs of the results each
  of its searches returned, in order and without duplicates, from that
  search's `tool.output` on. A read must name one of them exactly; any other
  URL, including a result's before its search returned it, is
  `permission_denied` without network I/O. A deployment that does not read
  pages (`--disable-web-fetch`) grants none.
- **One read lease for both.** With a search service the read lease allows
  three calls in total, shared by the user's links and found pages; without
  one it keeps min(links, 3). The lease's exact `url:` resources grow as
  searches return results: `InvocationAuthorizer::extend_lease_resources`
  adds exact resources to a registered lease without changing its budget or
  expiry. The search lease, the fetch policy (public addresses only, pinned
  connections, manual redirects, size and time bounds) and the results'
  label as untrusted content are unchanged, so a service that returns links
  into the local network cannot make Ditto reach them.
- **Replay.** Replay grows the grant from the recorded, bounded results of
  each search in the same order, and checks every read's start, budget and
  refusal against the grant at that point, without network I/O.
- **Wire.** No event or payload changes. The `web.browse` summary for a
  deployment that reads and searches says a page may be one "whose link the
  user sent in this message or a search in this turn returned"; the package
  summary and intents say the same.

## Compatibility and migration

Turns recorded under contract 13 no longer replay, as ADR 0034 requires;
their answers stay readable in run status and thread history. Nothing is
released, so no stored data needs migration.

## Consequences

Ditto can search, read the pages it finds and answer from them in one turn,
with every page journaled and replayable. A turn reads at most three pages,
about 72,000 characters of untrusted text, and memory writes stay refused
after any of them (ADR 0031).

The model chooses which result to open, so a page that injects instructions
could steer a later query and pick among the results it returns; a site
that has search engines index pages for each value of a small secret could
learn that value from which of its pages Ditto opens. The query that would
carry the secret already reaches the operator's search service, credential-
shaped queries are refused, and nothing outside the returned results can be
reached. Links inside a read page stay unreadable, since a page could list a
link for every value without any search engine.

## Alternatives

- **Following links in read pages too.** Removes the search engine from the
  lookup channel above; deferred until a need outweighs it.
- **Granting only results of searches made before any page was read.**
  Breaks search, read, refine-the-search loops for a channel that already
  needs pre-indexed pages, so it is not taken.
- **Asking before each found page is read.** ADR 0033 made consent a
  hand-off; a page read changes nothing outside Ditto.
- **A separate lease or budget for found pages.** Two budgets for one kind
  of call; the shared three bound the context as before.

## Rollback

Restore contract 13 and the user-links-only grant from git; the lease
extension has no other caller. No stored data changes.
