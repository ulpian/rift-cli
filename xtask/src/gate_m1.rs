//! `xtask gate-m1` — the scripted M1 gate (PRD-M1 "Testing Decisions").
//!
//! Drives the **real** `rift` binary in a scratch repo end-to-end:
//! init (skip-audited) → enable → a two-turn agent session with a human edit
//! between turns → commit → trailers + normalized rows + turn-boundary
//! attribution asserted → a checkpoint shard on `rift/checkpoints/v1` →
//! `rift rebuild` (idempotent, delete-and-rebuild) → `rift verify` green →
//! 1-byte shard corruption → verify red → brand + auth-verb sweep → shadow-ref
//! push refusal.
//!
//! Run `cargo run -p xtask -- gate-m1` locally (it builds `rift` first). The
//! dogfood half of the gate is running the same binary on this repository with
//! capture enabled from gate day onward (PRD-M1 "Further Notes") — that is an
//! operational step, not a script.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One gate step outcome.
fn violation(v: &mut Vec<String>, ok: bool, what: &str) {
    if !ok {
        v.push(what.to_owned());
    }
}

/// The scratch environment: a temp repo + hermetic Rift dirs.
struct Scratch {
    _keep: tempfile::TempDir,
    repo: PathBuf,
    cache: PathBuf,
    config: PathBuf,
    rift: PathBuf,
}

impl Scratch {
    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("RIFT_CACHE_DIR".into(), self.cache.display().to_string()),
            ("RIFT_CONFIG_DIR".into(), self.config.display().to_string()),
            ("RIFT_NO_PROVISION".into(), "1".into()),
            ("RIFT_HEADLESS".into(), "1".into()),
            ("GIT_AUTHOR_NAME".into(), "Gate".into()),
            ("GIT_AUTHOR_EMAIL".into(), "gate@rift.local".into()),
            ("GIT_COMMITTER_NAME".into(), "Gate".into()),
            ("GIT_COMMITTER_EMAIL".into(), "gate@rift.local".into()),
            ("GIT_AUTHOR_DATE".into(), "2026-01-01T00:00:00Z".into()),
            ("GIT_COMMITTER_DATE".into(), "2026-01-01T00:00:00Z".into()),
        ]
    }

    /// Run `rift <args>` in the scratch repo; return (exit_code, stdout).
    fn rift(&self, args: &[&str], stdin: Option<&str>) -> (i32, String) {
        run_cmd(&self.rift, args, &self.repo, &self.env(), stdin)
    }

    /// Run `git <args>` in the scratch repo; return (exit_code, stdout).
    fn git(&self, args: &[&str]) -> (i32, String) {
        run_cmd(Path::new("git"), args, &self.repo, &self.env(), None)
    }

    /// Send one agent hook (stdin envelope) — always exits 0 by contract.
    fn hook(&self, verb: &str, session: &str, turn: u32, payload: &str) -> i32 {
        let body = format!(r#"{{"session":"{session}","turn":{turn},"payload":{payload}}}"#);
        self.rift(&["hooks", "claude-code", verb], Some(&body)).0
    }

    fn projection_db(&self) -> Option<PathBuf> {
        // <cache>/rift/<repo_id>/projection.db — repo_id is content-addressed;
        // find it rather than re-derive it.
        find_file(&self.cache, "projection.db")
    }
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|f| f == name) {
                return Some(p);
            }
        }
    }
    None
}

