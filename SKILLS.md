# SKILLS.md

Repo "skills" are the named developer tasks for Rift. They are runnable via
the [`skills.sh`](skills.sh) wrapper and described machine-readably in
[`skills.json`](skills.json).

## Running

```bash
./skills.sh <task> [args]
./skills.sh list          # show all tasks
```

## Tasks

| Task        | What it does                                              | Underlying command |
| ----------- | -------------------------------------------------------- | ------------------ |
| `fmt`       | Format all crates                                        | `cargo fmt --all` |
| `fmt-check` | Verify formatting (no writes)                            | `cargo fmt --all --check` |
| `lint`      | Clippy across the workspace, warnings denied             | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `test`      | Full workspace test suite                                | `cargo test --workspace --all-features` |
| `build`     | Debug build                                              | `cargo build --workspace` |
| `release`   | Optimized release build (matches CI)                     | `cargo build --workspace --release` |
| `check`     | **Pre-push gate**: `fmt-check` + `lint` + `test`         | — |
| `run`       | Run the `rift` binary, forwarding args (`run -- status`) | `cargo run -p rift -- …` |
| `tui`       | Launch the Canopy dashboard (bare `rift`)                | `cargo run -p rift` |
| `memd`      | Run the `rift-memd` recall binary (`memd -- serve`)      | `cargo run -p rift-memd -- …` |
| `smoke`     | Quick smoke: `rift` + `rift-memd` report status          | — |

## Conventions

- **`check` is the gate**: run `./skills.sh check` before pushing. CI runs the
  same three steps plus `release`.
- `skills.json` is the source of truth for the task list/descriptions; keep it
  and `skills.sh` in sync. The runner is dependency-free (no `jq` required).
- To add a task: add an entry to `skills.json`, a `case` arm in `skills.sh`, and
  a row here.
