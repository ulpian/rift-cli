//! `vogon` — the deterministic scripted fake agent.
//!
//! Rift's capture surface is `rift hooks <agent> <verb>`: a short-lived process
//! reading one JSON object on stdin. Everything downstream — turn ordinals, the
//! session state machine, attribution windows, condensation — is a function of
//! the *sequence* of those processes. Until this binary existed, the only such
//! sequence any **merge-blocking** test drove was Claude Code's five —
//! `SessionStart` / `UserPromptSubmit` / `PostToolUse` / `Stop` / `SessionEnd`
//! (the `bins/rift/tests/hooks_*_e2e.rs` suites) — so a fan-out or a lifecycle transition
//! only the other four reach was asserted in-process or not at all.
//!
//! The claim is scoped to CI deliberately, because a first draft of this file
//! overstated it. `PreToolUse` **is** spawned as a process elsewhere:
//! `benches/native-baseline/src/scenario.rs:46,93` sends it for the `ToolCallBash`
//! and `ToolCallRead` scenarios. That harness is its own workspace
//! (`benches/native-baseline/Cargo.toml:14`), is not a root member, and is named
//! nowhere in `.github/workflows/ci.yml` — measured, all three — so it can
//! never redden a merge, which is the property that matters here.
//! `SubagentStart`, `SubagentStop` and `PreCompact` are the genuinely
//! first-ever case: outside the adapters, the sweep and this file, the whole of
//! `benches`, `bins` and `fuzz` mentions them once, in a comment
//! (`bins/rift/src/hooks.rs:486`).
//!
//! Vogon plays a whole session — session → turns → tools → subagent →
//! compaction → commit — deterministically: no clock, no RNG, no environment
//! read into a payload. The same flags always produce the same bytes, which is
//! what lets the benchmark harness (`benches/native-baseline`, the condense
//! scenario) replay `session_turns` as a workload and lets the e2e assert an exact
//! receipt set.
//!
//! # The script cannot drift from the adapter
//!
//! Every mode validates the script through
//! [`rift_agent::conformance::check_stream`] against the **registered**
//! adapter — `AdapterRegistry::with_builtin()`, the same registry the shipped
//! binary dispatches through — before emitting or driving a single byte. A verb
//! this file names that the adapter stops declaring, or a fan-out whose shape
//! moves, exits `2` here instead of quietly scripting fiction.
//!
//! # Argv contract (stable; consumed by `benches/native-baseline`)
//!
//! ```text
//! vogon script --agent <token> --turns <n> [--session <id>]
//!              [--subagent-turn <n>] [--compact-turn <n>] [--repo <dir>]
//!     → stdout: one JSON object per line, in fire order. The object holds
//!       `verb` and `payload`; `serde_json` sorts keys, so the bytes read
//!       `{"payload":…,"verb":…}` — a consumer must parse rather than pattern
//!       match on the leading key.
//!       Pure: reads nothing, writes nothing but stdout.
//!
//! vogon --version | -V | version
//!     → stdout: one JSON object identifying this driver. See [`version`] for
//!       what the two digests do and do not distinguish.
//!
//! vogon drive  --rift <path> --repo <dir> [--agent <token>] [--turns <n>]
//!              [--session <id>] [--subagent-turn <n>] [--compact-turn <n>]
//!              [--commit]
//!     → spawns `<rift> hooks <agent> <verb>` per line, cwd = <repo>, payload on
//!       stdin; with --commit, a real `git commit` runs after the last turn and
//!       before `SessionEnd`, so the post-commit condense sees a live session.
//!       → stdout: one JSON summary object.
//! ```
//!
//! Defaults: `--agent claude-code`, `--turns 2`, `--session vogon-session`,
//! `--subagent-turn 2`, `--compact-turn 1`. `0` disables the subagent/compaction
//! beat. Unknown flags are refused rather than ignored — a typo in a benchmark
//! invocation must not silently change the workload.
//!
//! Exit codes: `0` ok · `1` usage · `2` the script is not conformant against the
//! registered adapter · `3` a driven hook did not exit 0 (a hook must never fail
//! its agent, always) · `4` a git step failed. **`--version` adds no
//! code and shadows none**: it is a leading mode handled before flag parsing and
//! it returns the ordinary success path, so a consumer's exit-code table does
//! not change. It can still answer `2`, with `2`'s existing meaning, because it
//! verifies the workload it names (see [`version`]).
//!
//! # Why `--repo` is mandatory for `drive`
//!
//! The agent-hook surface resolves its repository from the process working
//! directory and walks *up*. A driver that inherited its parent's cwd would
//! write session state into whatever repository the caller happened to sit in —
//! the exact class that put a fixture session into this repository's immutable
//! checkpoint history once already (`bins/rift/tests/common/mod.rs`). There is
//! no default and no `PATH` lookup for `--rift` either: both are named
//! explicitly or the process refuses to start.
#![forbid(unsafe_code)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rift_agent::conformance::{check_stream, Stream};
use rift_core::EventKind;
use rift_shadow_hooks::AdapterRegistry;

