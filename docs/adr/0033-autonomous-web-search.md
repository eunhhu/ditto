# ADR 0033: Web search on its own; consent as a hand-off

Status: accepted on 2026-10-01 for Task 032. Turn payload version 11. Amends
[ADR 0027](0027-web-fetch-for-user-links.md), which left web search waiting for
per-call approval. Amended by [ADR 0036](0036-two-agent-tools-one-lifecycle.md): searching is the `query` request of `web.browse`.

## Context

ADR 0027 kept web search back until each query could be approved, because a
model that chooses what to send outside the machine could leak private
memories. On 2026-10-01 the user asked for the approval layer to be a
hand-off, and for Ditto to work on its own ("알아서 잘 딱"): nothing should
stop and wait for a click.

A search does not need per-query consent to be safe, if the model cannot
choose where its query goes. The leak ADR 0027 guards against needs a
destination the attacker controls; a search goes only to the service the
operator configured.

## Decision

- **Consent is a hand-off.** No turn ever waits inside for a person. When a
  step needs the user (a decision, information only they have, or consent to
  something irreversible outside the conversation such as spending money or
  contacting others), Ditto says exactly what it needs in its answer and
  stops; the user's next message continues. From version 11 the instructions
  say so, and tell the model to use the supplied tools on its own otherwise.
  None of today's tools needs consent: each is bounded by what the harness
  grants. A future tool that does will hand off through the harness, and its
  ADR will define the grant its approval carries.
- **Web search.** `web.search` is offered to agent runs when the operator
  configures a SearXNG-compatible endpoint (`--search-url`); without one it
  is not offered. Input `{ query }`, 1 to 200 characters. The resource is the
  configured endpoint itself, fixed by the operator and never by the model,
  and the request is `<endpoint>/search?q=<query>&format=json`. Effect: read
  content, no mutation, network, user. A turn lease of three calls scoped to
  exactly the endpoint bounds the searches. A credential-shaped query is
  refused and never sent. The request uses the hardened client of ADR 0027;
  the operator's endpoint may be on the local network. At most five results
  return, `{ title, url, snippet }`, bounded, as untrusted search results.
- **Records.** `agent.search.requested` (model), `agent.search.started`
  (capability: claim evidence and the request URL) and `agent.search.output`
  (capability: `found` with the results, or `error`). Replay validates the
  normalization, that the request URL is exactly what the query becomes,
  the budget, the claim and the bounds, and feeds the recorded results back
  without network I/O.
- **Untrusted content.** After a `web.search` call, memory writes in the turn
  are refused, as after a fetch ([ADR 0031](0031-model-managed-memory.md)).

## Consequences

Ditto can answer questions about current or outside facts without the user
pasting links, with every search journaled and replayable. Every agent-run
request carries the 393-byte autonomy instruction, and, with a search
service, the search tool's definition. The search
service sees queries, which may carry what the conversation is about; the
operator chooses that service. Result pages are not read yet: only links the
user sends are fetched.

## Alternatives

- **Per-query approval, by a click or as a hand-off.** A round trip for every
  query, for a leak the fixed destination already closes.
- **Letting the model fetch any URL.** An exfiltration channel; ADR 0027
  stands for model-chosen destinations.
- **Search APIs that need a key.** A credential and a cost; a self-hosted
  SearXNG is local-first and free.
