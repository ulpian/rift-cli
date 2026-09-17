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
  run [args]   Run the rift binary, forwarding args (e.g. run -- status)
  tui          Launch the Canopy dashboard (bare `rift`)
  memd [args]  Run the rift-memd recall binary (e.g. memd -- status)
  smoke        Quick smoke: rift + rift-memd report status
  coverage     Instrumented run + the per-crate >=95% gate (NEEDS +nightly)
  docs         Rustdoc at the zero-warning baseline (matches the CI docs job)
EOF
}

task="${1:-list}"
[ "$#" -gt 0 ] && shift || true

case "$task" in
  list | help | -h | --help)
    list
    ;;
  coverage)
    # The >=95%-line gate for ci/security-crates.txt, run the way CI runs it.
    #
    # WHY +nightly, and why this task exists at all: `--branch` passes
    # `-Z coverage-options=branch`, which the toolchain rust-toolchain.toml
    # pins (1.96.0 stable) REFUSES -- and `xtask check-coverage` then fails
    # CLOSED on the missing branch data. So on the pinned toolchain the gate
    # can be neither passed nor failed. Someone hit exactly that and reported
    # the gate unrunnable; it is runnable, just not on the pinned channel, and
    # nothing here said so. That is what this task fixes.
    #
    # `clean` first: stale profiles give phantom numbers.
    cargo +nightly llvm-cov clean --workspace
    cargo +nightly llvm-cov --workspace --all-features --branch --no-report
    cargo +nightly llvm-cov report --json --output-path cov.json \
      --ignore-filename-regex 'keychain_os\.rs'
    cargo run -p xtask -- check-coverage cov.json
    ;;
  fmt)
    cargo fmt --all
    ;;
  docs)
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
    ;;
  fmt-check)
    cargo fmt --all --check
    ;;
  lint)
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    # The feature-off leg of CI's clippy job, mirrored here for the same reason
    # it is there: `--no-default-features` is the configuration bins/rift's
    # manifest advertises, and nothing else in this file or in CI builds it.
    cargo clippy -p rift --no-default-features --all-targets -- -D warnings
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
    # Allow an optional `--` separator: `./skills.sh run -- status`
    [ "${1:-}" = "--" ] && shift || true
    cargo run -p rift -- "$@"
    ;;
  tui)
    # Bare `rift` (no subcommand) launches Canopy.
    cargo run -p rift
    ;;
  memd)
    [ "${1:-}" = "--" ] && shift || true
    cargo run -p rift-memd -- "$@"
    ;;
  smoke)
    # Quick smoke: the two binaries report status and the recall plane is up.
    cargo run -q -p rift -- status
    cargo run -q -p rift-memd -- status
    ;;
  *)
    echo "unknown task: $task" >&2
    echo >&2
    list >&2
    exit 2
    ;;
esac