/// Environment that re-targets a child git/rift at a repository other than the
/// one its cwd names. Scrubbed on every spawn, for the same reason
/// `bins/rift/tests/common` scrubs it: an inherited `GIT_DIR` re-aims a child
/// regardless of how carefully the cwd was chosen.
const RETARGETING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_CEILING_DIRECTORIES",
];

/// One scripted hook: the native verb, the agent-native payload, and the
/// canonical kinds the adapter must produce for it.
#[derive(Debug)]
struct Step {
    verb: &'static str,
    payload: serde_json::Value,
    kinds: &'static [EventKind],
}

/// The parsed invocation.
#[derive(Debug)]
struct Spec {
    agent: String,
    session: String,
    turns: u32,
    subagent_turn: u32,
    compact_turn: u32,
    repo: Option<PathBuf>,
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(Fail { code, message }) => {
            let _ = writeln!(std::io::stderr(), "vogon: {message}");
            std::process::ExitCode::from(code)
        }
    }
}

/// A refusal: the process exit code and the one line printed to stderr.
#[derive(Debug)]
struct Fail {
    code: u8,
    message: String,
}

fn fail(code: u8, message: impl Into<String>) -> Fail {
    Fail {
        code,
        message: message.into(),
    }
}

fn run() -> Result<(), Fail> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (mode, rest) = argv
        .split_first()
        .ok_or_else(|| fail(1, "usage: vogon script|drive [flags]  (see --help)"))?;
    if mode == "--help" || mode == "-h" {
        let _ = writeln!(std::io::stdout(), "{}", usage());
        return Ok(());
    }
    // Handled HERE, beside `--help` and before `parse_spec`, so the
    // unknown-flag refusal below is untouched: a flag is not a mode, and
    // `vogon script --version` still exits 1.
    if mode == "--version" || mode == "-V" || mode == "version" {
        writeln!(std::io::stdout(), "{}", version()?)
            .map_err(|e| fail(1, format!("stdout: {e}")))?;
        return Ok(());
    }
    let spec = parse_spec(rest)?;
    let steps = script(&spec)?;
    conformance(&spec, &steps)?;
    match mode.as_str() {
        "script" => emit(&steps),
        "drive" => drive(&spec, rest, &steps),
        other => Err(fail(1, format!("unknown mode {other:?}; {}", usage()))),
    }
}

fn usage() -> &'static str {
    "usage: vogon script --agent <token> --turns <n> [--session <id>] \
     [--subagent-turn <n>] [--compact-turn <n>] [--repo <dir>]\n       \
     vogon drive --rift <path> --repo <dir> [script flags] [--commit]"
}

// --- argv -------------------------------------------------------------------

/// Every flag either mode accepts. A flag outside this set is a usage error:
/// silently ignoring one would let a benchmark believe it changed a workload it
/// did not change.
const FLAGS: &[&str] = &[
    "--agent",
    "--turns",
    "--session",
    "--subagent-turn",
    "--compact-turn",
    "--repo",
    "--rift",
];

