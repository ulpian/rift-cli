//! End-to-end: the native archive through the real post-commit hook — the
//! shard and its four artifacts, the transcript pair, the read facts, the
//! opted-in ignored file, the trailer that names the shard that lands, and the
//! files under `.rift/` that decide nothing any more.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_read_hook, archive_head, checkpoint_ref_exists, checkpoint_shards,
    commit_carrying_trailer, doctor_json, git_in, json_str, now_ms, only_checkpoint_id,
    prepare_commit_msg_hook, rev_parse, rift_json, run_rift, run_rift_capture, run_rift_capture_in,
    run_rift_stdout, seed_live_session_repo, session_phase, shard_blob, shard_path_of,
    shard_tree_names, sync_env, sync_env_with_data, write_agent_transcript, LIVE_SESSION,
    TWO_LINE_TRANSCRIPT,
};

/// A repository whose live session remembers the agent transcript's ADDRESS,
/// archived once through the real post-commit hook, and the private roots
/// the verbs that read the shard run under.
struct ArchivedTranscript {
    repo: PathBuf,
    cache: PathBuf,
    data: PathBuf,
}

impl ArchivedTranscript {
    /// The transcript pair at the product surface: the fixture the tests of the
    /// compact transcript and of the prompt arm share.
    fn archive(tmp: &Path) -> Self {
        let repo = seed_live_session_repo(tmp);
        // The agent's transcript, where the agent keeps it (outside the repo —
        // the archive must read the ADDRESS, not assume a location).
        let transcript = write_agent_transcript(tmp, LIVE_SESSION, TWO_LINE_TRANSCRIPT);
        // The cache carries the path the way a hook payload would have left it,
        // the turn still open, touched now so the idle sweep sees it live.
        std::fs::write(
            repo.join(".git").join("rift-sessions").join("e2e-live-1.json"),
            format!(
                r#"{{"schema":1,"session_id":"e2e-live-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":{},"native_id_map":{{}},"transcript_path":{}}}"#,
                now_ms(),
                json_str(&transcript)
            ),
        )
        .unwrap();
        let cache = tmp.join("cache");
        let data = tmp.join("data");
        archive_head(&repo, &sync_env_with_data(&cache, &data));
        Self { repo, cache, data }
    }

    fn env(&self) -> [(&'static str, &str); 5] {
        sync_env_with_data(&self.cache, &self.data)
    }
}

/// The condense regression at the product surface: a commit made while a
/// native-hook session is live archives a real checkpoint shard onto
/// `refs/heads/rift/checkpoints/v1`, with its four structural artifacts, on a
/// repository that has **never been configured** — no `.rift/rewrite.toml`,
/// which is every repository, because nothing in the product ever wrote one.
///
/// Before 2026-08-10 this exact scenario produced no
/// checkpoint at all: the route table's absent-file arm answered `Wrapper`, the
/// wrapper binary was not installed, and the hook skipped with a "run
/// `rift setup`" line. That was the shipped default, and it is the defect
/// "zero checkpoints → recall empty" standing in the product for a year.
/// The route table and the wrapper were deleted on 2026-08-11; this is the
/// behaviour that outlived them, and its twin
/// `a_stale_rewrite_toml_is_named_once_and_changes_nothing` proves the file the
/// old default was read from now decides nothing.
#[test]
fn an_unconfigured_repo_archives_a_checkpoint_without_any_rewrite_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    assert!(
        !repo.join(".rift").join("rewrite.toml").exists(),
        "the point of this test is that nothing configures the archive"
    );
    let cache = tmp.path().join("cache");
    // A private substrate root too, so nothing on the developer's machine can
    // make this pass by accident.
    let data = tmp.path().join("data");

