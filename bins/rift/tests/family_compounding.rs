//! The compounding fixture, through the shipped binary: savings shared across
//! a repository family.
//!
//! The owner's standing requirement is that Rift's savings are **shared**:
//! *"ensure that the benefits of rift/recall are shared so that savings are
//! compounded; worktrees are often used by subagents and this adds to the token
//! savings."* The family-scope decision turns that into six assertions over
//! one family, and this file is those six assertions — one `#[test]` each,
//! named for the claim it makes, driven through the real `rift` process
//! against a real git family.
//!
//! # The fixture
//!
//! ```text
//!   C0 seed ──► C1 (notes/one) ──► C2 (notes/two)      <- main, the CAPTURING member
//!                    └─► C3 (notes/apart)              <- `divergent`, a real fork
//!                                     └─────────────── <- `ancestry`, cut from the tip
//! ```
//!
//! Only the main work tree ever runs a session: two of them, each reading one
//! file and each condensed by a real `post-commit`, so the family holds exactly
//! two checkpoints — `k1` anchored at `C1` (the `alpha.rs` read) and `k2`
//! anchored at `C2` (the `beta.rs` read). The two linked members captured
//! nothing at all, which is the point: everything they are served, they are
//! served out of the family's one store.
//!
//! `divergent` is cut at `C1` and commits its own `C3`, so `k1`'s anchor is on
//! its line and `k2`'s is not — which is what makes `out_of_branch: 1` a
//! *measurement* rather than a fixture constant.
//!
//! # Determinism
//!
//! Every session journal is planted with fixed timestamps, every hook runs with
//! `RIFT_CAPTURE_SYNC=1` so nothing is handed to a detached worker that could
//! land between two assertions, and no test reads a clock. Each test builds its
//! **own** family: five of the six serve, and a serve writes an audit row, so a
//! shared fixture would make the sixth assertion depend on test order.
//!
//! The scripted-agent driver is `hooks_e2e.rs`'s: `bins/rift`'s test target
//! cannot reach the `vogon` binary (it belongs to `rift-shadow-hooks`, and
//! `CARGO_BIN_EXE_*` names only its own package's binaries), so the fake agent
//! here is the same stdin-contract hook delivery every other fixture in this
//! crate uses.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

use rift_store::CanonicalStore as _;

/// The two files the captured sessions read. Long enough that the tokens a
/// serve stands in for clearly exceed the tokens it returns — a `tokens_avoided`
/// of zero would otherwise be an artifact of a two-line fixture rather than a
/// fact about sharing.
const ALPHA: &str = "alpha.rs";
const BETA: &str = "beta.rs";

/// Repository policy, committed in the seed so every member checks it out —
/// `rift recall` reads the *asking* checkout's `.rift/`, and a member that
/// inherited no policy would refuse for the wrong reason.
const POLICY: &str = "schema_version = 1\n\
     [project]\nname = \"compounding\"\n\
     [features]\ntotal_recall = true\n";

/// A session journal the capture path accepts as live. Fixed timestamp.
fn journal(id: &str) -> String {
    format!(
        r#"{{"schema":1,"session_id":"{id}","state":"active","agent":"claude-code",
"current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,
"updated_at_ms":1783000000000,"native_id_map":{{}}}}"#
    )
    .replace('\n', "")
}