fn flag<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, Fail> {
    let mut found = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == name {
            let value = it
                .next()
                .ok_or_else(|| fail(1, format!("{name} needs a value")))?;
            found = Some(value.as_str());
        }
    }
    Ok(found)
}

fn number(args: &[String], name: &str, default: u32) -> Result<u32, Fail> {
    match flag(args, name)? {
        None => Ok(default),
        Some(text) => text
            .parse::<u32>()
            .map_err(|_| fail(1, format!("{name} expects a non-negative integer"))),
    }
}

fn parse_spec(args: &[String]) -> Result<Spec, Fail> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--commit" {
            continue;
        }
        if !FLAGS.contains(&arg.as_str()) {
            return Err(fail(1, format!("unknown flag {arg:?}; {}", usage())));
        }
        let _ = it.next();
    }
    let turns = number(args, "--turns", 2)?;
    if turns == 0 {
        return Err(fail(1, "--turns must be at least 1"));
    }
    Ok(Spec {
        agent: flag(args, "--agent")?.unwrap_or("claude-code").to_owned(),
        session: flag(args, "--session")?
            .unwrap_or("vogon-session")
            .to_owned(),
        turns,
        subagent_turn: number(args, "--subagent-turn", 2)?.min(turns),
        compact_turn: number(args, "--compact-turn", 1)?.min(turns),
        repo: flag(args, "--repo")?.map(PathBuf::from),
    })
}

// --- the script -------------------------------------------------------------

/// The path a turn's tool payloads name. Absolute under `--repo` when one is
/// given (Claude Code sends absolute paths), repo-relative otherwise, and always
/// a pure function of the turn ordinal.
fn turn_file(spec: &Spec, turn: u32) -> String {
    let rel = format!("src/vogon_{turn}.rs");
    spec.repo.as_ref().map_or(rel.clone(), |repo| {
        repo.join(&rel).to_string_lossy().into_owned()
    })
}

