//! End-to-end: the session verbs beside resume — `attach` adopting an
//! out-of-band session, `import` bulk-reading an agent store, the
//! list/info/stop surface, and the hook-environment refusal both share.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::Stdio;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use common::rift_cmd;
use hooks_fixture::{
    agent_hook, checkpoint_shards, current_branch, doctor_json, git_in, rev_parse, rift_json,
    run_rift_stdout, seed_live_session_repo, seed_plain_repo, session_phase, sync_env,
};

/// A repository with one commit and an out-of-band Claude Code session: no
/// journal, no hook ever fired, the transcript at the agent's own default
/// location under a redirected HOME.
struct OutOfBand {
    repo: PathBuf,
    home: PathBuf,
    native: PathBuf,
    cache: PathBuf,
}

impl OutOfBand {
    fn seed(tmp: &Path) -> Self {
        let tmp = &underscored_root(tmp);
        let repo = seed_plain_repo(tmp);
        let home = tmp.join("home");
        let native = common::claude_code_project_dir(&home, &repo).join("oob-1.jsonl");
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::write(
            &native,
            concat!(r#"{"type":"user","message":"out of band work"}"#, "\n"),
        )
        .unwrap();
        let cache = tmp.join("cache");
        common::provision_here(&repo, &cache);
        Self {
            repo,
            home,
            native,
            cache,
        }
    }

    /// The amend is a real `git commit`, so the identity it commits under is
    /// stated here the way `git_in` states it for the fixture's own commits.
    /// Left to git's auto-detection it depends on the machine: a Linux
    /// runner's hostname yields one, a Windows box's `(none)` domain is
    /// refused outright.
    fn env(&self) -> [(&'static str, &str); 7] {
        [
            ("RIFT_CACHE_DIR", self.cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("HOME", self.home.to_str().unwrap()),
            ("GIT_AUTHOR_NAME", "T"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "T"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
        ]
    }

    /// `session attach oob-1 --agent claude-code --json` on a pipe: the document.
    fn attach(&self) -> serde_json::Value {
        rift_json(
            &self.repo,
            &[
                "session",
                "attach",
                "oob-1",
                "--agent",
                "claude-code",
                "--json",
            ],
            &self.env(),
        )
    }

    /// The same attach with the explicit yes that lets it amend the commit.
    fn attach_forced(&self) -> serde_json::Value {
        rift_json(
            &self.repo,
            &[
                "session",
                "attach",
                "oob-1",
                "--agent",
                "claude-code",
                "--force",
                "--json",
            ],
            &self.env(),
        )
    }
}

/// A repository with one commit and Claude Code's per-repo transcript store
/// holding two sessions under a redirected HOME — what `import` reads.
struct ClaudeStore {
    repo: PathBuf,
    home: PathBuf,
    store_dir: PathBuf,
    cache: PathBuf,
}

impl ClaudeStore {
    fn seed(tmp: &Path) -> Self {
        let tmp = &underscored_root(tmp);
        let repo = seed_plain_repo(tmp);
        let home = tmp.join("home");
        let store_dir = common::claude_code_project_dir(&home, &repo);
        std::fs::create_dir_all(&store_dir).unwrap();
        std::fs::write(
            store_dir.join("s-one.jsonl"),
            concat!(
                r#"{"type":"user","message":"first imported session"}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            store_dir.join("s-two.jsonl"),
            concat!(
                r#"{"type":"user","message":"second imported session"}"#,
                "\n"
            ),
        )
        .unwrap();
        let cache = tmp.join("cache");
        common::provision_here(&repo, &cache);
        Self {
            repo,
            home,
            store_dir,
            cache,
        }
    }

    fn env(&self) -> [(&'static str, &str); 3] {
        [
            ("RIFT_CACHE_DIR", self.cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("HOME", self.home.to_str().unwrap()),
        ]
    }

    /// `import claude-code <args> --json`: the document.
    fn import(&self, extra: &[&str]) -> serde_json::Value {
        let mut args = vec!["import", "claude-code"];
        args.extend_from_slice(extra);
        args.push("--json");
        rift_json(&self.repo, &args, &self.env())
    }

    /// `checkpoint show <id> --json`'s `strategy`.
    fn strategy_of(&self, id: &str) -> serde_json::Value {
        let shown = rift_json(
            &self.repo,
            &["checkpoint", "show", id, "--json"],
            &self.env(),
        );
        shown["strategy"].clone()
    }
}

/// `attach` adopts an out-of-band session (no journal, no hooks ever fired)
/// by finding its native transcript at the agent's default location and
/// archiving it against the last commit through the one archive machinery.
/// Idempotent: an unchanged re-attach derives the same checkpoint id and moves
/// nothing.
#[test]
fn attach_adopts_a_session_idempotently_and_amend_needs_an_explicit_yes() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = OutOfBand::seed(tmp.path());
    let head_before = rev_parse(&fx.repo, "HEAD");

    // 1. Non-TTY without --force: archived, adopted, NOT amended.
    let v = fx.attach();
    let checkpoint_id = v["checkpoint_id"].as_str().unwrap().to_owned();
    assert_eq!(
        v["amended"], false,
        "a pipe never amends without --force: {v}"
    );
    assert_eq!(v["already_claimed"], false, "{v}");
    // Compared canonically: the binary derives its answer from its cwd, which
    // is never the verbatim spelling `canonicalize` gives the expectation.
    assert_eq!(
        v["transcript_path"]
            .as_str()
            .map(|p| std::fs::canonicalize(p).expect("the transcript it names exists")),
        Some(fx.native.canonicalize().unwrap()),
        "found at the agent's default location: {v}"
    );
    assert_eq!(
        rev_parse(&fx.repo, "HEAD"),
        head_before,
        "no amend happened"
    );
    // Adopted: the journal now records the session as ended work.
    let journal = std::fs::read_to_string(common::read_path(
        &common::sessions_dir(&fx.repo.join(".git"), &fx.cache),
        "oob-1.json",
    ))
    .expect("attach adopted the session into the journal");
    assert!(journal.contains(r#""state": "ended""#), "{journal}");
    // The shard is real and readable.
    assert!(
        checkpoint_shards(&fx.repo)
            .first()
            .is_some_and(|s| s.replace('/', "") == checkpoint_id),
        "the shard landed at the reported id"
    );

    // 2. Idempotency: the same attach derives the same id and moves nothing.
    let ref_before = rev_parse(&fx.repo, "refs/heads/rift/checkpoints/v1");
    let v2 = fx.attach();
    assert_eq!(v2["checkpoint_id"], checkpoint_id.as_str(), "same id: {v2}");
    assert_eq!(
        rev_parse(&fx.repo, "refs/heads/rift/checkpoints/v1"),
        ref_before,
        "an unchanged re-attach is a no-op on the checkpoint ref"
    );
}

fn attached_full_transcript(fx: &OutOfBand, id: &str) -> String {
    let names = hooks_fixture::shard_tree_names(&fx.repo);
    let metadata = names
        .lines()
        .find(|path| path.ends_with("/metadata.json") && path.replace('/', "").starts_with(id))
        .unwrap_or_else(|| panic!("missing metadata for {id}; artifacts: {names}"));
    let meta: serde_json::Value =
        serde_json::from_str(&hooks_fixture::shard_blob(&fx.repo, metadata)).unwrap();
    assert_eq!(
        meta["compact_transcript"],
        rift_shadow::backend::TRANSCRIPT_COMPACT_NAME
    );
    assert_eq!(
        meta["compact_transcript_start"], 0,
        "attach must start from the whole transcript"
    );
    let parent = metadata.rsplit_once('/').unwrap().0;
    let compact = hooks_fixture::shard_blob(
        &fx.repo,
        &format!("{parent}/{}", rift_shadow::backend::TRANSCRIPT_COMPACT_NAME),
    );
    assert!(
        compact.contains("out of band work"),
        "complete compact transcript: {compact}"
    );
    let path = format!("{parent}/{}", rift_shadow::backend::TRANSCRIPT_FULL_NAME);
    assert!(
        names.lines().any(|name| name == path),
        "missing native full artifact: {names}"
    );
    hooks_fixture::shard_blob(&fx.repo, &path)
}

fn supported_oob_record(fx: &OutOfBand) {
    std::fs::write(
        &fx.native,
        concat!(
            r#"{"type":"user","uuid":"oob-user-1","timestamp":"2026-10-15T10:00:00Z","message":{"role":"user","content":"out of band work"}}"#,
            "\n"
        ),
    )
    .unwrap();
}

/// Attach must discard progress found through an unmoved legacy fallback,
/// then archive the complete transcript on both identical invocations.
#[test]
fn attach_with_legacy_progress_preserves_full_content_and_the_checkpoint_ref_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = OutOfBand::seed(tmp.path());
    supported_oob_record(&fx);
    let control_tmp = tempfile::tempdir().unwrap();
    let control = OutOfBand::seed(control_tmp.path());
    supported_oob_record(&control);
    let control_result = control.attach();
    let control_id = control_result["checkpoint_id"].as_str().unwrap();
    assert!(
        attached_full_transcript(&control, control_id).contains("out of band work"),
        "normal control must archive the supported record"
    );
    eprintln!("no-progress control: full and compact content verified");
    let git_dir = fx.repo.join(".git").canonicalize().unwrap();
    let sessions = common::sessions_dir(&git_dir, &fx.cache);
    let member = sessions.parent().unwrap();
    rift_shadow::hooks::run_state::record_git_dir(member, &git_dir).unwrap();
    rift_shadow::hooks::run_state::record_work_tree(member, &fx.repo).unwrap();
    std::fs::create_dir_all(&sessions).unwrap();
    let old = common::seed_sessions_dir(&git_dir);
    rift_shadow::hooks::session_cache::write_transcript_progress(
        &old,
        &rift_shadow::hooks::session_cache::TranscriptProgress {
            schema: 1,
            session_id: "oob-1".into(),
            raw_lines: 1,
            updated_at_ms: 1000,
        },
    )
    .unwrap();
    assert!(!rift_shadow::hooks::run_state::is_moved(member));
    assert_eq!(
        rift_shadow::hooks::session_cache::read_transcript_progress(&sessions, "oob-1")
            .unwrap()
            .unwrap()
            .raw_lines,
        1
    );
    let mut previous = None;
    for _ in 0..2 {
        let value = fx.attach();
        let id = value["checkpoint_id"].as_str().unwrap();
        let full = attached_full_transcript(&fx, id);
        assert!(
            full.contains("out of band work"),
            "complete native transcript: {full}"
        );
        let tip = rev_parse(&fx.repo, "refs/heads/rift/checkpoints/v1");
        if let Some((prior_id, prior_tip)) = &previous {
            assert_eq!(id, prior_id);
            assert_eq!(&tip, prior_tip);
        }
        previous = Some((id.to_owned(), tip));
    }
}

/// Only on an explicit yes does `attach` amend the commit to carry the
/// trailer; once the claim stands nothing amends again, and the provenance
/// surface resolves the attached checkpoint.
#[test]
fn attach_with_force_amends_the_commit_once_and_explain_resolves_it() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = OutOfBand::seed(tmp.path());
    let head_before = rev_parse(&fx.repo, "HEAD");
    let checkpoint_id = fx.attach()["checkpoint_id"].as_str().unwrap().to_owned();

    // 3. --force: the amend fires, HEAD moves, the message carries the trailer.
    let v3 = fx.attach_forced();
    assert_eq!(v3["amended"], true, "{v3}");
    let head_after = rev_parse(&fx.repo, "HEAD");
    assert_ne!(head_after, head_before, "the amend rewrote the commit");
    assert!(
        git_in(&fx.repo, &["log", "-1", "--format=%B"])
            .contains(&format!("Rift-Checkpoint: {checkpoint_id}")),
        "the trailer rode the amend"
    );

    // 4. After the amend the claim already stands: nothing amends again.
    let v4 = fx.attach_forced();
    assert_eq!(v4["already_claimed"], true, "{v4}");
    assert_eq!(v4["amended"], false, "{v4}");
    assert_eq!(
        rev_parse(&fx.repo, "HEAD"),
        head_after,
        "an already-claimed attach never amends again"
    );

    // 5. The provenance surface resolves the attached checkpoint.
    let e = rift_json(
        &fx.repo,
        &["checkpoint", "explain", &checkpoint_id, "--json"],
        &fx.env(),
    );
    assert_eq!(e["checkpoint_id"], checkpoint_id.as_str(), "{e}");
    assert_eq!(
        e["prompt"]["text"], "out of band work",
        "the adopted transcript serves its prompt: {e}"
    );
}

/// The fixture's root under `tmp`, named with an underscore: Claude Code
/// folds `_` in a project path like any other non-alphanumeric character,
/// and a temporary directory that happens to carry none would hide a fixture
/// that folds it differently from the binary.
fn underscored_root(tmp: &Path) -> PathBuf {
    let root = tmp.join("fixture_root");
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A live session with one turn behind it, provider-reported usage on the
/// turn end: `(repo, cache)`.
fn one_turn_session(tmp: &Path) -> (PathBuf, PathBuf) {
    let repo = seed_live_session_repo(tmp);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.join("cache");
    let env = sync_env(&cache);
    agent_hook(
        &repo,
        "UserPromptSubmit",
        "e2e-live-1",
        1,
        r#"{"text":"work"}"#,
        &env,
    );
    agent_hook(
        &repo,
        "Stop",
        "e2e-live-1",
        1,
        r#"{"usage":{"input_tokens":5,"output_tokens":7}}"#,
        &env,
    );
    (repo, cache)
}

/// The row of a `session list` document naming `id`.
fn listed_session(doc: &serde_json::Value, id: &str) -> serde_json::Value {
    doc["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["session_id"] == id)
        .unwrap_or_else(|| panic!("{id} is listed: {doc}"))
        .clone()
}

/// The session-state surface, read side. `list` carries ages, agents, models
/// and token totals — through the `sessions` alias, which the PRD spells;
/// `info` adds the session's checkpoints; an unknown id is a typed refusal.
#[test]
fn sessions_list_and_info_carry_the_live_sessions_identity_and_totals() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = one_turn_session(tmp.path());
    let env = sync_env(&cache);

    let v = rift_json(&repo, &["sessions", "list", "--json"], &env);
    let row = listed_session(&v, "e2e-live-1");
    assert_eq!(row["agent"], "claude-code", "{row}");
    assert_eq!(row["state"], "active", "{row}");
    assert!(
        row["age_ms"].as_i64().is_some(),
        "an age rides the row: {row}"
    );
    assert_eq!(
        row["tokens"]["output"], 7,
        "token totals ride the row: {row}"
    );
    assert_eq!(row["tokens"]["uncached_input"], 5, "{row}");

    // info: the same identity plus the session's checkpoints (none yet).
    let i = rift_json(&repo, &["session", "info", "e2e-live-1", "--json"], &env);
    assert_eq!(i["state"], "active", "{i}");
    assert_eq!(i["checkpoints"].as_array().unwrap().len(), 0, "{i}");
    // …and an unknown id is a typed refusal, not an empty document.
    let (code, out) = run_rift_stdout(&repo, &["session", "info", "nope", "--json"], &env);
    assert_eq!(code, 4, "{out}");
    let e: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(e["error"], "unknown_session", "{e}");
}

/// The session-state surface, write side. `stop` finalizes through the state
/// machine's IdleTimeout → condense → CondenseDone path (targeted: nothing
/// else is even looked at), records its own chain position, and is idempotent
/// (`already_ended`); the stopped session lists as ended and is exactly what
/// the resume picker offers.
#[test]
fn session_state_surface_lists_infos_and_stops_through_the_machine() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = one_turn_session(tmp.path());
    let env = sync_env(&cache);

    // stop: the machine finalizes it — IdleTimeout -> condense (a real
    // checkpoint lands, SessionEnd reason) -> CondenseDone -> `ended`.
    let s = rift_json(&repo, &["session", "stop", "e2e-live-1", "--json"], &env);
    assert_eq!(s["finalized"][0], "e2e-live-1", "{s}");
    assert!(
        !checkpoint_shards(&repo).is_empty(),
        "the stop condensed a checkpoint"
    );
    assert_eq!(session_phase(&repo, "e2e-live-1"), "ended");
    // The stop's condense recorded its OWN chain position (P84Bv claim-gap
    // pin): report-only doctor — which heals NOTHING — sees no tail gap.
    // Before archive_now recorded in-op, this exact read showed gap 1.
    let report = doctor_json(&repo, &env);
    assert_eq!(
        report["capture_chain"]["gap"], 0,
        "the stop-condensed checkpoint is IN the chain without any heal: {report}"
    );
    assert!(
        report["capture_chain"]["recorded"].as_i64().unwrap_or(0) >= 1,
        "…and the chain records it: {report}"
    );

    // A second stop is honest about it.
    let s2 = rift_json(&repo, &["session", "stop", "e2e-live-1", "--json"], &env);
    assert_eq!(s2["already_ended"][0], "e2e-live-1", "{s2}");

    // The stopped session now lists as ended — and still resumable from the
    // picker's list (a stopped session is exactly what resume shows).
    let after = rift_json(&repo, &["session", "list", "--json"], &env);
    assert_eq!(listed_session(&after, "e2e-live-1")["state"], "ended");
    let picker = rift_json(&repo, &["session", "resume", "--json"], &env);
    assert!(
        picker["resumable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["session_id"] == "e2e-live-1" && r["state"] == "ended"),
        "an ended session is exactly what the picker offers: {picker}"
    );
}

/// `import <agent>` bulk-reads the agent's per-repo transcript store into
/// READ-ONLY checkpoints — dry-run writes nothing, a real run chains each
/// shard once (the appended positions are the run's import epoch), a re-run is
/// a full no-op (same content-derived ids; ref and chain unmoved), and the
/// import marker survives a rebuild.
#[test]
fn import_bulk_reads_the_agent_store_idempotently_as_read_only_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = ClaudeStore::seed(tmp.path());

    // Dry run: the report, and NOTHING on disk.
    let v = fx.import(&["--dry-run"]);
    assert_eq!(v["dry_run"], true, "{v}");
    assert_eq!(v["sessions"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["imported"], 0, "{v}");
    assert!(
        checkpoint_shards(&fx.repo).is_empty(),
        "dry-run wrote nothing"
    );

    // The real run: two read-only checkpoints, chained as one import epoch.
    let v = fx.import(&[]);
    assert_eq!(v["imported"], 2, "{v}");
    assert_eq!(v["already_imported"], 0, "{v}");
    assert_eq!(v["import_epoch"]["start_position"], 0, "{v}");
    assert_eq!(v["import_epoch"]["end_position"], 1, "{v}");
    let first = v["sessions"][0].clone();
    assert_eq!(first["session_id"], "s-one", "name-sorted: {v}");
    assert_eq!(first["status"], "imported", "{v}");
    let id1 = first["checkpoint_id"].as_str().unwrap().to_owned();
    assert_eq!(checkpoint_shards(&fx.repo).len(), 2, "two shards landed");
    // The import marker: the projection row says `import`,
    // not `manual-commit` — an imported transcript is not a live archive.
    assert_eq!(fx.strategy_of(&id1), "import");
    let ref_tip = rev_parse(&fx.repo, "refs/heads/rift/checkpoints/v1");

    // Idempotency: the re-run derives the same ids and moves NOTHING.
    let v2 = fx.import(&[]);
    assert_eq!(v2["imported"], 0, "{v2}");
    assert_eq!(v2["already_imported"], 2, "{v2}");
    assert_eq!(
        v2["import_epoch"],
        serde_json::Value::Null,
        "a no-op run appends no epoch: {v2}"
    );
    assert_eq!(
        rev_parse(&fx.repo, "refs/heads/rift/checkpoints/v1"),
        ref_tip,
        "the checkpoint ref is unmoved by a re-import"
    );
    // …and `rift rebuild` re-derives the marker from the shard's own
    // metadata.json, so the projection cannot forget it.
    rift_json(&fx.repo, &["rebuild", "--json"], &fx.env());
    assert_eq!(
        fx.strategy_of(&id1),
        "import",
        "rebuild lost the import marker"
    );
}

/// The first import records an `import`-basis worktree observation after the
/// archive; a checkpoint freezes hook-basis observations only, so that record
/// cannot reach the second import's derivation: the unchanged session keeps
/// its checkpoint id and the checkpoint ref stays where it was.
#[test]
fn reimporting_an_unchanged_session_keeps_its_checkpoint_id_and_moves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = ClaudeStore::seed(tmp.path());

    let v = fx.import(&["--session", "s-one"]);
    assert_eq!(v["imported"], 1, "{v}");
    let checkpoint_id = v["sessions"][0]["checkpoint_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ref_before = rev_parse(&fx.repo, "refs/heads/rift/checkpoints/v1");

    let v2 = fx.import(&["--session", "s-one"]);
    assert_eq!(v2["imported"], 0, "{v2}");
    assert_eq!(v2["sessions"][0]["status"], "already-imported", "{v2}");
    assert_eq!(
        v2["sessions"][0]["checkpoint_id"],
        checkpoint_id.as_str(),
        "same id: {v2}"
    );
    assert_eq!(
        rev_parse(&fx.repo, "refs/heads/rift/checkpoints/v1"),
        ref_before,
        "an unchanged re-import is a no-op on the checkpoint ref"
    );
    assert_eq!(checkpoint_shards(&fx.repo).len(), 1, "one shard, not two");
}

/// Read-only means NOT resumable: no journal entry rides an import and no
/// commit claims one, so both resume surfaces answer emptiness — but the
/// imports ARE explainable by id, and `--session` imports exactly the named
/// one.
#[test]
fn imported_sessions_are_explainable_but_never_resumable() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = ClaudeStore::seed(tmp.path());
    let v = fx.import(&[]);
    let id1 = v["sessions"][0]["checkpoint_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let picker = rift_json(&fx.repo, &["session", "resume", "--json"], &fx.env());
    assert_eq!(
        picker["resumable"].as_array().unwrap().len(),
        0,
        "imported sessions are not resumable: {picker}"
    );
    let branch = current_branch(&fx.repo);
    let (code, out) = run_rift_stdout(
        &fx.repo,
        &["session", "resume", &branch, "--json"],
        &fx.env(),
    );
    assert_eq!(code, 4, "{out}");
    let w: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        w["error"], "no_checkpoint",
        "no commit claims an import: {w}"
    );

    // …but they ARE searchable/explainable: explain by id serves the prompt.
    let e = rift_json(
        &fx.repo,
        &["checkpoint", "explain", &id1, "--json"],
        &fx.env(),
    );
    assert_eq!(e["prompt"]["text"], "first imported session", "{e}");

    // --session imports exactly the named one.
    std::fs::write(
        fx.store_dir.join("s-three.jsonl"),
        concat!(
            r#"{"type":"user","message":"third imported session"}"#,
            "\n"
        ),
    )
    .unwrap();
    let v3 = fx.import(&["--session", "s-three"]);
    assert_eq!(v3["imported"], 1, "{v3}");
    assert_eq!(v3["sessions"].as_array().unwrap().len(), 1, "{v3}");
    assert_eq!(v3["sessions"][0]["session_id"], "s-three", "{v3}");
}

/// The hook-refusal rail (import half): the same typed,
/// byte-locked refusal attach gives.
#[test]
fn import_refuses_to_run_from_a_hook_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let mut cmd = rift_cmd(&repo);
    cmd.args(["import", "claude-code", "--json"])
        .env("RIFT_CACHE_DIR", cache.to_str().unwrap())
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("GIT_DIR", repo.join(".git"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(4), "a typed refusal");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\n  \"env\": \"GIT_DIR\",\n  \"error\": \"refused_in_hook\",\n  \"verb\": \"import\"\n}\n",
        "the refusal document is locked"
    );
    assert!(checkpoint_shards(&repo).is_empty(), "nothing was imported");
}

/// The hook-refusal rail (attach half): a hook environment
/// gets a typed, byte-locked refusal — never an archive, never a hang.
#[test]
fn attach_refuses_to_run_from_a_hook_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let mut cmd = rift_cmd(&repo);
    cmd.args([
        "session",
        "attach",
        "e2e-live-1",
        "--agent",
        "claude-code",
        "--json",
    ])
    .env("RIFT_CACHE_DIR", cache.to_str().unwrap())
    .env("RIFT_NO_KEYCHAIN", "1")
    // The exact variable git exports to every hook it runs. `rift_cmd`
    // scrubs it from the inherited environment; setting it here recreates
    // the hook shape deliberately.
    .env("GIT_DIR", repo.join(".git"))
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(4), "a typed refusal");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\n  \"env\": \"GIT_DIR\",\n  \"error\": \"refused_in_hook\",\n  \"verb\": \"attach\"\n}\n",
        "the refusal document is locked"
    );
    assert!(
        checkpoint_shards(&repo).is_empty(),
        "nothing was archived from a hook"
    );
    // Rift's own worker marker refuses too — the rail is not git-shaped only.
    let mut cmd = rift_cmd(&repo);
    cmd.args([
        "session",
        "attach",
        "e2e-live-1",
        "--agent",
        "claude-code",
        "--json",
    ])
    .env("RIFT_CACHE_DIR", cache.to_str().unwrap())
    .env("RIFT_NO_KEYCHAIN", "1")
    .env("RIFT_HOOK", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"env\": \"RIFT_HOOK\""),
        "the rift worker marker is refused by name"
    );
}