    let (code, stderr) = run_rift_capture(
        &repo,
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            repo.join(".git").to_str().unwrap(),
        ],
        Some(""),
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("XDG_DATA_HOME", data.to_str().unwrap()),
            ("LOCALAPPDATA", data.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code, 0, "hook never fails the git op; stderr: {stderr}");
    assert!(
        !stderr.contains("FAILED") && !stderr.contains("skipped"),
        "a successful archive is quiet: {stderr}"
    );

    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&refs.stdout).into_owned();
    assert!(
        listing.contains("refs/heads/rift/checkpoints/v1"),
        "an unconfigured repo archived nothing: {listing:?}"
    );
    // The shard carries the four structural artifacts of a native archive.
    let tree = Command::new("git")
        .args([
            "ls-tree",
            "-r",
            "--name-only",
            "refs/heads/rift/checkpoints/v1",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&tree.stdout).into_owned();
    for artifact in [
        "content.redacted",
        "content_hash.txt",
        "metadata.json",
        "redaction_report.json",
    ] {
        assert!(names.contains(artifact), "{artifact} in shard: {names}");
    }
}

/// A session whose cache remembers the agent transcript's ADDRESS archives the
/// transcript PAIR into its checkpoint shard — compact + full, with the
/// compact name and boundary in `metadata.json` — through the real post-commit
/// hook binary, and the verify surface accepts the shard.
#[test]
fn a_real_archive_carries_the_compact_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = ArchivedTranscript::archive(tmp.path());

    let names = shard_tree_names(&fx.repo);
    for artifact in ["transcript.compact.jsonl", "transcript.jsonl"] {
        assert!(names.contains(artifact), "{artifact} in shard: {names}");
    }
    let meta = shard_blob(&fx.repo, &shard_path_of(&fx.repo, "metadata.json"));
    assert!(
        meta.contains(r#""compact_transcript":"transcript.compact.jsonl""#),
        "metadata records the compact artifact name: {meta}"
    );
    assert!(
        meta.contains(r#""compact_transcript_start":0"#),
        "…and the first checkpoint's boundary starts at 0: {meta}"
    );
    // The worker recorded its progress boundary for the NEXT checkpoint:
    // two non-empty raw lines consumed.
    let progress = std::fs::read_to_string(
        fx.repo
            .join(".git")
            .join("rift-sessions")
            .join("e2e-live-1.transcript"),
    )
    .unwrap();
    assert!(
        progress.contains(r#""raw_lines": 2"#),
        "the sidecar carries the compactor's own raw-line count: {progress}"
    );
    // And the verify surface accepts the transcript-carrying shard.
    let (code, out) = run_rift_capture_in(&fx.repo, &["verify"], "", &fx.env());
    assert_eq!(
        code, 0,
        "verify green over a transcript-carrying shard: {out}"
    );
}

/// The prompt arm of `why` and `explain` must be ALIVE on a shard the CURRENT
/// native writer produced — canon-3 archives the transcript as
/// `transcript.jsonl`, and both verbs serve the STORED redacted prompt out of
/// it (never a re-read of the agent's live file, which this fixture keeps
/// outside the repo).
#[test]
fn why_and_explain_serve_the_stored_prompt_from_the_native_shard() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = ArchivedTranscript::archive(tmp.path());
    let checkpoint_id = only_checkpoint_id(&fx.repo);
    std::fs::create_dir_all(fx.repo.join(".rift")).unwrap();
    commit_carrying_trailer(&fx.repo, &checkpoint_id);

    let w = rift_json(&fx.repo, &["why", "b.txt:1", "--json"], &fx.env());
    assert_eq!(
        w["prompt"]["text"], "add a feature",
        "why serves the stored redacted prompt from the canon-3 shard: {w}"
    );
    assert_eq!(w["prompt"]["extracted"], true, "{w}");
    assert_eq!(
        w["prompt"]["line"], 0,
        "the intent locator's address, not a guess: {w}"
    );
    let e = rift_json(
        &fx.repo,
        &["checkpoint", "explain", &checkpoint_id, "--json"],
        &fx.env(),
    );
    assert_eq!(
        e["prompt"]["text"], "add a feature",
        "explain's prompt arm reads the same canon-3 artifact: {e}"
    );
}

/// The read-facts chain: a read-shaped PostToolUse hook captures the
/// path + content hash durably, and the next post-commit archive freezes them
/// into the shard's `tool_calls.jsonl` — the artifact the recall plane ingests
/// as `n_file_read`. Without this, memory is empty by construction.
#[test]
fn captured_read_facts_reach_the_checkpoint_shard() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    // The agent reads a repo file.
    agent_read_hook(&repo, "e2e-live-1", 1, &repo.join("a.txt"), &env);

    // Commit → the archive freezes the session's read facts into the shard.
    archive_head(&repo, &env);
    let names = shard_tree_names(&repo);
    assert!(
        names.contains("tool_calls.jsonl"),
        "the shard carries the read facts: {names}"
    );
    let jsonl = shard_blob(&repo, &shard_path_of(&repo, "tool_calls.jsonl"));
    assert!(
        jsonl.contains(r#""path":"a.txt""#) && jsonl.contains(r#""tool":"Read""#),
        "the read fact names the repo-relative path: {jsonl}"
    );
    // The recorded hash is the read-time content hash (sha256 of "one\n").
    assert!(
        jsonl.contains("2c8b08da5ce60398e1f19af0e5dccc744df274b826abe585eaba68c525434806"),
        "content_sha256 matches the bytes: {jsonl}"
    );

    // The recall plane ingests the shard: cost-hotspots shows the REAL read
    // (memory is no longer empty by construction), and a second run is a
    // watermark no-op (same answer, no error).
    for pass in 0..2 {
        let (code, text) = run_rift_stdout(&repo, &["memory", "cost-hotspots"], &env);
        assert_eq!(code, 0, "cost-hotspots pass {pass}: {text}");
        assert!(
            text.contains("a.txt"),
            "pass {pass}: the captured read is a hotspot: {text}"
        );
        assert!(
            !text.contains("no captured reads"),
            "pass {pass}: the empty-state is gone: {text}"
        );
    }
}

/// "HEAD move without commit" and "mid-session stash → pull → apply".
///
/// In the legacy backend these are one case for a structural reason: its ephemeral
/// checkpoints live on a shadow branch **named after the base commit**
/// (`legacy/<c7>-<wt6>`), so moving HEAD without committing orphans that branch
/// and it has to be renamed — the tag's own architecture note calls this out as
/// "Shadow Branch Migration". Rift's native archive writes every checkpoint to
/// one orphan ref, `NATIVE_CHECKPOINT_REF` = `refs/heads/rift/checkpoints/v1`,
/// and the modelled `shadow_branch` relation has no writer anywhere in the tree
/// (verified by search, not assumed). So there is no base-commit-named ref to
/// migrate: the migration class is structurally **absent** here, not merely
/// unimplemented, and it can only return if a future refs-based checkpoint
/// backend reintroduces per-base-commit ephemeral refs.
///
/// What remains is the guarantee underneath it, which is what this pins: the
/// session survives the move, work archived *before* HEAD moved is still on the
/// ref *after* it, and an archive after the move lands on the same ref.
#[test]
fn a_session_and_its_checkpoints_survive_a_head_move_without_a_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let base = commit_on_an_upstream_branch(&repo);
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    archive_head(&repo, &env);
    let before = checkpoint_shards(&repo);
    assert!(!before.is_empty(), "a checkpoint exists before the move");

    let moved = stash_pull_apply(&repo);
    assert_ne!(moved, base, "HEAD really moved");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "work in progress\n",
        "the stashed work came back"
    );

    // The session is untouched by the move — it is keyed by session id, never
    // by the commit it happened to start against.
    assert_eq!(session_phase(&repo, "e2e-live-1"), "active");

    // Nothing was orphaned, and the ref keeps taking archives at the new HEAD.
    //
    // Survival on its own would also be satisfied by a ref nothing can write to
    // any more — which is what the migration class looks like when it bites — so
    // the post-move archive has to be seen LANDING. That needs the session to
    // have captured something new, and the reason is a real property of the
    // native archive rather than test scaffolding: the checkpoint id folds in
    // the captured content and deliberately EXCLUDES the anchoring commit (the
    // trailer is predicted pre-commit), so a session that captured nothing new
    // re-archives the SAME shard path as an idempotent no-op. Measured, not
    // assumed: with no read in between, the second post-commit leaves the shard
    // list byte-identical.
    agent_read_hook(&repo, "e2e-live-1", 2, &repo.join("b.txt"), &env);
    archive_head(&repo, &env);
    let after = checkpoint_shards(&repo);
    for shard in &before {
        assert!(
            after.contains(shard),
            "checkpoint {shard} archived before the HEAD move is still on the ref: {after:?}"
        );
    }
    assert!(
        after.len() > before.len(),
        "the post-move archive landed on the ref: {before:?} → {after:?}"
    );
    // And it landed at the MOVED head, not the base the session started against.
    let fresh: Vec<&String> = after.iter().filter(|s| !before.contains(s)).collect();
    assert_eq!(fresh.len(), 1, "one new checkpoint: {before:?} → {after:?}");
    let meta = shard_blob(&repo, &format!("{}/metadata.json", fresh[0]));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&meta).unwrap()["commit"],
        serde_json::json!(moved),
        "the post-move checkpoint is anchored at the new HEAD"
    );
}