/// Build the whole scripted session for `spec`, or refuse if the agent has no
/// profile yet.
///
/// **Claude Code only, deliberately — and the reason has changed twice, so the
/// current one is dated.** A profile is a claim about what an agent really
/// sends, so an agent joins when its verb table has been verified against the
/// agent's own published surface.
///
/// The sentence here used to scope this binary by *"the other eight adapters'
/// verb tables are exactly what the next work item is correcting"*. That item
/// was retired when five of those eight agents became unsupported-this-alpha
/// rather than defective, and the scope sentence outlived it — a justification
/// pointing at work nobody is doing.
///
/// **What is true now:** the supported set is
/// `claude-code`, `codex` and `antigravity`; Codex's and Claude Code's surfaces
/// are pinned into the tree (`crates/rift-agent-*/fixtures/`) and their tables
/// are measured against them. The refusal below stays because a *profile* is a
/// second claim on top of a verified table — the exact payload shapes a real
/// session sends, in order — and this binary has one only for Claude Code.
fn script(spec: &Spec) -> Result<Vec<Step>, Fail> {
    if spec.agent != "claude-code" {
        return Err(fail(
            1,
            format!(
                "no vogon profile for agent {:?}: only `claude-code` has a verified \
                 full-lifecycle hook surface today",
                spec.agent
            ),
        ));
    }
    let mut steps = vec![Step {
        verb: "SessionStart",
        payload: base(spec, serde_json::json!({"source": "startup"})),
        kinds: &[EventKind::SessionStart],
    }];
    for turn in 1..=spec.turns {
        let file = turn_file(spec, turn);
        steps.push(Step {
            verb: "UserPromptSubmit",
            payload: base(
                spec,
                serde_json::json!({"prompt": format!("vogon turn {turn}")}),
            ),
            kinds: &[EventKind::PromptSubmit],
        });
        steps.extend(tool(spec, "Read", &file, &[EventKind::ToolCall]));
        steps.extend(tool(
            spec,
            "Edit",
            &file,
            &[EventKind::ToolCall, EventKind::FileEdit],
        ));
        if turn == spec.subagent_turn {
            steps.push(Step {
                verb: "PreToolUse",
                payload: base(
                    spec,
                    serde_json::json!({
                        "tool_name": rift_agent_claude_code::SUBAGENT_TOOL,
                        "tool_input": {"description": "vogon subagent"},
                    }),
                ),
                kinds: &[EventKind::ToolCall],
            });
            // The pinned shape, not an invented one: this family's
            // `SubagentStartHookInput`/`SubagentStopHookInput` carry the
            // parent's session id plus a REQUIRED `agent_id`/`agent_type`, and
            // that pair is what keys the window a subagent's pointer, baseline
            // and window journal all live under. Scripting the parent's session
            // with a `-sub` suffix instead produced one window where the drive
            // should produce two, so the boundary this walk exists to exercise
            // was never crossed.
            let agent_id = format!("{}-agent", spec.session);
            steps.push(Step {
                verb: "SubagentStart",
                payload: base(
                    spec,
                    serde_json::json!({
                        "agent_id": agent_id,
                        "agent_type": "general-purpose",
                    }),
                ),
                kinds: &[EventKind::SubagentStart],
            });
            steps.push(Step {
                verb: "SubagentStop",
                payload: base(
                    spec,
                    serde_json::json!({
                        "agent_id": agent_id,
                        "agent_type": "general-purpose",
                    }),
                ),
                kinds: &[EventKind::SubagentEnd],
            });
            steps.push(Step {
                verb: "PostToolUse",
                payload: base(
                    spec,
                    serde_json::json!({
                        "tool_name": rift_agent_claude_code::SUBAGENT_TOOL,
                        "tool_response": {"ok": true},
                    }),
                ),
                kinds: &[EventKind::ToolResult],
            });
        }
        steps.push(Step {
            verb: "Stop",
            payload: base(spec, serde_json::json!({"stop_hook_active": false})),
            kinds: &[EventKind::TurnEnd],
        });
        if turn == spec.compact_turn {
            steps.push(Step {
                verb: "PreCompact",
                payload: base(spec, serde_json::json!({"trigger": "auto"})),
                kinds: &[EventKind::SessionCompaction],
            });
        }
    }
    steps.push(Step {
        verb: "SessionEnd",
        payload: base(spec, serde_json::json!({"reason": "exit"})),
        kinds: &[EventKind::SessionEnd],
    });
    Ok(steps)
}

/// The `PreToolUse` → `PostToolUse` pair for one tool. `PreToolUse` is the
/// fan-out verb (a file-mutating tool adds a `FileEdit` beside the `ToolCall`),
/// which is why it is the one that had never been driven through a process.
fn tool(spec: &Spec, name: &str, file: &str, pre_kinds: &'static [EventKind]) -> Vec<Step> {
    vec![
        Step {
            verb: "PreToolUse",
            payload: base(
                spec,
                serde_json::json!({"tool_name": name, "tool_input": {"file_path": file}}),
            ),
            kinds: pre_kinds,
        },
        Step {
            verb: "PostToolUse",
            payload: base(
                spec,
                serde_json::json!({
                    "tool_name": name,
                    "tool_input": {"file_path": file},
                    "tool_response": {"ok": true},
                }),
            ),
            kinds: &[EventKind::ToolResult],
        },
    ]
}

/// The envelope every payload carries: the agent's own session-id spelling and
/// cwd, exactly as the agent sends it. **Not** Rift's `{schema,session,turn,
/// payload}` envelope — that shape exists for tests, and a fake agent that used
/// it would never exercise `build_raw_hook`'s native fallback or the turn
/// derivation that a real agent's hook depends on.
fn base(spec: &Spec, mut extra: serde_json::Value) -> serde_json::Value {
    let mut root = serde_json::Map::new();
    root.insert(
        "session_id".to_owned(),
        serde_json::Value::String(spec.session.clone()),
    );
    if let Some(repo) = &spec.repo {
        root.insert(
            "cwd".to_owned(),
            serde_json::Value::String(repo.to_string_lossy().into_owned()),
        );
    }
    if let Some(fields) = extra.as_object_mut() {
        root.append(fields);
    }
    serde_json::Value::Object(root)
}

