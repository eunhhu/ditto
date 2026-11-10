# ADR 0040: A verified sort returns the lines it sorted

Status: accepted on 2026-11-07 for Task 038. Turn contract 16 (replacing 15, as
[ADR 0034](0034-one-turn-contract.md) requires). Amends
[ADR 0032](0032-artifact-sort-capability.md), which returned only a reference,
a verifier name and line counts.

## Context

An attachment task costs Ditto one extra model request. In the Task 017
campaigns an artifact turn runs four requests where three carry real work:
read the attachment, sort it, read the sorted artifact back, then answer. The
`artifact.read` between the sort and the answer exists only because the sort
result named a reference and the model could not otherwise report the order.

A request is not free. The measured body of a read-only turn is 4.7 KB
(system 3.4 KB, tool schemas 1.1 KB, conversation the rest) and one round trip
takes 4-8 s on the calibrated provider. Across 130 attempts the campaign
averaged 2.66 requests per turn, and the artifact cases (T1-T4, X1-X4, 32
attempts) sat at 3.2-4.0.

Two alternatives were weighed. Letting the harness answer for the model would
take judgment away from it, which the mission forbids. Raising `artifact.read`
limits would not remove the request, only shrink one of its parts.

## Decision

- **The verified result carries up to eight lines of what it produced**, each
  cut to 120 characters, in `lines`, beside `reference`, `verifier`,
  `input_lines` and `output_lines`.
- **Bounded and optional.** `lines` is omitted when empty and defaults when
  absent, so a journal recorded at contract 15 replays unchanged; replay
  validates length and per-line width against the recorded counts.
- **The model still decides.** The result adds evidence; nothing in the harness
  answers for the model, and a model that wants the whole artifact still calls
  `artifact.read`.

## Consequences

- An artifact turn can answer in the sort response, one request shorter.
- The tool output grows by at most 8 x 120 bytes, well under the request it
  replaces.
- Sort output longer than eight lines is still truncated evidence: the model
  must read the artifact when the order beyond the preview matters.
- Journals recorded before contract 16 carry no `lines` and remain replayable.