/// An "upstream" commit the working branch has not seen yet, on a branch of
/// its own; HEAD is back where it started. Returns the base commit.
fn commit_on_an_upstream_branch(repo: &Path) -> String {
    let base = rev_parse(repo, "HEAD");
    git_in(repo, &["checkout", "-q", "-b", "upstream"]);
    std::fs::write(repo.join("b.txt"), "upstream\n").unwrap();
    git_in(repo, &["add", "b.txt"]);
    git_in(repo, &["commit", "-q", "-m", "upstream work"]);
    git_in(repo, &["checkout", "-q", "-"]);
    assert_eq!(rev_parse(repo, "HEAD"), base);
    base
}

/// stash → pull → apply: HEAD moves onto `upstream` with no commit of ours in
/// between, and the dirty work comes back. Returns the moved HEAD.
fn stash_pull_apply(repo: &Path) -> String {
    std::fs::write(repo.join("a.txt"), "work in progress\n").unwrap();
    git_in(repo, &["stash", "--quiet"]);
    git_in(repo, &["merge", "--ff-only", "-q", "upstream"]);
    git_in(repo, &["stash", "pop", "--quiet"]);
    rev_parse(repo, "HEAD")
}

/// End to end and durably: a git-ignored file opted in through
/// `.worktreeinclude` is captured into the checkpoint, and the secret inside it
/// is masked in the bytes git actually stores.
///
/// The two halves have to be asserted together. "No secret in the shard" passes
/// trivially if nothing was captured, and "the file was captured" says nothing
/// about the gate — so this reads the real `content.redacted` blob back out of
/// the checkpoint ref and requires the record naming `.env`, the placeholder,
/// and the absence of the key, in one artifact.
#[test]
fn an_opted_in_ignored_file_reaches_the_shard_redacted() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    // The shape this control exists for: a credential file git is told to
    // ignore, so nothing else in Rift can ever see the work done in it.
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    std::fs::write(
        repo.join(".env"),
        "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\nFEATURE_FLAG=on\n",
    )
    .unwrap();
    std::fs::write(repo.join(".worktreeinclude"), "# local config\n.env\n").unwrap();

    let cache = tmp.path().join("cache");
    archive_head(&repo, &sync_env(&cache));

    let shards = checkpoint_shards(&repo);
    assert_eq!(shards.len(), 1, "one checkpoint: {shards:?}");
    let blob = git_in(
        &repo,
        &[
            "show",
            &format!(
                "refs/heads/rift/checkpoints/v1:{}/content.redacted",
                shards[0]
            ),
        ],
    );
    assert!(
        blob.contains("rift-capture/1") && blob.contains("\nf .env\n"),
        "the opted-in file is in the checkpoint, named: {blob:?}"
    );
    assert!(
        blob.contains("FEATURE_FLAG=on"),
        "its non-secret content is captured, not thrown away with the secret: \
         {blob:?}"
    );
    assert!(
        !blob.contains("AKIAIOSFODNN7EXAMPLE"),
        "and the secret never reached the bytes git stores: {blob:?}"
    );
    assert!(blob.contains("[REDACTED:vendor]"), "masked, not dropped");

    // The report beside it counts the masking, so the shard is self-describing.
    let report = git_in(
        &repo,
        &[
            "show",
            &format!(
                "refs/heads/rift/checkpoints/v1:{}/redaction_report.json",
                shards[0]
            ),
        ],
    );
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert!(
        report["total_spans"].as_u64().unwrap() >= 1,
        "the redaction report accounts for it: {report}"
    );
}

