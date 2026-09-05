#!/usr/bin/env bash
set -euo pipefail

required_files=(
  AGENTS.md
  Cargo.lock
  docs/agent/README.md
  docs/agent/NEXT.md
  docs/agent/HANDOFF.md
  docs/agent/QUALITY-GATES.md
)

for file in "${required_files[@]}"; do
  if [[ ! -f "$file" ]]; then
    echo "missing required repository control file: $file" >&2
    exit 1
  fi
done

./scripts/agent-canary.sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
cargo build --locked -p ditto-cli
cargo test --locked -p ditto-daemon runs::tests::built_cli_run_wait_retry_status_and_cancel -- --exact --ignored
cargo test --locked -p ditto-daemon sorts::tests::built_cli_sort_wait_retry_status_and_cancel -- --exact --ignored