/// A file big enough to be worth avoiding.
fn source(name: &str) -> String {
    let mut body = format!("//! {name} — fixture source for the compounding family.\n");
    for i in 0..120 {
        body.push_str(&format!(
            "pub fn {name_fn}_{i}(input: u32) -> u32 {{ input.saturating_add({i}) }}\n",
            name_fn = name.trim_end_matches(".rs"),
        ));
    }
    body
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One repository family: the capturing main work tree and two linked members
/// that captured nothing.
struct Family {
    _tmp: tempfile::TempDir,
    main: PathBuf,
    /// A member on the family's own line — every checkpoint is in its scope.
    ancestry: PathBuf,
    /// A member on a fork — `k2` is off its line and must be held back.
    divergent: PathBuf,
    home: PathBuf,
}

impl Family {
    /// Every root the binary might otherwise resolve on the developer's machine.
    fn env(&self) -> Vec<(&'static str, String)> {
        let at = |sub: &str| self.home.join(sub).display().to_string();
        vec![
            ("RIFT_CACHE_DIR", at("cache")),
            ("RIFT_CONFIG_DIR", at("config")),
            ("RIFT_FAMILY_DIR", at("families")),
            ("CODEX_HOME", at(".codex")),
            ("HOME", self.home.display().to_string()),
            ("XDG_DATA_HOME", at("data")),
            ("LOCALAPPDATA", at("data")),
            ("RIFT_HEADLESS", "1".to_owned()),
            ("RIFT_NO_KEYCHAIN", "1".to_owned()),
            // The capture a verb would hand to a detached worker runs inline
            // instead: a fixture whose facts land between two assertions is a
            // race, not a measurement.
            ("RIFT_CAPTURE_SYNC", "1".to_owned()),
        ]
    }

    fn cache(&self) -> PathBuf {
        self.home.join("cache")
    }

    /// Run the shipped binary from `cwd`; `(exit, stdout, stderr)`.
    fn rift(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let out = rift_cmd(cwd)
            .args(args)
            .envs(self.env())
            .stdin(Stdio::null())
            .output()
            .expect("rift runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// `rift --json <args>`, parsed, with the exit code asserted clean.
    fn json(&self, cwd: &Path, verb: &[&str]) -> serde_json::Value {
        let mut argv = vec!["--json"];
        argv.extend_from_slice(verb);
        let (code, out, err) = self.rift(cwd, &argv);
        assert_eq!(code, 0, "rift {argv:?}: {out}{err}");
        serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("rift {argv:?} printed no JSON: {e}\n{out}"))
    }

    /// Deliver one read-shaped agent hook, as Claude Code sends it.
    fn agent_read(&self, cwd: &Path, session: &str, file: &str) {
        use std::io::Write as _;
        // Serialised, not spliced: a Windows path carries `\`, which a raw
        // JSON string would read as an escape and the hook would defer on.
        let body = serde_json::json!({
            "schema": 1,
            "session": session,
            "turn": 1,
            "payload": {
                "tool_name": "Read",
                "tool_input": { "file_path": cwd.join(file).display().to_string() },
            },
        })
        .to_string();
        let mut child = rift_cmd(cwd)
            .args(["hooks", "claude-code", "PostToolUse"])
            .envs(self.env())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success(), "a hook never fails");
    }

    /// The family's one recall projection, resolved the way the product
    /// resolves it: through the family manifest, never through a path.
    ///
    /// The manifest root override the child processes run under is injected
    /// here rather than exported, because a test that mutates the process
    /// environment mutates its neighbours' too — which is the seam
    /// `resolve_family_under` exists for.
    fn recall_db(&self) -> PathBuf {
        let paths = rift_git::discover(&self.main).expect("the fixture is a repository");
        let env = self.env();
        let lookup = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
        let rift_git::FamilyState::Minted(family) =
            rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
                .unwrap()
        else {
            panic!("the family should have been minted by the first serve");
        };
        rift_store::recall_db_under(&self.cache(), &family.recall_id)
    }

    /// Rows out of the family's recall projection.
    fn rows(&self, sql: &str) -> Vec<Vec<String>> {
        let db = self.recall_db();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            store
                .query(sql, &[])
                .await
                .unwrap()
                .iter()
                .map(|r| {
                    (0..8)
                        .map_while(|c| {
                            r.get_text(c)
                                .ok()
                                .or_else(|| r.get_i64(c).ok().map(|n| n.to_string()))
                        })
                        .collect()
                })
                .collect()
        })
    }

    /// The family's checkpoints, oldest first: `(node_id, commit_oid)`.
    fn checkpoints(&self) -> Vec<(String, String)> {
        self.rows(
            "SELECT c.node_id, a.commit_oid FROM n_checkpoint c \
             JOIN n_checkpoint_commit a ON a.node_id = c.node_id ORDER BY c.ordinal",
        )
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone()))
        .collect()
    }
}