/// The commit trailer must still name the checkpoint that actually lands, now
/// that the capture it is derived from has content in it.
///
/// `prepare-commit-msg` predicts the condense id BEFORE the commit exists, and
/// `post-commit` archives at the id it derives afterwards. Both read the same
/// `session_raw_capture`, which is why they agree — but the inclusion document
/// is the first thing ever to make that capture non-empty, so a divergence
/// (different work tree, different read moment) would now produce a trailer
/// pointing at a shard that does not exist: a provenance lie, silently.
#[test]
fn the_predicted_trailer_id_is_the_shard_that_lands_with_an_include_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::write(repo.join(".env"), "FEATURE_FLAG=on\n").unwrap();
    std::fs::write(repo.join(".worktreeinclude"), ".env\n").unwrap();

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let msg = tmp.path().join("COMMIT_EDITMSG");
    std::fs::write(&msg, "do the thing\n").unwrap();
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "prepare-commit-msg",
                "--git-dir",
                gd.to_str().unwrap(),
                "--",
                msg.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    let injected = std::fs::read_to_string(&msg).unwrap();
    let predicted = injected
        .lines()
        .find_map(|l| l.strip_prefix("Rift-Checkpoint: "))
        .expect("the trailer was injected")
        .to_owned();

    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    assert_eq!(
        checkpoint_shards(&repo),
        vec![format!("{}/{}", &predicted[..2], &predicted[2..])],
        "the archive landed exactly where the trailer promised"
    );

    provenance_verbs_answer_over_the_real_shard(&repo, &env, &predicted);
}