/// Validate the script against the **registered** adapter before it is emitted
/// or driven: every verb declared, the concatenated fan-out exact, and a second
/// play identical.
fn conformance(spec: &Spec, steps: &[Step]) -> Result<(), Fail> {
    let registry = AdapterRegistry::with_builtin();
    let agent = rift_core::AgentId::new(spec.agent.clone());
    let adapter = registry
        .get(&agent)
        .ok_or_else(|| fail(2, format!("no adapter registered for {:?}", spec.agent)))?;
    let pairs: Vec<(&str, serde_json::Value)> = steps
        .iter()
        .map(|s| (s.verb, s.payload.clone()))
        .collect::<Vec<_>>();
    let kinds: Vec<EventKind> = steps.iter().flat_map(|s| s.kinds.iter().copied()).collect();
    let stream = Stream::new("vogon scripted session", &pairs, &kinds);
    check_stream(adapter, &stream).map_err(|e| fail(2, e))
}

// --- identity ---------------------------------------------------------------

/// FNV-1a over the driver's own source text, folded at compile time.
///
/// `indexing_slicing` is allowed at this site and nowhere near it: `i` is
/// bounded by `bytes.len()` on the line above, and `<[u8]>::get` is not a
/// `const fn` on this toolchain, so a checked read is not available in a
/// `const` context.
#[allow(clippy::indexing_slicing)]
const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// A digest of this file's source text, computed while compiling it.
const SOURCE_FNV1A: u64 = fnv1a(include_str!("vogon.rs").as_bytes());

/// SHA-256 of `bytes`, rendered through `rift-core`'s own hex, so this file
/// does not become the workspace's second hex renderer.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    rift_core::Sha256Id::from_bytes(out).to_hex()
}

/// The spec whose emitted workload `script_sha256` names. Pinned: changing it
/// changes what every published version string means, so it changes here or not
/// at all.
fn reference_spec() -> Spec {
    Spec {
        agent: "claude-code".to_owned(),
        session: "vogon-session".to_owned(),
        turns: 2,
        subagent_turn: 2,
        compact_turn: 1,
        repo: None,
    }
}

/// One JSON object identifying this driver, for the provenance line of a
/// benchmark artifact.
///
/// **What the fields distinguish.** `script_sha256` is SHA-256 over the exact
/// bytes `script` emits for [`reference_spec`] — it names *the workload this
/// driver authors*, which is the thing a throughput row actually depends on,
/// and it is verified against the registered adapter before it is printed, so a
/// drifted binary answers `2` rather than publishing a digest for a workload it
/// would refuse to emit. `source_fnv1a` distinguishes builds whose source text
/// differs. `version` is `CARGO_PKG_VERSION`, and on its own it is the same
/// string for every build of this crate, which is why it does not travel alone.
///
/// **What they do NOT distinguish, stated because a consumer will otherwise
/// assume otherwise — and this sentence is an ARGUMENT, not a claim a test can
/// redden: nothing here is a BUILD fingerprint.** Two
/// builds from byte-identical source under different toolchains, profiles or
/// flags report identical strings. A consumer that needs to tell those apart
/// must keep an independent discriminator — the binary's mtime — knowingly,
/// rather than retire it believing this replaced it. Doing better needs a
/// `build.rs` on a library the whole capture plane depends on, or a
/// version-stamping dependency; both are owner decisions, not this binary's to
/// make.
fn version() -> Result<String, Fail> {
    let spec = reference_spec();
    let steps = script(&spec)?;
    conformance(&spec, &steps)?;
    let digest = sha256_hex(script_bytes(&steps).as_bytes());
    Ok(serde_json::json!({
        "schema": 1,
        "name": "vogon",
        "version": env!("CARGO_PKG_VERSION"),
        "script_sha256": digest,
        "source_fnv1a": format!("{SOURCE_FNV1A:016x}"),
    })
    .to_string())
}

// --- modes ------------------------------------------------------------------

