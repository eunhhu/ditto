# Product intent

## Confirmed positioning

On 2026-09-06, the user clarified that Ditto's intended product is a personal
general-purpose agent, with Hermes as a positioning reference. The problems
the user wants Ditto to address are memory footprint, inefficient context
management, unreliable memory and cron jobs, self-improvement that becomes
less efficient over time, task performance, and latency.

This records the user's intended problem and product category. It is not a
measured assessment of Hermes, a compatibility requirement, or evidence that
Ditto already outperforms another agent.

The personal agent is the product. The semantic microkernel is the architecture
used to deliver it. Developer integrations can support the product, but do not
make an agent-building framework the primary user proposition. Personal use is
not limited to software development or server administration.

## Zero cost and zero overhead

The user further confirmed **zero cost, zero overhead** as a primary design
premise, with implementation effort and future technical debt also intended
to converge toward zero. Efficient operation alone is insufficient if the
solution creates continuing development, maintenance, or user-management work.
Zero is the direction of optimization; it is not an achieved-cost claim.

Apply this premise across the product's lifetime:

- Unused capabilities should add no model calls or loaded implementations.
  Avoid repeated work when a verified result can be reused within its scope.
- Remembering, scheduling, and recovery should reduce user effort, including
  the effort of inspecting and correcting failures. Moving hidden work onto
  the user does not count as a saving.
- Prefer the smallest implementation that satisfies the actual contract.
  Reuse existing boundaries and remove duplication. Introduce abstractions,
  dependencies, services, and refactors only for a demonstrated reduction in
  total work or a necessary product behavior.
- Evaluate development effort together with later modification, debugging,
  migration, and operation. A short implementation that creates lasting
  maintenance obligations does not satisfy the premise.
- Learned behavior should reduce repeated work. Count the cost of creating,
  evaluating, retaining, applying, and eventually retiring that behavior.

Measure necessary task/model/tool work separately from Ditto-added overhead,
including resource use, latency, monetary cost, and human or coding-agent
effort. This accounting does not establish whether eliminating external
model/API charges is also required; that cost boundary remains to be clarified.
Savings must preserve task quality, memory and schedule reliability, and the
existing authority invariants. Proportional evidence should reuse existing
checks rather than introduce a new recurring reporting burden.

## User experience to build

A person can delegate varied work, reuse relevant personal context, and arrange
future or recurring work without continually tending the agent itself. They
can inspect and correct what it remembers, understand the status of scheduled
work, and see evidence for reported results. The frontier model retains freedom
to decide how to approach the task within the user's authorized scope.

As history and available capabilities grow, the agent should continue to find
what matters without loading everything into RAM or every model request.
Remembering more should help the current task rather than bury it in irrelevant
history. Scheduling should have visible, dependable outcomes, including when a
run is interrupted. Learned behavior should improve work without an accumulating
burden of redundant instructions, skills, background calls, or conflicting rules.

Local-first means the user's environment owns durable work state and control.
It does not promise that every model inference runs locally.

## Product outcomes and proposed evaluation

The outcomes below translate the confirmed intent into proposed measurements.
They are not new runtime contracts, fixed acceptance thresholds, or completed
features. Establish baselines before setting targets.

| User concern | Desired outcome | Evidence to collect |
| --- | --- | --- |
| Process memory footprint | Keeping the agent available and adding capabilities remain affordable on a personal machine. | Idle and peak RSS, loaded runtime count, and growth with catalogue and history size. |
| Context efficiency | Each task gets enough relevant context with little unnecessary input. | Relevant-context recall, irrelevant inclusion, input tokens, cache reuse, and task success on the same workload. |
| Memory reliability | Useful facts remain retrievable; corrections and outdated facts behave predictably. | Recall and correction scenarios across sessions and restarts, stale-fact use, and time needed for user correction. |
| Scheduled work reliability | Users know what was scheduled, what ran, and what needs attention. | Missed and duplicate runs, schedule-to-start delay, and interruption/restart recovery under an explicitly defined delivery policy. |
| Sustainable self-improvement | Accumulated experience improves outcomes without steadily increasing overhead. | Before/after task success, tokens, latency, and active learned-rule count; unrelated-task regression and rollback checks. |
| Task performance and latency | Delegated work finishes correctly and promptly with less user intervention. | Verified task success, user interventions, time to first useful progress, end-to-end p50/p95 latency, model calls, and tool time. |
| Development effort and technical debt | New behavior reduces total work while implementation and future maintenance burden converge toward zero. | Time to implement and safely change behavior, rework, manual setup/recovery steps, and recurring dependency or migration obligations. |

Distinguish process RAM from remembered personal information and model context;
they are separate resources and require separate measurements. Reducing context
or calls is useful only when task quality remains acceptable. Durable history
may grow; measure that growth separately while driving avoidable working and
maintenance costs toward zero. A zero-cost target is not evidence of constant
storage or already eliminated maintenance.

Evaluate repeated use as well as fresh installation: small and large histories,
many capabilities, recurring schedules, corrections, interruptions, and
accumulated learning. Separate startup/recovery cost from steady-state cost.
For comparisons, record the workload, model, reasoning settings, hardware,
available tools, software versions, and verified outcomes so harness and model
effects can be distinguished.

## Planning implications

- Use zero cost and zero overhead as a first-order decision criterion, including
  development effort and future debt. Evaluate the simplest viable alternative
  before adding another subsystem or layer of process.
- Tie each infrastructure slice to one of these user outcomes and an observable
  scenario. Security and authority invariants remain mandatory constraints.
- Connect an everyday end-to-end agent workflow and its measurements early;
  evaluate long-use degradation alongside immediate task success.
- Treat durable memory, scheduled work, resource use, and sustainable learning
  as central product concerns. Their implementation still requires bounded
  task contracts and evidence.
- Add or promote learned behavior only when measured benefits justify its cost
  and unrelated behavior remains acceptable. Existing promotion boundaries
  continue to apply.

This clarification changes product framing, not the implementation frontier or
accepted architecture contracts. `docs/agent/NEXT.md` still owns task ordering.
The existing read-only loop and persistence foundations do not establish a
complete personal agent, a scheduler, production semantic retrieval, or a
self-improvement system. Implemented and deferred capabilities remain recorded
in `README.md` and `docs/agent/HANDOFF.md`.
