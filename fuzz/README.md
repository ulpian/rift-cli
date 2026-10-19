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

Each target's contract, from the testing strategy and the security model: for any
input it must **never panic, hang, or OOM**. A crash is a security bug; minimize
it (`cargo fuzz tmin`), add a regression unit test in the parser crate, and add
the input to the seed corpus.

## Targets

| Target | Parser | Status |
|---|---|---|
| `config_parse` | `rift_config::EffectiveConfig::from_bytes` | **live** |
| `glob_match` | `rift_config::glob_match` (policy pattern × path) | **live** |
| `run_hook` | `rift_shadow_hooks` stdin envelope + full entrypoint | **live** |
| `redact` | `rift_shadow_redact` transcript redaction | **live** |
| `redact_jsonl` | `rift_shadow_redact`'s JSON walker, fed structure rather than noise | **live** |
| `redact_fallback` | `rift_shadow_redact`'s per-line fallback, under packs that never settle | **live** |
| `diff` | `rift_shadow_attribution` unified-diff parse | **live** |
| `checkpoint_artifact` | `rift_recall_substrate::parse` (all five shard entrypoints) | **live** |
| `mcp_request` | `rift_recall_serve::decode_request` (target name frozen: renaming it discards the accumulated corpus) | **live** |
| `shell_read_bash` | `rift_agent::shell_read::recognise` against `bash -n` (skips where no `bash` runs) | **live** |

Every live target runs a 60-second smoke in PR CI (the required `fuzz-smoke`
job in `.github/workflows/ci.yml`) and a 10-minute daily deep run
(`fuzz-nightly.yml`, crash artifacts uploaded). Each fuzzed parser also carries
a `never_panics`-style proptest in its own crate, gating the same contract in
the normal `cargo test` job (no nightly required) — the fuzzer adds
coverage-guided depth the proptest's random sampling cannot reach.

Seed corpora live in `corpus/<target>/` and are committed (valid examples + tricky
cases). Crash artifacts (`artifacts/`, `target/`) are git-ignored.
