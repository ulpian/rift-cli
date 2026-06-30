# rift-fuzz

`cargo-fuzz` targets for Rift's untrusted-byte parsers. This is a **separate
workspace** (its own `[workspace]` in `Cargo.toml`) so the main
`cargo build/test --workspace` on stable never builds it — libFuzzer needs a
nightly toolchain.

## Run

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run config_parse                 # short interactive run
cargo +nightly fuzz run config_parse -- -max_total_time=60   # CI smoke budget
```

Each target's contract (testing-strategy 06 §6.1.6, security-model §5.9): for any
input it must **never panic, hang, or OOM**. A crash is a security bug; minimize
it (`cargo fuzz tmin`), add a regression unit test in the parser crate, and add
the input to the seed corpus.

## Targets

| Target | Parser | Status |
|---|---|---|
| `config_parse` | `rift_config::EffectiveConfig::from_bytes` | **live** |
| `checkpoint_artifact` | rift-shadow checkpoint manifest | deferred — Stage 2 |
| `mcp_request` | rmcp request decode | deferred — Stage 3 P5 |
| `fff_query` | fff query language | deferred — Stage 5 P2 |
| `tool_result_template` | tool-result templating | deferred — Stage 3 |

Only `config_parse` exists today because it is the one untrusted parser present
in Stage 1; the others are added as their parsers land. The proptest
`parser_never_panics_on_arbitrary_bytes` in `rift-config` gates the same contract
in the normal `cargo test` job (no nightly required).

Seed corpora live in `corpus/<target>/` and are committed (valid examples + tricky
cases). Crash artifacts (`artifacts/`, `target/`) are git-ignored.
