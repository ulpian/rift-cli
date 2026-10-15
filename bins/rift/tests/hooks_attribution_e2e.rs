//! End-to-end: attribution through the shipped binary — the stored rows agree
//! with a recompute from primary sources, and a populated pipeline answers
//! `blame`, `why` and `explain` while unproved authorship stays unknown.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_hook, archive_head, checkpoint_shards, doctor_json, git_in, hook_pair_commit, json_str,
    predicted_trailer, rev_parse, rift_json, seed_live_session_repo, sync_env,
};

/// One file's attribution row as the archive persisted it.
struct StoredAttribution {
    path: String,
    agent: i64,
    human: i64,
    hma: i64,
    conf: i64,
}

/// Two sessions, three commits and recorded prompts/checkpoint associations.
/// Test-authored edits are fixture premises, not production writer evidence.
struct PopulatedPipeline {
    repo: PathBuf,
    cache: PathBuf,
    /// Session 1's predicted trailer id — both of its commits predict it, and
    /// neither of its landed shards is it.
    ckpt1: String,
    /// Session 2's predicted trailer id, which IS its landed shard.
    ckpt3: String,
    sid1: String,
    sid2: String,
}

impl PopulatedPipeline {
    fn build(tmp: &Path) -> Self {
        let repo = seed_live_session_repo(tmp);
        let cache = tmp.join("cache");
        let env = sync_env(&cache);
        let (ckpt1, ckpt2) = session_one(&repo, tmp, &env);
        // MEASURED, then pinned: the predicted trailer is capture-addressed
        // (opted-in worktree bytes + read facts), and neither changed between
        // session 1's commits — turn events are not capture bytes — so the two
        // commits PREDICT the same id. The landed shards still differ (the
        // transcript slice moved); the union blame makes is what keeps that
        // honest.
        assert_eq!(
            ckpt1, ckpt2,
            "unchanged capture predicts the same trailer id"
        );
        // Fixture edits between sessions remain ordinary captured bytes.
        // Session association does not establish who wrote those changes.
        append_to(
            &repo.join("parser.txt"),
            "human refactor line six\nhuman refactor line seven\n",
        );
        let ckpt3 = session_two(&repo, tmp, &env);
        assert_ne!(ckpt2, ckpt3, "a different session is a different capture");
        // The query surface sits behind the `.rift/` init guard.
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        Self {
            repo,
            cache,
            ckpt1,
            ckpt3,
            sid1: rift_core::capture_session_row_id("claude-code", "e2e-live-1"),
            sid2: rift_core::capture_session_row_id("claude-code", "e2e-live-2"),
        }
    }

