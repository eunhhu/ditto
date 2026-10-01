# ADR 0036: Two agent tools, one tool lifecycle

Status: accepted on 2026-10-01 for Task 035. Turn contract 13 (replacing 12,
as [ADR 0034](0034-one-turn-contract.md) requires). Merges the tools of
[ADR 0027](0027-web-fetch-for-user-links.md), [ADR 0029](0029-memory-search-tool.md),
[ADR 0031](0031-model-managed-memory.md) and
[ADR 0033](0033-autonomous-web-search.md), and moves `artifact.sort`
([ADR 0018](0018-user-scoped-model-sort.md)) onto the shared lifecycle; completes
the "one builtin tool lifecycle" that [ADR 0028](0028-thin-realtime-harness.md)
deferred.

## Context

The user asked for Ditto to stop accumulating modules and tools and to be as
light and thin as possible ("주루루 모듈이나 도구달지말고 종합해서 최대한
가볍고 얇게 짜라"). An ordinary agent run offered five tools (`artifact.read`,
`web.fetch`, `memory.search`, `memory.remember`, `memory.forget`, and
`web.search` with a search service): their definitions were 1,971 of the
4,538 bytes of a request, every request. `artifact.read` was offered although
nothing could be read without an attachment. Each tool had its own event
kinds (eleven `agent.*` kinds), payload types, run module, replay module and
validators, which repeated the same request, claim, start and output rules.

## Decision

- **Two tools for an ordinary run.** `memory.manage` replaces the three
  memory tools: `{"action": "search", "query"}`, `{"action": "remember",
  "text", "replaces"?}` or `{"action": "forget", "memory_id"}`, with the
  rules of ADRs 0029 and 0031 unchanged (search over the compiled memories;
  writes refused after untrusted content, for credentials, for an inactive
  target and past three per turn). `web.browse` replaces `web.fetch` and
  `web.search`: `{"url"}` reads a page linked in the user's message and
  `{"query"}` searches through the configured service, with the leases of
  ADRs 0027 and 0033 unchanged. Its schema offers only what the deployment
  enables, so a deployment without a search service offers no `query`.
- **`artifact.read` only with an attachment.** An agent run offers it only
  with a sort attachment; a legacy artifact turn still requires it first.
- **One lifecycle.** Every call of `web.browse`, `memory.manage` and
  `artifact.sort` journals `tool.requested` (model: the call and its
  normalized arguments), then, for a leased call, `tool.started` (the claim
  evidence, and for a search its request URL), then `tool.output` (whether
  it was claimed, and its result). A stop before authorization is one turn
  failure, "tool call stopped before authorization". Run and replay share one
  set of envelope, claim and output rules, and replay projects the calls as
  `tool_calls`. `memory.written` is unchanged. `artifact.read` keeps the
  original `capability.requested` / `execution.*` events of the legacy
  artifact turn.
- **One table of builtins.** Selection at run time and its rebuild in replay
  read one table of builtin contracts (package, schema, deriver revision).

## Compatibility and rollback

Turns recorded under contract 12 no longer replay; their answers stay
readable in status and history. The five old packages are replaced by
`memory-manage` and `web-browse`. Clients that read the `agent.*` events
(the web app's progress and memory notices) read `tool.*` instead. Rolling
back restores contract 12 and the five packages from git; no stored data
needs migration because nothing is released.

## Consequences

An ordinary request carries two tool definitions instead of five or six, and
the turn module loses one module triplet per tool. Measurements are in Task
035's evidence.

## Alternatives

- **Keeping separate tools and sharing only code.** Leaves the request
  bytes and the model's choice among near-duplicate tools.
- **One tool with free-form commands.** The closed schema profile has no
  `oneOf`; an `action` enum with per-action fields that the deriver checks is
  the closest typed form.
- **Moving `artifact.read` onto `tool.*` too.** Its legacy turn has its own
  checkpoints and failure grammar that a large test suite pins; it can move
  when that turn is retired.
- **Offering a tool only when a message needs it.** Changes the cached
  prompt prefix between turns (ADR 0028 Phase A); the two tools stay stable.