/// Build the family of the module docs. Called once **per test**.
fn family() -> Family {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let main = root.join("main");
    let home = root.join("home");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    git_in(&main, &["init", "-q", "-b", "main"]);
    // An empty hooks dir: this suite drives `post-commit` itself, and an
    // installed hook firing on `git worktree add` would adopt a member the
    // assertions have not asked about yet.
    let nohooks = root.join("nohooks");
    std::fs::create_dir_all(&nohooks).unwrap();
    git_in(
        &main,
        &["config", "core.hooksPath", nohooks.to_str().unwrap()],
    );

    // C0: the two files a session will read, and the policy every member
    // inherits by checking it out.
    std::fs::write(main.join(ALPHA), source(ALPHA)).unwrap();
    std::fs::write(main.join(BETA), source(BETA)).unwrap();
    std::fs::create_dir_all(main.join(".rift")).unwrap();
    std::fs::write(main.join(".rift/config.toml"), POLICY).unwrap();
    std::fs::create_dir_all(main.join("notes")).unwrap();
    git_in(&main, &["add", "-A"]);
    git_in(&main, &["commit", "-q", "-m", "seed"]);

    let fixture = Family {
        _tmp: tmp,
        main: main.clone(),
        ancestry: root.join("ancestry"),
        divergent: root.join("divergent"),
        home,
    };
    let sessions = main.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();

    // Session one reads ALPHA; C1 condenses it into k1.
    std::fs::write(sessions.join("swt-one.json"), journal("swt-one")).unwrap();
    fixture.agent_read(&main, "swt-one", ALPHA);
    commit(&fixture, "notes/one.txt", "one\n", "work one");

    // The fork is cut HERE, so k1's anchor is on its line and k2's is not.
    let c1 = git_in(&main, &["rev-parse", "HEAD"]).trim().to_owned();
    git_in(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "apart",
            fixture.divergent.to_str().unwrap(),
            &c1,
        ],
    );
    std::fs::write(fixture.divergent.join("notes/apart.txt"), "apart\n").unwrap();
    git_in(&fixture.divergent, &["add", "-A"]);
    git_in(&fixture.divergent, &["commit", "-q", "-m", "a fork"]);

    // Session two reads BETA; C2 condenses it into k2.
    std::fs::write(sessions.join("swt-two.json"), journal("swt-two")).unwrap();
    fixture.agent_read(&main, "swt-two", BETA);
    commit(&fixture, "notes/two.txt", "two\n", "work two");

    // The member on the family's own line, cut from the tip.
    git_in(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "line",
            fixture.ancestry.to_str().unwrap(),
            "main",
        ],
    );
    fixture
}

/// Commit `rel` in the main work tree and archive the live session against it,
/// through the shipped binary's own `post-commit` handler.
fn commit(f: &Family, rel: &str, body: &str, message: &str) {
    std::fs::write(f.main.join(rel), body).unwrap();
    git_in(&f.main, &["add", "--", rel]);
    git_in(&f.main, &["commit", "-q", "-m", message]);
    let git_dir = f.main.join(".git");
    let (code, out, err) = f.rift(
        &f.main,
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            git_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "a hook never fails git: {out}{err}");
}

/// Bind a summary to `path`'s captured revision, as a session in the main work
/// tree would have.
///
/// **Seeded, and that is a recorded deviation.** `summary_binding`'s one writer
/// is `rift_recall_ledger::validity_store::put_summary`, and no shipped path
/// calls it: the validity ingest that will is not built, and `known_state` passes
/// `None` for every summary today. So the half of the compounding claim this
/// file can prove is the SERVING half — that a binding written once in the family's
/// store is served to a member that did not write it, on the same bytes, and
/// is not recomputed — and the binding itself is planted at the seam the ingest
/// will use, against the hash the projection actually holds.
fn bind_summary(f: &Family, path: &str) -> String {
    let revisions = f.rows(&format!(
        "SELECT content_sha256 FROM n_file_revision WHERE path = '{path}'"
    ));
    assert_eq!(
        revisions.len(),
        1,
        "the capture recorded exactly one revision of {path}: {revisions:?}"
    );
    let sha = revisions[0][0].clone();
    let db = f.recall_db();
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        store
            .insert_or_ignore(
                "summary_binding",
                &[
                    "summary_id",
                    "path",
                    "source_sha256",
                    "bound_blob_oid",
                    "extractor_ver",
                    "policy_ver",
                    "redaction_ver",
                    "digest_ref",
                    "created_at",
                ],
                &[
                    rift_store::Value::text(format!("summary-{sha}")),
                    rift_store::Value::text(path),
                    rift_store::Value::text(&sha),
                    rift_store::Value::text(&sha),
                    rift_store::Value::Int(1),
                    rift_store::Value::Int(1),
                    rift_store::Value::Int(1),
                    rift_store::Value::text(format!("digest-{sha}")),
                    rift_store::Value::Int(0),
                ],
            )
            .await
            .unwrap();
    });
    sha
}