/// The provenance composition (called from the predicted-trailer test, which owns the
/// only fixture in this file that archives a REAL shard): `checkpoint
/// explain <id>` resolves natively, and `why` walks line → commit → trailer →
/// checkpoint → session end-to-end.
fn provenance_verbs_answer_over_the_real_shard(repo: &Path, env: &[(&str, &str)], predicted: &str) {
    // The query surface (unlike the hook surface) sits behind the `.rift/`
    // init guard, so the fixture opts in the way `rift init` would.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let (code, out) = run_rift_stdout(repo, &["checkpoint", "explain", predicted, "--json"], env);
    assert_eq!(code, 0, "explain by id: {out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["checkpoint_id"], predicted);
    assert_eq!(
        v["sessions"][0]["session_id"],
        rift_core::capture_session_row_id("claude-code", "e2e-live-1"),
        "the bridge's linkage answers with the DERIVED session row id even \
         though this fixture seeded the cache directly (no SessionStart ever \
         ingested a projection session row): {v}"
    );
    assert_eq!(
        v["sessions"][0]["agent"], "unknown",
        "an un-ingested session keeps its linkage and says its metadata is \
         unknown rather than vanishing: {v}"
    );
    assert_eq!(
        v["metadata"]["sessions"][0], "e2e-live-1",
        "the shard itself records the NATIVE session id: {v}"
    );
    assert!(
        v["metadata"]["canon_version"].as_u64().is_some(),
        "shard metadata was read from the ref: {v}"
    );

    // A commit that CARRIES the trailer gives `why` a real chain to walk.
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    git_in(repo, &["add", "b.txt"]);
    git_in(
        repo,
        &[
            "commit",
            "-q",
            "-m",
            &format!("carry\n\nRift-Checkpoint: {predicted}"),
        ],
    );
    let (code, out) = run_rift_stdout(repo, &["why", "b.txt:1", "--json"], env);
    assert_eq!(code, 0, "why through a real trailer: {out}");
    let w: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(w["selection"]["checkpoint_id"], predicted);
    assert_eq!(
        w["sessions"][0]["session_id"],
        rift_core::capture_session_row_id("claude-code", "e2e-live-1")
    );
    assert_eq!(
        w["prompt"],
        serde_json::Value::Null,
        "this session archived no transcript, and why says absence rather than guessing"
    );
    assert!(
        w["handles"]["file"]
            .as_str()
            .is_some_and(|h| h.starts_with("file:b.txt@sha256:")),
        "{w}"
    );
}

/// A redactor pack whose regex does not compile must reach the developer
/// **within one commit cycle**.
///
/// It did not. `prepare-commit-msg` loaded the packs and dropped the loader's
/// notes on the floor (`let (packs, _notes) = …`), and the only other loader on
/// the commit path runs in the detached worker, whose stderr is `/dev/null`. So
/// the repository's coverage silently fell back to the built-ins and nothing
/// anywhere said so — the silent-degradation failure mode, in the one plane where it is a
/// security property rather than an inconvenience.
///
/// The cycle is driven as git drives it: `prepare-commit-msg`, then
/// `post-commit`. The assertion is on the second one's stderr, because that is
/// the sweep that says markers out loud.
#[test]
fn a_pack_that_will_not_compile_is_said_within_one_commit_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let packs = repo.join(".rift").join("policies").join("redactors");
    std::fs::create_dir_all(&packs).unwrap();
    std::fs::write(
        packs.join("broken.toml"),
        "schema_version = 1\nname = \"broken\"\nversion = \"1\"\n\
         [[rules]]\nid = \"unclosed\"\nregex = \"ACME-[0-9\"\n",
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let msg = tmp.path().join("COMMIT_EDITMSG");
    std::fs::write(&msg, "do the thing\n").unwrap();

    // Cycle step 1. This hook notices; it is not the one that speaks.
    let (code, _) = prepare_commit_msg_hook(&repo, &msg, &env);
    assert_eq!(code, 0, "a broken pack never blocks a commit");

    // Cycle step 2. This one does.
    let stderr = archive_head(&repo, &env);
    assert!(
        stderr.contains("unclosed"),
        "the rule that was dropped is named: {stderr:?}"
    );
    assert!(
        stderr.contains("did not compile"),
        "and why it was dropped: {stderr:?}"
    );
    // Counted by LINE, not by token: the compiler's own reason happens to
    // contain the rule's name too, so `matches("unclosed")` would read 2 for a
    // single note and quietly turn this into an assertion about wording.
    assert_eq!(
        stderr
            .lines()
            .filter(|l| l.contains("redactor rule"))
            .count(),
        1,
        "said once for the condition, not once per path that noticed it: {stderr:?}"
    );

    // …and the commit still produced its checkpoint: a broken pack costs its
    // own rule, never the capture.
    assert_eq!(
        checkpoint_shards(&repo).len(),
        1,
        "the archive still landed"
    );

    // The WORKER's own notice, isolated: this second `post-commit` has no
    // `prepare-commit-msg` before it, so anything it says came from the marker
    // the previous cycle's worker left. That path is not redundant — a merge
    // commit's `prepare-commit-msg` returns before the loader runs, so the
    // worker is the only thing that notices at all.
    let gd = repo.join(".git");
    let (code, stderr) = run_rift_capture(
        tmp.path(),
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            gd.to_str().unwrap(),
        ],
        Some(""),
        &env,
    );
    assert_eq!(code, 0);
    assert!(
        stderr.contains("did not compile"),
        "the detached worker's notice reaches the next sweep too: {stderr:?}"
    );
}