fn run_cmd(
    program: &Path,
    args: &[&str],
    cwd: &Path,
    env: &[(String, String)],
    stdin: Option<&str>,
) -> (i32, String) {
    use std::io::Write as _;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let Ok(mut child) = cmd.spawn() else {
        return (-1, String::new());
    };
    if let (Some(s), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(s.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        return (-1, String::new());
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

/// COUNT(*) of `table` in the scratch projection — read through the sanctioned
/// store seam (`rift-store` is the only crate that may hold a storage backend).
fn count(db: &Path, table: &str) -> i64 {
    scalar(db, &format!("SELECT COUNT(*) FROM {table}"))
}

fn scalar(db: &Path, sql: &str) -> i64 {
    use rift_store::CanonicalStore as _;
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().build() else {
        return -1;
    };
    let Ok(store) = rift_store::open_capture_projection(db) else {
        return -1;
    };
    rt.block_on(store.query_opt_i64(sql, &[]))
        .ok()
        .flatten()
        .unwrap_or(-1)
}

/// Build the `rift` binary and return its path.
fn build_rift(root: &Path) -> Result<PathBuf, String> {
    let status = Command::new("cargo")
        .args(["build", "-p", "rift"])
        .current_dir(root)
        .status()
        .map_err(|e| format!("cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build -p rift failed".into());
    }
    let target =
        std::env::var("CARGO_TARGET_DIR").map_or_else(|_| root.join("target"), PathBuf::from);
    let bin = target
        .join("debug")
        .join(if cfg!(windows) { "rift.exe" } else { "rift" });
    if bin.exists() {
        Ok(bin)
    } else {
        Err(format!("built binary not found at {}", bin.display()))
    }
}

/// The fixture checkpoint shard the bridge/verify legs run against. Returns
/// `(checkpoint_id, merkle_root_hex, leaves)` — root recomputed from leaves so
/// `rift verify`'s Merkle pass is exercised for real.
fn fixture_shard() -> (String, String, Vec<String>) {
    let leaves = vec![
        rift_hashchain::leaf_hash(b"gate-m1 leaf 0").to_hex(),
        rift_hashchain::leaf_hash(b"gate-m1 leaf 1").to_hex(),
    ];
    let ids: Vec<_> = leaves
        .iter()
        .filter_map(|h| rift_core::Sha256Id::parse_hex(h).ok())
        .collect();
    let root = rift_hashchain::merkle_root(&ids).to_hex();
    ("ab".repeat(32), root, leaves)
}

fn full_jsonl() -> String {
    let sha = "e".repeat(64);
    [
        format!(r#"{{"session":"bridge-native","turn":0,"event":0,"kind":"session_start","verb":"SessionStart","agent":"claude-code","occurred_at_ms":10,"payload_sha256":"{sha}","payload":{{"model":"claude-sonnet-4-5"}}}}"#),
        format!(r#"{{"session":"bridge-native","turn":1,"event":0,"kind":"tool_call","verb":"PreToolUse","agent":"claude-code","occurred_at_ms":11,"payload_sha256":"{sha}","payload":{{"tool_name":"Bash"}}}}"#),
        format!(r#"{{"session":"bridge-native","turn":1,"event":0,"kind":"turn_end","verb":"Stop","agent":"claude-code","occurred_at_ms":12,"payload_sha256":"{sha}","payload":{{}}}}"#),
        format!(r#"{{"session":"bridge-native","turn":2,"event":0,"kind":"session_end","verb":"SessionEnd","agent":"claude-code","occurred_at_ms":13,"payload_sha256":"{sha}","payload":{{}}}}"#),
    ]
    .join("\n")
}

/// Write the fixture shard onto the orphan checkpoint branch (gix_walker prior
/// art: orphan checkout → clean → write → commit → back), or a corrupted
/// variant when `corrupt` is set.
fn write_checkpoint_branch(s: &Scratch, corrupt: bool) -> Result<(), String> {
    let (id, root, leaves) = fixture_shard();
    let (d1, d2) = (&id[..2], &id[2..]);
    let branch_exists = s.git(&["rev-parse", "--verify", "rift/checkpoints/v1"]).0 == 0;
    let checkout = if branch_exists {
        s.git(&["checkout", "rift/checkpoints/v1"])
    } else {
        s.git(&["checkout", "--orphan", "rift/checkpoints/v1"])
    };
    if checkout.0 != 0 {
        return Err("checkout of the checkpoint branch failed".into());
    }
    if !branch_exists {
        let _ = s.git(&["rm", "-rf", "--quiet", "."]);
    }
    let shard = s.repo.join(d1).join(d2);
    std::fs::create_dir_all(&shard).map_err(|e| e.to_string())?;
    let report = format!(
        r#"{{"total_spans":1,"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
        "c".repeat(64)
    );
    let mut leaf_text = leaves.join("\n");
    if corrupt {
        // The 1-byte corruption: flip the first hex digit of the first leaf
        // (stays valid hex, so the failure is TAMPER, not STRUCTURE).
        let first = leaf_text.get(..1).unwrap_or("a").to_owned();
        let flipped = if first == "a" { "b" } else { "a" };
        leaf_text = leaf_text.replacen(&first, flipped, 1);
    }
    let files = [
        (
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{root}","sessions":["bridge-native"]}}"#
            ),
        ),
        ("redaction_report.json", report),
        ("full.jsonl", full_jsonl()),
        ("merkle_leaf.txt", leaf_text),
        ("merkle_root.txt", root.clone()),
    ];
    for (name, content) in files {
        std::fs::write(shard.join(name), content).map_err(|e| e.to_string())?;
    }
    if s.git(&["add", "-A"]).0 != 0 {
        return Err("git add on the checkpoint branch failed".into());
    }
    let msg = if corrupt { "corrupt shard" } else { "shard" };
    if s.git(&["commit", "-m", msg, "--allow-empty"]).0 != 0 {
        return Err("git commit on the checkpoint branch failed".into());
    }
    if s.git(&["checkout", "main"]).0 != 0 {
        return Err("checkout back to main failed".into());
    }
    Ok(())
}

/// Run the whole gate. Returns the violation list (empty ⇒ gate green).
///
/// # Errors
/// A hard `Err` only for environment problems (no git, build failure) — a
/// failed *assertion* is a violation, not an error.
#[allow(clippy::too_many_lines)] // one linear scripted gate; splitting would obscure the story
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = Vec::new();
    let rift = build_rift(root)?;

    let keep = tempfile::tempdir().map_err(|e| e.to_string())?;
    let s = Scratch {
        repo: keep.path().join("repo"),
        cache: keep.path().join("cache"),
        config: keep.path().join("config"),
        rift,
        _keep: keep,
    };
    std::fs::create_dir_all(&s.repo).map_err(|e| e.to_string())?;
    if s.git(&["init", "-b", "main", "-q"]).0 != 0 {
        return Err("git init failed (git required for the gate)".into());
    }
    std::fs::write(s.repo.join("README.md"), "gate\n").map_err(|e| e.to_string())?;
    s.git(&["add", "-A"]);
    s.git(&["commit", "-q", "-m", "init"]);

    // --- init / enable (W0: skip is audited; ADR-0001 both paths) -----------
    let (code, out) = s.rift(&["init", "--no-provision"], None);
    violation(&mut v, code == 0, "init --no-provision must exit 0");
    violation(
        &mut v,
        out.contains("skip recorded"),
        "init must record + report the provisioning skip",
    );
    let (code, _) = s.rift(&["enable", "--agent", "claude-code"], None);
    violation(&mut v, code == 0, "enable claude-code must exit 0");
    let (code, _) = s.rift(&["enable", "--agent", "not-an-agent"], None);
    violation(
        &mut v,
        code == 4,
        "unknown agent must exit 4 (precondition)",
    );

    // --- the two-turn session with a human edit between turns ---------------
    let sess = "gate-session";
    s.hook("SessionStart", sess, 0, r#"{"model":"claude-sonnet-4-5"}"#);
    s.hook("UserPromptSubmit", sess, 1, "{}");
    // agent writes within turn 1:
    std::fs::write(
        s.repo.join("feature.txt"),
        "agent line one\nagent line two\n",
    )
    .map_err(|e| e.to_string())?;
    s.hook(
        "Stop",
        sess,
        1,
        r#"{"model":"claude-sonnet-4-5","usage":{"input_tokens":1000,"output_tokens":500}}"#,
    );
    // the human edits BETWEEN turns:
    std::fs::write(
        s.repo.join("feature.txt"),
        "agent line one\nagent line two\nhuman line\n",
    )
    .map_err(|e| e.to_string())?;
    s.hook("UserPromptSubmit", sess, 2, "{}");
    s.hook(
        "Stop",
        sess,
        2,
        r#"{"model":"claude-sonnet-4-5","usage":{"input_tokens":400,"output_tokens":100}}"#,
    );

    // --- commit: trailers injected, C-diff + attribution captured -----------
    s.git(&["add", "-A"]);
    let (code, _) = s.git(&["commit", "-q", "-m", "feature work"]);
    violation(
        &mut v,
        code == 0,
        "git commit must succeed (capture never blocks)",
    );
    let (_, message) = s.git(&["log", "-1", "--format=%B"]);
    violation(
        &mut v,
        message.contains("Rift-Checkpoint:"),
        "commit message must carry the Rift-Checkpoint trailer",
    );
    violation(
        &mut v,
        message.contains("Rift-Attribution: agent="),
        "commit message must carry the Rift-Attribution trailer",
    );
    let agent_pct: i64 = message
        .lines()
        .find_map(|l| l.strip_prefix("Rift-Attribution: agent="))
        .and_then(|r| r.split('%').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(-1);
    violation(
        &mut v,
        agent_pct > 0 && agent_pct < 100,
        "attribution must resolve BOTH agent and human lines (0 < agent% < 100)",
    );

    // --- normalized rows populated (the ledger is not hollow) ---------------
    let Some(db) = s.projection_db() else {
        return Err("projection.db not found under the scratch cache".into());
    };
    for (table, min) in [
        ("session", 1),
        ("session_event", 1),
        ("turn", 2),
        ("model_call", 2),
        ("token_usage", 2),
        ("cost_fact", 2),
        ("turn_boundary_snapshot", 4),
        ("turn_diff_hunk", 1),
        ("file_revision", 1),
        ("changed_hunk", 1),
        ("substrate_install", 1), // the audited skip
    ] {
        violation(
            &mut v,
            count(&db, table) >= min,
            &format!("normalized table {table} must have >= {min} row(s)"),
        );
    }
    violation(
        &mut v,
        scalar(&db, "SELECT COUNT(DISTINCT edit_kind) FROM turn_diff_hunk") == 2,
        "turn_diff_hunk must carry both agent (A_i) and human (H_i) edits",
    );

    // --- checkpoint shard → rebuild (idempotent, delete-and-rebuild) --------
    write_checkpoint_branch(&s, false)?;
    let (code, out) = s.rift(&["--json", "rebuild"], None);
    violation(&mut v, code == 0, "rebuild must exit 0");
    let inserted = |o: &str| {
        o.lines()
            .filter_map(|l| l.trim().strip_prefix("\"events_inserted\":"))
            .find_map(|r| r.trim_end_matches(',').trim().parse::<i64>().ok())
            .unwrap_or(-1)
    };
    violation(
        &mut v,
        inserted(&out) > 0,
        "first rebuild must insert events",
    );
    let (_, out2) = s.rift(&["--json", "rebuild"], None);
    violation(
        &mut v,
        inserted(&out2) == 0,
        "re-running rebuild must insert ZERO rows (idempotent)",
    );
    let before_delete = count(&db, "captured_event");
    std::fs::remove_file(&db).map_err(|e| e.to_string())?;
    let (code, _) = s.rift(&["--json", "rebuild"], None);
    violation(
        &mut v,
        code == 0,
        "rebuild after projection delete must succeed",
    );
    violation(
        &mut v,
        count(&db, "captured_event") > 0 && before_delete > 0,
        "the projection must be rebuildable from git alone",
    );

    // --- verify: green, then red after a 1-byte shard corruption ------------
    let (code, out) = s.rift(&["verify"], None);
    violation(
        &mut v,
        code == 0,
        &format!(
            "verify must be green on an intact branch (got {code}: {})",
            out.trim()
        ),
    );
    write_checkpoint_branch(&s, true)?;
    let (code, _) = s.rift(&["verify"], None);
    violation(
        &mut v,
        code == 1,
        &format!("verify must exit 1 (tamper) after a 1-byte leaf corruption (got {code})"),
    );

    // --- brand + auth-verb sweep (user-facing surface only) -----------------
    for args in [&["--help"][..], &["doctor"], &["status"]] {
        let (_, out) = s.rift(args, None);
        violation(
            &mut v,
            !out.to_ascii_lowercase().contains("entire"),
            &format!("`rift {}` output must not name the backend", args.join(" ")),
        );
    }
    for verb in ["auth", "login", "cloud", "sync"] {
        let (code, _) = s.rift(&[verb], None);
        violation(
            &mut v,
            code == 2,
            &format!("`rift {verb}` must be refused as unknown usage (exit 2)"),
        );
    }

    // --- shadow refs never leave the machine ---------------------------------
    let bare = s
        .repo
        .parent()
        .map(|p| p.join("origin.git"))
        .unwrap_or_default();
    run_cmd(
        Path::new("git"),
        &["init", "--bare", "-q", bare.to_str().unwrap_or_default()],
        s.repo.parent().unwrap_or(&s.repo),
        &s.env(),
        None,
    );
    s.git(&["remote", "add", "origin", bare.to_str().unwrap_or_default()]);
    s.git(&["branch", "rift/secret-shadow"]);
    let (code, out) = s.git(&["push", "origin", "rift/secret-shadow"]);
    violation(
        &mut v,
        code != 0 && out.contains("refusing to push internal rift/* refs"),
        "pre-push must refuse rift/* refs",
    );
    let (code, _) = s.git(&["push", "origin", "main"]);
    violation(&mut v, code == 0, "pushing a normal branch must still work");

    Ok(v)
}