/// The exact bytes `script` writes for `steps`.
///
/// Split out for one reason: [`version`] digests the workload this driver
/// authors, and a digest computed from a second rendering would be a claim
/// about a workload nobody emits. One renderer, two callers.
fn script_bytes(steps: &[Step]) -> String {
    let mut out = String::new();
    for step in steps {
        let line = serde_json::json!({"verb": step.verb, "payload": step.payload});
        out.push_str(&line.to_string());
        out.push('\n');
    }
    out
}

fn emit(steps: &[Step]) -> Result<(), Fail> {
    let mut out = std::io::stdout().lock();
    write!(out, "{}", script_bytes(steps)).map_err(|e| fail(1, format!("stdout: {e}")))
}

fn drive(spec: &Spec, args: &[String], steps: &[Step]) -> Result<(), Fail> {
    let rift = PathBuf::from(
        flag(args, "--rift")?
            .ok_or_else(|| fail(1, "drive needs --rift <path to the rift binary>"))?,
    );
    let repo = spec
        .repo
        .clone()
        .ok_or_else(|| fail(1, "drive needs --repo <work tree>"))?;
    if !rift.is_file() {
        return Err(fail(1, format!("--rift {} is not a file", rift.display())));
    }
    if !repo.is_dir() {
        return Err(fail(
            1,
            format!("--repo {} is not a directory", repo.display()),
        ));
    }
    // `SessionEnd` is always the last beat; the commit lands between the last
    // turn and it, so the post-commit archive still sees a live session.
    let (last, head) = steps.split_last().ok_or_else(|| fail(1, "empty script"))?;
    for step in head {
        hook(&rift, &repo, &spec.agent, step)?;
    }
    let committed = args.iter().any(|a| a == "--commit");
    if committed {
        commit(&repo, spec.turns)?;
    }
    hook(&rift, &repo, &spec.agent, last)?;
    let summary = serde_json::json!({
        "schema": 1,
        "agent": spec.agent,
        "session": spec.session,
        "turns": spec.turns,
        "hooks": steps.len(),
        "committed": committed,
    });
    writeln!(std::io::stdout(), "{summary}").map_err(|e| fail(1, format!("stdout: {e}")))
}

/// Drive one hook through the real binary, exactly as the agent would: the
/// payload on stdin, the verb in argv, the cwd pinned to `--repo`.
fn hook(rift: &Path, repo: &Path, agent: &str, step: &Step) -> Result<(), Fail> {
    let mut cmd = Command::new(rift);
    cmd.args(["hooks", agent, step.verb])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for key in RETARGETING_ENV {
        cmd.env_remove(key);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| fail(3, format!("spawn {}: {e}", rift.display())))?;
    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(step.payload.to_string().as_bytes())
            .map_err(|e| fail(3, format!("{}: stdin: {e}", step.verb)))?;
    }
    let status = child
        .wait()
        .map_err(|e| fail(3, format!("{}: wait: {e}", step.verb)))?;
    if status.success() {
        Ok(())
    } else {
        // A hook never fails its agent. A non-zero exit here
        // is a product defect, not a driver error, so it gets its own code.
        Err(fail(
            3,
            format!("hooks {agent} {} exited {:?}", step.verb, status.code()),
        ))
    }
}

/// A real commit in `repo`, so the post-commit hook's condense path runs against
/// a live session. Identity is passed per-invocation (`-c`), never written to
/// the repository's config, and the file is added by name — never `git add -A`,
/// which on this fleet resurrects paths another agent deleted.
fn commit(repo: &Path, turns: u32) -> Result<(), Fail> {
    let name = format!("vogon-{turns}.txt");
    std::fs::write(repo.join(&name), format!("vogon drove {turns} turns\n"))
        .map_err(|e| fail(4, format!("write {name}: {e}")))?;
    git(repo, &["add", "--", &name])?;
    git(
        repo,
        &[
            "-c",
            "user.name=vogon",
            "-c",
            "user.email=vogon@rift.invalid",
            "commit",
            "-q",
            "-m",
            "vogon: scripted session",
        ],
    )
}

