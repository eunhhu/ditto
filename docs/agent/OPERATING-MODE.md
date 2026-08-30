# Long-run operating mode

The agent is expected to work autonomously across many edit/test/review cycles
without replacing judgment with a rigid planner pipeline.

## Working-set discipline

At any moment keep one active vertical slice, one explicit next action, and one
short risk list. Record durable state in `HANDOFF.md`; do not rely on a long chat
history to remember project facts.

Before reading another large file, state internally which unresolved question it
will answer. Prefer symbol search and narrow ranges over whole-repository reads.

## Execution cadence

For each slice:

1. Inspect the contract, implementation, callers, and tests.
2. Write the failure model before the happy path.
3. Implement the smallest complete path.
4. Run focused tests immediately.
5. Run the full repository gate before declaring the slice complete.
6. Review the diff for false claims, unused abstractions, authority leaks, and
   context or latency growth.
7. Update handoff and next-task state.
8. Commit.

Continue to the next listed slice when all exit criteria pass and the remaining
work is in scope. Do not stop merely to announce progress.

## Recovery after compaction or interruption

Re-open, in order:

1. `AGENTS.md`;
2. `docs/agent/HANDOFF.md`;
3. `docs/agent/NEXT.md`;
4. the active task file;
5. `git status`, recent commits, and failing checks.

Trust repository evidence over remembered conversation. Re-run the narrowest
command that can verify the current state.

## Avoided failure modes

- architecture-shaped directories containing no integrated behavior;
- declaring a task verified because a model stream or process ended;
- broad refactors that destroy a known-green baseline;
- adding a second source of truth for convenience;
- generating many overlapping skills, prompts, plans, or TODO lists;
- repeatedly rereading static context instead of maintaining a concise handoff;
- hiding test failures behind “scaffold” language.