/// A repository that never opted anything in must condense exactly as it did
/// before the control existed — same empty capture, therefore the same
/// checkpoint id. Without this, adding `.worktreeinclude` support would silently
/// re-key every checkpoint in every repo that does not use it.
#[test]
fn a_repo_without_the_include_file_condenses_byte_identically() {
    let shard_of = |include: Option<&str>| -> Vec<String> {
        let tmp = tempfile::tempdir().unwrap();
        let repo = seed_live_session_repo(tmp.path());
        if let Some(text) = include {
            std::fs::write(repo.join(".worktreeinclude"), text).unwrap();
        }
        let cache = tmp.path().join("cache");
        assert_eq!(
            run_rift(
                &repo,
                &[
                    "hooks",
                    "git",
                    "post-commit",
                    "--git-dir",
                    repo.join(".git").to_str().unwrap(),
                ],
                Some(""),
                &[
                    ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
                    ("RIFT_NO_KEYCHAIN", "1"),
                    ("RIFT_CAPTURE_SYNC", "1"),
                ],
            ),
            0
        );
        checkpoint_shards(&repo)
    };
    let without = shard_of(None);
    assert_eq!(without.len(), 1);
    assert_eq!(
        shard_of(Some("# nothing opted in\n")),
        without,
        "an include file that names nothing is the same as none at all"
    );
    assert_ne!(
        shard_of(Some("a.txt\n")),
        without,
        "…and one that names something is not — the capture is in the id"
    );
}