fn git(repo: &Path, args: &[&str]) -> Result<(), Fail> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for key in RETARGETING_ENV {
        cmd.env_remove(key);
    }
    let status = cmd
        .status()
        .map_err(|e| fail(4, format!("spawn git {args:?}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(fail(4, format!("git {args:?} exited {:?}", status.code())))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn spec(turns: u32) -> Spec {
        Spec {
            agent: "claude-code".into(),
            session: "vogon-session".into(),
            turns,
            subagent_turn: 2.min(turns),
            compact_turn: 1,
            repo: None,
        }
    }

    /// The headline: the four verbs the `bins/rift/tests/hooks_*_e2e.rs` suites have never
    /// driven through a process are all in the script, and the script the
    /// binary emits is the one the registered adapter agrees with.
    #[test]
    fn the_script_covers_the_four_undriven_verbs_and_passes_conformance() {
        let s = spec(2);
        let steps = script(&s).unwrap();
        let verbs: Vec<&str> = steps.iter().map(|s| s.verb).collect();
        for verb in ["PreToolUse", "SubagentStart", "SubagentStop", "PreCompact"] {
            assert!(verbs.contains(&verb), "{verb} missing from the script");
        }
        conformance(&s, &steps).unwrap();
    }

    /// Determinism is the whole contract for a benchmark workload: the same
    /// flags produce the same bytes, with no clock and no RNG in between.
    #[test]
    fn the_same_flags_produce_byte_identical_scripts() {
        let render = |s: &Spec| {
            script(s)
                .unwrap()
                .iter()
                .map(|step| format!("{} {}", step.verb, step.payload))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(render(&spec(3)), render(&spec(3)));
        assert_ne!(render(&spec(3)), render(&spec(4)));
    }

    /// `--turns` is the benchmark's workload dial, so the relationship between
    /// it and the hook count is pinned rather than assumed.
    #[test]
    fn turns_scale_the_hook_count_linearly() {
        let count = |turns| script(&spec(turns)).unwrap().len();
        // SessionStart + SessionEnd + PreCompact, then 6 hooks per plain turn.
        assert_eq!(count(1), 3 + 6 + 4);
        assert_eq!(count(3) - count(2), 6);
        assert_eq!(count(4) - count(3), 6);
    }

    /// A payload is the AGENT's own shape, not Rift's test envelope — otherwise
    /// the driver would never exercise the native session-id fallback that
    /// every real hook depends on.
    #[test]
    fn payloads_are_agent_native_not_rifts_test_envelope() {
        let steps = script(&spec(1)).unwrap();
        for step in &steps {
            assert!(step.payload.get("payload").is_none(), "{}", step.verb);
            assert_eq!(
                step.payload.get("session_id").and_then(|v| v.as_str()),
                Some("vogon-session"),
                "{} carries no native session id",
                step.verb
            );
        }
    }

    /// An unprofiled agent refuses rather than inventing a verb table — the
    /// failure mode that would make this binary a source of fiction.
    #[test]
    fn an_unprofiled_agent_is_refused_not_guessed() {
        let mut s = spec(1);
        s.agent = "cursor".into();
        assert_eq!(script(&s).unwrap_err().code, 1);
    }

    /// Beats can be switched off, and `0` means off rather than "turn zero".
    #[test]
    fn a_zero_beat_turn_drops_that_beat() {
        let mut s = spec(2);
        s.subagent_turn = 0;
        s.compact_turn = 0;
        let steps = script(&s).unwrap();
        let verbs: Vec<&str> = steps.iter().map(|st| st.verb).collect();
        assert!(!verbs.contains(&"SubagentStart"));
        assert!(!verbs.contains(&"PreCompact"));
        conformance(&s, &steps).unwrap();
    }

    /// The published digest must be DERIVED from the script it names, not a
    /// literal that agrees with it today. A hardcoded digest is exactly the
    /// shape that goes quietly false the first time the workload moves.
    #[test]
    fn the_version_digest_is_derived_from_the_workload_it_names() {
        let reported: serde_json::Value = serde_json::from_str(&version().unwrap()).unwrap();
        let steps = script(&reference_spec()).unwrap();
        let expected = sha256_hex(script_bytes(&steps).as_bytes());
        assert_eq!(reported["script_sha256"].as_str(), Some(expected.as_str()));
        assert_eq!(
            reported["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            reported["source_fnv1a"].as_str().map(str::len),
            Some(16),
            "the source digest must be present and fixed width"
        );
        assert_ne!(SOURCE_FNV1A, 0, "the source digest folded nothing");
    }

    /// `script_bytes` is what `emit` writes, so the digest names the bytes a
    /// consumer really receives. Pinned against the rendering going through a
    /// second path.
    #[test]
    fn the_digested_bytes_are_the_emitted_bytes() {
        let steps = script(&reference_spec()).unwrap();
        let rendered = script_bytes(&steps);
        assert_eq!(rendered.lines().count(), steps.len());
        let first: serde_json::Value =
            serde_json::from_str(rendered.lines().next().unwrap()).unwrap();
        assert_eq!(first["verb"].as_str(), Some("SessionStart"));
        assert!(rendered.ends_with('\n'), "every line is terminated");
    }

    /// The reference spec is the meaning of every published version string, so
    /// a change to it is deliberate rather than incidental.
    #[test]
    fn the_reference_spec_is_pinned() {
        let s = reference_spec();
        assert_eq!(
            (
                s.agent.as_str(),
                s.session.as_str(),
                s.turns,
                s.subagent_turn,
                s.compact_turn
            ),
            ("claude-code", "vogon-session", 2, 2, 1)
        );
        assert!(
            s.repo.is_none(),
            "a repo would put a machine path in the id"
        );
    }

    /// The version arm rides the ordinary success path and takes nothing from
    /// the exit-code contract: an unknown mode is still a usage refusal.
    #[test]
    fn the_version_arm_adds_no_exit_code() {
        assert!(version().is_ok());
        let unknown = ["--warp".to_owned()];
        assert_eq!(parse_spec(&unknown).unwrap_err().code, 1);
    }

    /// Exit code 2 — the drift guard — is asserted rather than assumed. It was
    /// published in the argv contract and then never observed, because the only
    /// way to reach it is a script the registered adapter disagrees with, and
    /// the shipped script agrees with it by construction. Two of
    /// `check_stream`'s **four** refusals are driven here — an undeclared verb
    /// and mismatched kinds. The other two (a declared verb whose `map` errors,
    /// and a second play differing) are properties of the adapter rather than of
    /// this script, and each has its own negative in `rift_agent::conformance`.
    ///
    /// CORRECTION 2026-08-20: this said "three refusals" and claimed the third
    /// was "covered in `rift_agent::conformance`". The function has four arms,
    /// and that crate covered none of them *through `check_stream`* — its one
    /// negative went through `check_replay_safe`. The sentence described a test
    /// that did not exist. It does now, one per arm.
    #[test]
    fn a_script_the_registered_adapter_disagrees_with_is_refused_with_code_two() {
        let s = spec(1);
        let mut undeclared = script(&s).unwrap();
        undeclared.push(Step {
            verb: "NoSuchVerbTheAgentSends",
            payload: base(&s, serde_json::json!({})),
            kinds: &[],
        });
        let refused = conformance(&s, &undeclared).unwrap_err();
        assert_eq!(refused.code, 2, "{}", refused.message);
        assert!(
            refused.message.contains("not declared"),
            "{}",
            refused.message
        );

        let mut moved = script(&s).unwrap();
        moved[0].kinds = &[EventKind::FileEdit];
        let refused = conformance(&s, &moved).unwrap_err();
        assert_eq!(refused.code, 2, "{}", refused.message);
        assert!(refused.message.contains("expected"), "{}", refused.message);
    }

    /// An unknown flag is a refusal, not a silent no-op: a benchmark that
    /// believes it changed a workload it did not change is worse than an error.
    #[test]
    fn an_unknown_flag_is_refused() {
        let args = ["--turns".to_owned(), "2".to_owned(), "--warp".to_owned()];
        assert_eq!(parse_spec(&args).unwrap_err().code, 1);
    }
}