    fn env(&self) -> [(&'static str, &str); 3] {
        sync_env(&self.cache)
    }

    fn blame(&self, file: &str) -> serde_json::Value {
        rift_json(&self.repo, &["blame", file, "--json"], &self.env())
    }

    fn why(&self, at: &str) -> serde_json::Value {
        rift_json(&self.repo, &["why", at, "--json"], &self.env())
    }

    /// The shard blame RESOLVED for span `idx` of `file` — the id that exists,
    /// which for session 1 is not the id its trailer claims.
    fn resolved_checkpoint(&self, file: &str, idx: usize) -> String {
        self.blame(file)["spans"][idx]["selection"]["checkpoint_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

/// Stored attribution agrees with the exact commit-diff recomputation and
/// public blame output; current observations cannot supply missing writers.
#[test]
fn stored_attribution_agrees_with_blame_and_a_recompute_from_primary_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let (predicted, anchor) = attributed_turn(&repo, tmp.path(), &env);

    let repo_id = rift_git::repo_id_for(&repo).unwrap();
    let db = rift_store::projection_db_under(&cache, &repo_id);
    let store = rift_store::open_capture_projection(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    // STORED: the attribution rows exactly as the archive persisted them.
    let stored = rt.block_on(stored_attribution(&store, &predicted));
    assert_eq!(stored.path, "a.txt");
    assert_eq!(
        (stored.agent, stored.human, stored.hma),
        (0, 0, 0),
        "observed buffers do not establish their writers"
    );

    // RECOMPUTED: the same math, re-run here over the primary sources.
    let f = rt.block_on(recomputed_attribution(&store, &repo, &anchor));
    assert_eq!(
        i64::from(f.attribution.agent_lines),
        stored.agent,
        "stored vs recomputed agent_added"
    );
    assert_eq!(
        i64::from(f.attribution.human_lines),
        stored.human,
        "stored vs recomputed human_added"
    );
    assert_eq!(
        i64::from(f.attribution.human_modified_agent_lines),
        stored.hma,
        "stored vs recomputed human_modified_agent"
    );
    // Confidence within bounds: the same factors re-derived, allowing one
    // milli of rounding.
    #[allow(clippy::cast_possible_truncation)]
    let recomputed_milli = (f.confidence * 1000.0).round() as i64;
    assert!(
        (recomputed_milli - stored.conf).abs() <= 1,
        "stored confidence {} vs recomputed {recomputed_milli}",
        stored.conf
    );

    // …and BLAME surfaces the same stored rows over the same repo. The query
    // surface sits behind the `.rift/` init guard, so the fixture opts in.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let b = rift_json(&repo, &["blame", "a.txt", "--line", "2", "--json"], &env);
    let span = &b["spans"][0];
    assert_eq!(span["attribution"]["agent_added"], stored.agent, "{b}");
    assert_eq!(span["attribution"]["human_added"], stored.human, "{b}");
    assert_eq!(span["attribution"]["confidence_milli"], stored.conf, "{b}");
    assert_eq!(
        span["tag"], "unknown",
        "current observations cannot prove authorship: {b}"
    );
}

/// The fake agent doing real work: a prompt opens turn 1, the "agent" edits
/// the committed file between the turn's boundary hooks, the turn closes, and
/// the real hook pair commits + archives. Deliberately transcript-less, so the
/// predicted trailer is exact and `blame` can walk commit → trailer →
/// checkpoint. Returns `(the predicted checkpoint id, the anchoring commit)`.
fn attributed_turn(repo: &Path, tmp: &Path, env: &[(&str, &str)]) -> (String, String) {
    // Turn 1: prompt → the agent edits a committed file → turn ends. The two
    // boundary snapshots bracket exactly the agent's two appended lines.
    agent_hook(
        repo,
        "UserPromptSubmit",
        "e2e-live-1",
        1,
        r#"{"text":"add a feature"}"#,
        env,
    );
    std::fs::write(
        repo.join("a.txt"),
        "one\nagent line two\nagent line three\n",
    )
    .unwrap();
    agent_hook(repo, "Stop", "e2e-live-1", 1, "{}", env);

    // Commit through the real hook pair; a transcript-less session's predicted
    // trailer is the shard that lands.
    git_in(repo, &["add", "a.txt"]);
    let msg = tmp.join("COMMIT_EDITMSG");
    std::fs::write(&msg, "agent work\n").unwrap();
    let predicted = predicted_trailer(repo, &msg, env);
    git_in(repo, &["commit", "-q", "-F", msg.to_str().unwrap()]);
    let anchor = rev_parse(repo, "HEAD");
    archive_head(repo, env);
    assert_eq!(
        checkpoint_shards(repo),
        vec![format!("{}/{}", &predicted[..2], &predicted[2..])],
        "the archive landed where the trailer promised"
    );
    (predicted, anchor)
}

/// The one attributed file of `checkpoint`, read from the projection.
async fn stored_attribution(
    store: &rift_store::SqliteStore,
    checkpoint: &str,
) -> StoredAttribution {
    use rift_store::CanonicalStore as _;
    let stored = store
        .query(
            "SELECT fr.path, a.agent_added, a.human_added, a.human_modified_agent, \
             a.confidence_milli \
             FROM attribution a \
             JOIN file_revision fr ON fr.file_revision_id = a.file_revision_id \
             WHERE a.checkpoint_id = ? ORDER BY fr.path",
            &[rift_store::Value::text(checkpoint.to_owned())],
        )
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "one attributed file");
    let row = &stored[0];
    StoredAttribution {
        path: row.get_text(0).unwrap(),
        agent: row.get_i64(1).unwrap(),
        human: row.get_i64(2).unwrap(),
        hma: row.get_i64(3).unwrap(),
        conf: row.get_i64(4).unwrap(),
    }
}

/// Recompute the exact commit diff and verify that stored observations do
/// not modify its original attribution, confidence, categories or method.
async fn recomputed_attribution(
    store: &rift_store::SqliteStore,
    repo: &Path,
    anchor: &str,
) -> rift_shadow::attribution::FileChange {
    let reader = rift_git::GixBlobReader::open(repo).unwrap();
    let mut capture = rift_shadow::attribution::capture_commit(&reader, anchor).unwrap();
    let before = format!("{capture:?}");
    rift_shadow::enrich_attribution(store, &reader, "claude-code", "e2e-live-1", &mut capture)
        .await
        .unwrap();
    assert_eq!(format!("{capture:?}"), before);
    capture
        .files
        .into_iter()
        .find(|f| f.path == "a.txt")
        .expect("the edited file is in the recomputed capture")
}

/// Two hook-pair commits in one session: parser creation, then a parser
/// append and util creation. Fixture actors are known to the test; production
/// records the bytes and checkpoints without inferring those writers.
fn session_one(repo: &Path, tmp: &Path, env: &[(&str, &str)]) -> (String, String) {
    // A real transcript for session 1, in the dialect shape the compactor
    // models; its prompt line is what `why`/`explain` must serve back.
    let transcript = tmp.join("s1.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","timestamp":"2026-08-01T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"add the parser feature"}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-08-01T00:00:01Z","message":{"id":"msg_01MULTIA","role":"assistant","usage":{"input_tokens":64,"output_tokens":8},"content":[{"type":"text","text":"Writing the parser."}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    agent_hook(
        repo,
        "UserPromptSubmit",
        "e2e-live-1",
        1,
        &format!(
            r#"{{"text":"add the parser feature","transcript_path":{}}}"#,
            json_str(&transcript)
        ),
        env,
    );
    std::fs::write(
        repo.join("parser.txt"),
        "agent parser line one\nagent parser line two\nagent parser line three\n",
    )
    .unwrap();
    agent_hook(repo, "Stop", "e2e-live-1", 1, "{}", env);
    git_in(repo, &["add", "parser.txt"]);
    let ckpt1 = hook_pair_commit(repo, tmp, "agent parser work", env);

    append_to(
        &repo.join("parser.txt"),
        "human mid-session line four\nhuman mid-session line five\n",
    );
    agent_hook(
        repo,
        "UserPromptSubmit",
        "e2e-live-1",
        2,
        r#"{"text":"tidy the util"}"#,
        env,
    );
    std::fs::write(
        repo.join("util.txt"),
        "agent util line one\nagent util line two\n",
    )
    .unwrap();
    agent_hook(repo, "Stop", "e2e-live-1", 2, "{}", env);
    git_in(repo, &["add", "parser.txt", "util.txt"]);
    let ckpt2 = hook_pair_commit(repo, tmp, "agent util work plus human parser lines", env);
    agent_hook(
        repo,
        "SessionEnd",
        "e2e-live-1",
        2,
        r#"{"reason":"exit"}"#,
        env,
    );
    (ckpt1, ckpt2)
}

/// Session 2: its first boundary observes the human's parser delta; its agent
/// extends `util.txt`; one commit then carries BOTH — the mixed commit real
/// capture produces. Returns the predicted trailer id.
fn session_two(repo: &Path, tmp: &Path, env: &[(&str, &str)]) -> String {
    agent_hook(
        repo,
        "SessionStart",
        "e2e-live-2",
        0,
        r#"{"model":"claude-sonnet-4-5"}"#,
        env,
    );
    agent_hook(
        repo,
        "UserPromptSubmit",
        "e2e-live-2",
        1,
        r#"{"text":"extend the util"}"#,
        env,
    );
    append_to(&repo.join("util.txt"), "agent util line three\n");
    agent_hook(repo, "Stop", "e2e-live-2", 1, "{}", env);
    git_in(repo, &["add", "parser.txt", "util.txt"]);
    let ckpt3 = hook_pair_commit(
        repo,
        tmp,
        "human parser refactor plus agent util extension",
        env,
    );
    agent_hook(
        repo,
        "SessionEnd",
        "e2e-live-2",
        1,
        r#"{"reason":"exit"}"#,
        env,
    );
    ckpt3
}

/// The developer typing: `text` goes on the end of `path`.
fn append_to(path: &Path, text: &str) {
    let extended = std::fs::read_to_string(path).unwrap() + text;
    std::fs::write(path, extended).unwrap();
}

/// `(start, end, tag)` of every span of a blame document, in order.
fn span_shape(doc: &serde_json::Value) -> Vec<(i64, i64, String)> {
    doc["spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["start"].as_i64().unwrap(),
                s["end"].as_i64().unwrap(),
                s["tag"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Unknown-writer spans still resolve to the exact recorded checkpoints and
/// sessions. Transcript-backed predicted IDs may diverge from landed IDs;
/// resolving that association does not establish authorship.
#[test]
fn blame_binds_unknown_writer_spans_to_their_recorded_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let px = PopulatedPipeline::build(tmp.path());
    let parser_doc = px.blame("parser.txt");
    let spans = parser_doc["spans"].as_array().unwrap();
    assert_eq!(
        span_shape(&parser_doc),
        [(1, 3, "unknown"), (4, 5, "unknown"), (6, 7, "unknown")].map(|(s, e, t)| (
            s,
            e,
            t.to_owned()
        )),
        "{parser_doc}"
    );
    let resolved1 = spans[0]["selection"]["checkpoint_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(
        resolved1, px.ckpt1,
        "a transcript-carrying session's trailer is not the landed shard: {parser_doc}"
    );
    assert_eq!(
        parser_doc["commits"][spans[0]["commit"].as_str().unwrap()]["claims"][0]["checkpoint_id"],
        px.ckpt1.as_str(),
        "…while the document still records what the commit claims: {parser_doc}"
    );
    assert_eq!(
        spans[2]["selection"]["checkpoint_id"],
        px.ckpt3.as_str(),
        "a transcript-less session's trailer IS the landed shard: {parser_doc}"
    );

    // Preserve session/checkpoint associations independently of writer claims.
    assert_eq!(
        spans[0]["sessions"][0]["session_id"], px.sid1,
        "{parser_doc}"
    );
    assert_eq!(
        spans[1]["sessions"][0]["session_id"], px.sid1,
        "{parser_doc}"
    );
    assert_ne!(
        spans[1]["selection"]["checkpoint_id"], resolved1,
        "the mid-session lines resolve to session 1's second shard: {parser_doc}"
    );
    assert_eq!(spans[1]["attribution"]["human_added"], 0, "{parser_doc}");
    assert_eq!(spans[1]["attribution"]["agent_added"], 0, "{parser_doc}");
    assert_eq!(
        spans[2]["sessions"][0]["session_id"], px.sid2,
        "{parser_doc}"
    );
}

/// Keep all six spans and their checkpoint, session, and prompt associations
/// through the shipped binary. Current-byte observations establish none of
/// their writers, so every span remains Unknown.
#[test]
fn the_populated_pipeline_keeps_unsupported_authorship_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let px = PopulatedPipeline::build(tmp.path());
    let parser_doc = px.blame("parser.txt");
    let util_doc = px.blame("util.txt");
    assert_eq!(
        span_shape(&util_doc),
        [(1, 2, "unknown"), (3, 3, "unknown")].map(|(s, e, t)| (s, e, t.to_owned())),
        "{util_doc}"
    );
    let uspans = util_doc["spans"].as_array().unwrap();
    assert_eq!(
        uspans[0]["sessions"][0]["session_id"], px.sid1,
        "{util_doc}"
    );
    assert_eq!(
        uspans[1]["sessions"][0]["session_id"], px.sid2,
        "{util_doc}"
    );
    let seed_doc = px.blame("a.txt");
    assert_eq!(seed_doc["spans"].as_array().unwrap().len(), 1, "{seed_doc}");
    assert_eq!(
        seed_doc["spans"][0]["tag"], "unknown",
        "the pre-capture seed line stays honestly unknown: {seed_doc}"
    );

    let all_spans: Vec<&serde_json::Value> = [&parser_doc, &util_doc, &seed_doc]
        .iter()
        .flat_map(|d| d["spans"].as_array().unwrap())
        .collect();
    let known = all_spans.iter().filter(|s| s["tag"] != "unknown").count();
    assert_eq!((all_spans.len(), known), (6, 0));
}

/// Recorded prompts and checkpoint/session associations remain available
/// even when the file writer is Unknown. A transcript-less session yields a
/// null prompt rather than an invented explanation.
#[test]
fn why_serves_recorded_prompts_while_authorship_remains_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let px = PopulatedPipeline::build(tmp.path());
    let resolved1 = px.resolved_checkpoint("parser.txt", 0);

    let why_ai = px.why("parser.txt:2");
    assert_eq!(why_ai["tag"], "unknown", "{why_ai}");
    assert_eq!(
        why_ai["selection"]["checkpoint_id"],
        resolved1.as_str(),
        "{why_ai}"
    );
    assert_eq!(why_ai["sessions"][0]["session_id"], px.sid1, "{why_ai}");
    assert_eq!(
        why_ai["prompt"]["text"], "add the parser feature",
        "{why_ai}"
    );
    assert_eq!(why_ai["prompt"]["extracted"], true, "{why_ai}");

    let w4 = px.why("parser.txt:6");
    assert_eq!(w4["tag"], "unknown", "{w4}");
    assert_eq!(w4["selection"]["checkpoint_id"], px.ckpt3.as_str(), "{w4}");
    assert_eq!(w4["sessions"][0]["session_id"], px.sid2, "{w4}");
    assert_eq!(w4["prompt"], serde_json::Value::Null, "{w4}");
}

/// EXPLAIN affirms with the same non-null redacted prompt over the shard
/// blame resolved — the id that exists, not the phantom the trailer names —
/// and surfaces the real capture time, never the `-1` sentinel.
#[test]
fn explain_affirms_the_prompt_over_the_shard_blame_resolved() {
    let tmp = tempfile::tempdir().unwrap();
    let px = PopulatedPipeline::build(tmp.path());
    let resolved1 = px.resolved_checkpoint("parser.txt", 0);
    let explain_doc = rift_json(
        &px.repo,
        &["checkpoint", "explain", &resolved1, "--json"],
        &px.env(),
    );
    assert_eq!(
        explain_doc["checkpoint_id"],
        resolved1.as_str(),
        "{explain_doc}"
    );
    assert_eq!(
        explain_doc["prompt"]["text"], "add the parser feature",
        "{explain_doc}"
    );
    assert!(
        explain_doc["created_at_ms"]
            .as_i64()
            .is_some_and(|ms| ms > 0),
        "explain must surface the real capture time, not the sentinel: {explain_doc}"
    );
}

/// Fresh attribution remains Unknown despite real captured buffers/audits;
/// checkpoint event dates remain recorded and are not writer evidence.
#[test]
fn stored_authorship_is_unknown_despite_observed_buffers_and_dated_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let px = PopulatedPipeline::build(tmp.path());
    let repo_id = rift_git::repo_id_for(&px.repo).unwrap();
    let db = rift_store::projection_db_under(&px.cache, &repo_id);
    let store = rift_store::open_capture_projection(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    use rift_store::CanonicalStore as _;
    let sums = rt
        .block_on(store.query(
            "SELECT COALESCE(SUM(agent_added),0), COALESCE(SUM(human_added),0) FROM attribution",
            &[],
        ))
        .unwrap();
    let (agent_sum, human_sum) = (sums[0].get_i64(0).unwrap(), sums[0].get_i64(1).unwrap());
    assert_eq!(
        (agent_sum, human_sum),
        (0, 0),
        "no authorship from observations"
    );
    assert!(rt.block_on(store.count("turn_boundary_file")).unwrap() > 0);
    assert!(
        rt.block_on(store.count("turn_boundary_observation"))
            .unwrap()
            > 0
    );
    assert_eq!(rt.block_on(store.count("turn_diff_hunk")).unwrap(), 0);

    let times = rt
        .block_on(store.query(
            "SELECT COUNT(*), COALESCE(SUM(created_at > 0),0) FROM checkpoint",
            &[],
        ))
        .unwrap();
    let (rows, dated) = (times[0].get_i64(0).unwrap(), times[0].get_i64(1).unwrap());
    assert!(rows >= 3, "three hook-pair commits archived: {rows}");
    assert_eq!(
        dated, rows,
        "a native checkpoint row still carries the -1 sentinel"
    );
}

/// DOCTOR's per-session turn-boundary coverage: session 1 fired two prompts
/// and two Stops, and every one landed as its own boundary snapshot — the
/// count describes event coverage, never proof of file writers.
#[test]
fn doctor_counts_every_turn_boundary_the_first_session_fired() {
    let tmp = tempfile::tempdir().unwrap();
    let px = PopulatedPipeline::build(tmp.path());
    let d = doctor_json(&px.repo, &px.env());
    assert_eq!(
        d["turn_boundaries"]["collection"]["authorship"], "unavailable",
        "{d}"
    );
    assert_eq!(
        d["turn_boundaries"]["collection"]["historical_attributions_revalidated"], false,
        "{d}"
    );
    let s1 = d["turn_boundaries"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["session_id"] == px.sid1)
        .unwrap_or_else(|| panic!("session 1 missing from doctor: {d}"));
    assert_eq!(
        (s1["starts"].as_i64(), s1["ends"].as_i64()),
        (Some(2), Some(2)),
        "{d}"
    );
}