/// The fail-loud rule, honoured by loudness instead of by a dial.
///
/// A `.rift/rewrite.toml` used to route every write-side capability, and
/// `kill_switch = true` used to force them all back to the wrapper. Deleting the
/// wrapper (2026-08-11) turned the whole file inert — and an inert safety
/// control that a user still believes in is exactly the silence §3 forbids.
///
/// So the file is now read for one thing: whether it is there. Both directions
/// are asserted here because only the pair is a calibration — a note that always
/// fires and a note that never fires look identical from one side.
///
/// The **content is deliberately the benign case**: `archive = "native"` agreed
/// with reality even before the deletion. If the note only fired on
/// `kill_switch` or a wrapper route, this repository would keep a file it
/// believes configures something, and this test would pass while the user stayed
/// wrong.
#[test]
fn a_stale_rewrite_toml_is_named_once_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    // SILENCE: no file, nothing said, on the hook channel or in doctor.
    archive_head(&repo, &env);
    let quiet = doctor_json(&repo, &env);
    assert_eq!(
        quiet["stale_rewrite_toml"]["present"],
        serde_json::json!(false),
        "no file, no note: {quiet}"
    );

    // NOISE: the file appears, and the next commit's worker leaves the note
    // where the hook after it will say it out loud.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("rewrite.toml"),
        "[routes]\narchive = \"native\"\n",
    )
    .unwrap();
    git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "second"]);
    archive_head(&repo, &env);
    let said = archive_head(&repo, &env);
    assert!(
        said.contains("this file does nothing"),
        "the stale file is named at the next hook: {said:?}"
    );
    assert_eq!(
        said.matches("this file does nothing").count(),
        1,
        "said once, not once per line ever written: {said:?}"
    );

    // …and doctor says it every time, because the FILE is still there even
    // after the marker has been consumed. A marker is said once; a state is
    // reported.
    for pass in 0..2 {
        let v = doctor_json(&repo, &env);
        assert_eq!(
            v["stale_rewrite_toml"]["present"],
            serde_json::json!(true),
            "pass {pass}: doctor reports the file it can still see: {v}"
        );
    }

    // AND IT CHANGED NOTHING: the archive ran exactly as it does without the
    // file. A note that came with a behaviour change would be a route by
    // another name.
    assert!(
        checkpoint_ref_exists(&repo),
        "the checkpoint landed regardless of the stale file"
    );
}