/// The manifest verb, with the arguments the ranked surface requires.
fn known_state<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut argv = vec![
        "recall",
        "known-state",
        "--task",
        "review the parser change",
        "--file",
        ALPHA,
    ];
    argv.extend_from_slice(extra);
    argv
}

/// Every `recall.db` under the cache root, so "one store" is counted rather
/// than asserted about one path.
fn recall_dbs(cache: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![cache.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "recall.db") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

// --- the six compounding assertions -----------------------------------------

/// (1) One `recall.db` for the family.
///
/// Three checkouts serve; one store answers. The calibration arm is the rule
/// the family model retired: the per-path derivation names three different files, and
/// two of them must not exist — which is the defect this program was opened
/// for, stated as an absence.
#[test]
fn one_store_answers_every_member_of_the_family() {
    let f = family();
    for cwd in [&f.main, &f.ancestry, &f.divergent] {
        let doc = f.json(cwd, &known_state(&[]));
        assert_eq!(doc["verb"], serde_json::json!("known-state"), "{doc}");
    }

    let dbs = recall_dbs(&f.cache());
    assert_eq!(
        dbs,
        vec![f.recall_db()],
        "three members served and the family holds exactly one recall projection"
    );
    let per_path: Vec<PathBuf> = [&f.main, &f.ancestry, &f.divergent]
        .iter()
        .map(|p| rift_store::recall_db_for_repo(p).unwrap())
        .collect();
    assert_eq!(
        per_path
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "the retired rule really does name three different files"
    );
    for member in [&per_path[1], &per_path[2]] {
        assert!(
            !member.exists(),
            "a member minted a store of its own at {}",
            member.display()
        );
    }
}

/// (2) A summary computed in one member is *served* in another, on the same
/// bytes, counted as avoided tokens, and not recomputed.
#[test]
fn a_member_is_served_the_summary_its_sibling_bound() {
    let f = family();
    // The serve that ingests, so the capture's own hash is there to bind to.
    f.json(&f.main, &known_state(&[]));
    let sha = bind_summary(&f, ALPHA);
    let before = f.rows("SELECT summary_id FROM summary_binding").len();
    assert_eq!(before, 1);

    let doc = f.json(&f.ancestry, &["recall", "read-summary", ALPHA]);
    let result = &doc["result"]["result"];
    assert_eq!(
        result["state"],
        serde_json::json!("summary"),
        "the member is served the binding it never wrote: {doc}"
    );
    assert_eq!(
        result["source_hash"],
        serde_json::json!(sha),
        "and on the SAME bytes the capturing member bound"
    );
    assert!(
        doc["estimated_tokens_avoided"].as_u64().unwrap() > 0,
        "a served summary stands in for the file it summarises: {doc}"
    );
    assert_eq!(
        f.rows("SELECT summary_id FROM summary_binding").len(),
        before,
        "nothing was recomputed: the family's one binding answered"
    );
}

/// (3) A member's `diff-since <a sibling's checkpoint>` lists exactly what the
/// sibling did after it.
#[test]
fn a_member_reads_the_delta_its_sibling_left_behind() {
    let f = family();
    f.json(&f.main, &known_state(&[]));
    let checkpoints = f.checkpoints();
    assert_eq!(checkpoints.len(), 2, "the family holds k1 and k2");
    let k1 = &checkpoints[0].0;

    let doc = f.json(&f.ancestry, &["memory", "diff-since", k1]);
    let changed: Vec<&str> = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["path"].as_str())
        .collect();
    let unchanged: Vec<&str> = doc["unchanged"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert_eq!(
        changed,
        vec![BETA],
        "exactly what the sibling read after k1, and nothing else: {doc}"
    );
    assert_eq!(
        unchanged,
        vec![ALPHA],
        "and what k1 itself already held is not offered again: {doc}"
    );
}

/// (4) The divergent member's `known-state` holds back the checkpoint that is
/// off its line, and says how many.
#[test]
fn the_forked_member_is_not_handed_the_other_line() {
    let f = family();
    let doc = f.json(&f.divergent, &known_state(&[]));
    let paths: Vec<&str> = doc["result"]["envelopes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert_eq!(
        paths,
        vec![ALPHA],
        "only the checkpoint anchored on this member's own line is served: {doc}"
    );
    let containment = &doc["result"]["containment"];
    assert_eq!(
        containment["out_of_branch"],
        serde_json::json!(1),
        "and the one held back is counted, not silently dropped: {doc}"
    );
    assert_eq!(containment["unknown_head"], serde_json::json!(false));
    assert_eq!(containment["cross_branch_grant"], serde_json::json!(false));
}

/// (5) With an explicit grant it is served, and the person's reason is audited.
#[test]
fn a_granted_serve_crosses_the_branch_and_says_who_asked() {
    let f = family();
    let doc = f.json(
        &f.divergent,
        &known_state(&["--cross-branch", "review the other line"]),
    );
    let paths: Vec<&str> = doc["result"]["envelopes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert!(
        paths.contains(&ALPHA) && paths.contains(&BETA),
        "the grant widened containment: {doc}"
    );
    let containment = &doc["result"]["containment"];
    assert_eq!(containment["cross_branch_grant"], serde_json::json!(true));
    assert_eq!(containment["out_of_branch"], serde_json::json!(0));

    // The two halves land where their contracts allow: the fixed neutral tag in
    // the frozen audit column, the person's words in the additive relation.
    let grants = f.rows(
        "SELECT g.kind, g.reason_text, e.reason FROM serve_grant_event g \
         JOIN memd_audit_event e ON e.audit_id = g.audit_id",
    );
    assert_eq!(grants.len(), 1, "one grant, keyed to the serve it widened");
    assert_eq!(grants[0][0], "branches");
    assert_eq!(grants[0][1], "review the other line");
    assert_eq!(
        grants[0][2], "cross-branch-grant",
        "the frozen column carries the tag and never the words"
    );
}

/// (6) The family's avoided-token total is the sum over its members, and the
/// per-member breakdown adds back up to it.
///
/// This is the owner's requirement as arithmetic: three checkouts serve, the
/// figures land in one ledger, and the family total is what they add up to —
/// not the largest of them, and not one member's slice presented as the whole.
#[test]
fn the_family_total_is_the_sum_of_what_its_members_avoided() {
    let f = family();
    let mut by_member: Vec<(&str, u64)> = Vec::new();
    for (name, cwd) in [
        ("main", &f.main),
        ("ancestry", &f.ancestry),
        ("divergent", &f.divergent),
    ] {
        let doc = f.json(cwd, &known_state(&[]));
        by_member.push((name, doc["estimated_tokens_avoided"].as_u64().unwrap()));
    }
    assert!(
        by_member.iter().all(|(_, avoided)| *avoided > 0),
        "every member — including the two that captured nothing — was served \
         something worth having: {by_member:?}"
    );

    let ledger = f.rows("SELECT SUM(tokens_avoided) FROM memd_audit_event");
    let family_total: u64 = ledger[0][0].parse().unwrap();
    let summed: u64 = by_member.iter().map(|(_, avoided)| *avoided).sum();
    assert_eq!(
        family_total, summed,
        "the family's ledger is exactly its members' serves added up: {by_member:?}"
    );
    assert!(
        by_member.iter().all(|(_, avoided)| *avoided < family_total),
        "and it is a SUM, not the biggest member's own figure: {by_member:?}"
    );
}
