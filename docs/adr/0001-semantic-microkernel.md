# ADR 0001: Semantic microkernel

- Status: accepted
- Date: 2026-08-29

## Context

Frontier models benefit from freedom, while personal-agent runtimes need strict ownership of persistence, context, capabilities, effects, and execution lifetime. Rigid workflow engines add latency and suppress model competence; loose wrappers expose too much authority.

## Decision

Ditto is a semantic microkernel. The model owns intent, strategy, and judgment. The harness owns the model-visible world and all external effects. Planner, executor, reviewer, and persona agents are not mandatory architecture components.

## Consequences

Fast-path turns remain small. Complex work can acquire durable task state without forcing every turn through the same workflow. Provider features are preserved through feature-aware drivers rather than flattened into one lowest-common-denominator API.
