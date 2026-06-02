#!/usr/bin/env bash
# Dev task runner for rift-cli. Tasks are documented in SKILLS.md and mirrored
# (machine-readable) in skills.json. Usage:
#   ./skills.sh <task> [args...]
#   ./skills.sh list
set -euo pipefail

# Always run from the repo root (the directory of this script).
cd "$(dirname "$0")"

list() {
  cat <<'EOF'
Available tasks (./skills.sh <task>):
  fmt          Format all crates with rustfmt
  fmt-check    Verify formatting (no writes)
  lint         Clippy across the workspace, warnings denied
  test         Run the full workspace test suite
  build        Debug build of the whole workspace
  release      Optimized release build (matches CI)
  check        Pre-push gate: fmt-check + lint + test
  run [args]   Run the rift binary, forwarding args (e.g. run scan .)
  tui          Launch the TUI dashboard
  smoke        Quick end-to-end: scan fixture + dry-run redact
EOF
}

task="${1:-list}"
[ "$#" -gt 0 ] && shift || true

case "$task" in
  list | help | -h | --help)
    list
    ;;
  fmt)
    cargo fmt --all
    ;;
  fmt-check)
    cargo fmt --all --check
    ;;
  lint)
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    ;;
  test)
    cargo test --workspace --all-features
    ;;
  build)
    cargo build --workspace
    ;;
  release)
    cargo build --workspace --release
    ;;
  check)
    "$0" fmt-check && "$0" lint && "$0" test
    ;;
  run)
    # Allow an optional `--` separator: `./skills.sh run -- scan .`
    [ "${1:-}" = "--" ] && shift || true
    cargo run -p rift-cli -- "$@"
    ;;
  tui)
    cargo run -p rift-cli -- tui "$@"
    ;;
  smoke)
    cargo run -q -p rift-cli -- scan tests/fixtures/repo
    cargo run -q -p rift-cli -- redact --dry-run --store tests/fixtures/repo/checkpoints
    ;;
  *)
    echo "unknown task: $task" >&2
    echo >&2
    list >&2
    exit 2
    ;;
esac
