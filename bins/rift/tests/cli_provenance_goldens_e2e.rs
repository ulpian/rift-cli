//! The POPULATED `--json` contracts, byte-locked, and the squash tie-break
//! proven against trailer order — over one deterministic fixture whose every
//! input is pinned (dates, identity, clock, content), so the whole stdout of
//! each verb is a constant of the tree.
//!
//! The earlier locks froze the EMPTY documents, so the populated shape could
//! drift without a byte moving; these goldens freeze the answering shape.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

use rift_shadow::backend::{ArchivedTranscript, CheckpointArchive, RedactionReportView};
use rift_shadow::ShadowBackend as _;
use rift_store::{CanonicalStore as _, SqliteStore};

#[allow(dead_code)] // The CLI suites share these builders; each file uses a subset.
#[path = "common/cli_fixture.rs"]
mod cli_fixture;
mod common;
use cli_fixture::{code, git, git_commit_pinned, rift, rift_dir};

/// The fixture's two capture instants (epoch ms): X is OLDER, Y is NEWER —
/// and X is listed FIRST in the trailer, so trailer order and creation order
/// disagree on purpose.
const LOCK_T_OLD: i64 = 1_756_000_100_000;
const LOCK_T_NEW: i64 = 1_756_000_200_000;

/// The archives the fixture hand-builds: one session's redacted capture at
/// one recorded instant, with the transcript only on the one whose prompt
/// `why` and `explain` must serve.
struct ArchiveSeed {
    session: &'static str,
    text: &'static str,
    transcript: Option<ArchivedTranscript>,
    at: i64,
}

/// Build the populated provenance world in `repo`: one date-pinned commit of
/// `f.rs` whose message claims TWO checkpoints (trailer order X, Y), two real
/// shards archived through the production writer with recorded capture times
/// (X older, Y newer), the projection derived by the REAL bridge, and
/// deterministic session/attribution/cost rows. Returns `(ck_x, ck_y)`.
///
/// The ids, the commit oid, the prompt bytes and every timestamp in the three
/// `--json` documents are functions of pinned inputs — no wall clock, no RNG —
/// which is what makes the byte-locks below possible at all.
fn populated_provenance_fixture(home: &Path, repo: &Path) -> (String, String) {
    stage_the_seed_file(home, repo);
    let (mut ck_x, mut ck_y) = the_two_archives(repo);
    let anchor = commit_claiming_both(home, repo, &ck_x.checkpoint_id, &ck_y.checkpoint_id);
    archive_both_shards(repo, &anchor, &mut ck_x, &mut ck_y);
    derive_and_seed_projection(home, repo, &ck_x.checkpoint_id, &ck_y.checkpoint_id);
    rift_dir(home, repo);
    (ck_x.checkpoint_id, ck_y.checkpoint_id)
}

/// The commit's tree, staged and not yet committed: the archives are built
/// before the commit because the commit message has to claim their ids, and
/// the ids are derived from the archives themselves.
fn stage_the_seed_file(home: &Path, repo: &Path) {
    assert!(git(home, repo, &["init", "-q", "."]).status.success());
    std::fs::write(
        repo.join("f.rs"),
        "alpha line one\nalpha line two\nalpha line three\n",
    )
    .unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
}

/// The two archives, through the same functions the condense path calls.
fn the_two_archives(repo: &Path) -> (CheckpointArchive, CheckpointArchive) {
    let packs = rift_shadow::redact::RulePackSet::builtin();
    // Y carries the transcript, so the SELECTED checkpoint is the one whose
    // prompt `why`/`explain` must serve.
    let transcript = the_archived_transcript(&packs);
    let ck_x = archive_for(
        repo,
        &packs,
        ArchiveSeed {
            session: "01JG3LOCKA",
            text: "capture x: the older checkpoint\n",
            transcript: None,
            at: LOCK_T_OLD,
        },
    );
    let ck_y = archive_for(
        repo,
        &packs,
        ArchiveSeed {
            session: "01JG3LOCKB",
            text: "capture y: the newer checkpoint\n",
            transcript: Some(transcript),
            at: LOCK_T_NEW,
        },
    );
    (ck_x, ck_y)
}

/// The prompt the locked documents serve back, in the dialect the compactor
/// models: one user turn and one costed assistant turn.
fn the_archived_transcript(packs: &rift_shadow::redact::RulePackSet) -> ArchivedTranscript {
    let raw = concat!(
        r#"{"type":"user","timestamp":"2026-08-01T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"wire the alpha parser"}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-08-01T00:00:01Z","message":{"id":"msg_01G3LOCK","role":"assistant","usage":{"input_tokens":64,"output_tokens":8},"content":[{"type":"text","text":"Wiring it."}]}}"#,
        "\n",
    );
    let (transcript, _lines) = rift_shadow::archived_transcript(
        &rift_core::SecretBytes::new(raw.as_bytes().to_vec()),
        0,
        packs,
    )
    .unwrap();
    transcript
}

/// One archive with its id derived from its own bytes, exactly as the writer
/// derives it. A hand-built archive is in no repository: the member is
/// ABSENT, which is what keeps every frozen anchor and golden byte-identical.
fn archive_for(
    repo: &Path,
    packs: &rift_shadow::redact::RulePackSet,
    seed: ArchiveSeed,
) -> CheckpointArchive {
    let out = rift_shadow::redact::redact_with(
        &rift_core::SecretBytes::new(seed.text.as_bytes().to_vec()),
        packs,
    )
    .unwrap();
    let mut archive = CheckpointArchive {
        repo: repo.to_path_buf(),
        commit: String::new(),
        checkpoint_id: String::new(),
        sessions: vec![seed.session.to_owned()],
        strategy: "manual-commit".to_owned(),
        prev_hash: None,
        events: Vec::new(),
        read_facts: Vec::new(),
        serve_audit: None,
        usage: Vec::new(),
        session_agents: std::collections::BTreeMap::new(),
        window_journal: Vec::new(),
        content: out.redacted,
        report: RedactionReportView::from_redact(&out.report),
        transcript: seed.transcript,
        tasks: Vec::new(),
        created_at_ms: Some(seed.at),
        worktree: None,
        session_worktrees: std::collections::BTreeMap::new(),
    };
    archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
    archive
}

/// The commit CLAIMS both — X first, so the legacy `candidates[0]` rule and
/// the created-at rule name different checkpoints. Returns the anchor oid.
fn commit_claiming_both(home: &Path, repo: &Path, ck_x: &str, ck_y: &str) -> String {
    let msg = format!("seed\n\nRift-Checkpoint: {ck_x}\nRift-Checkpoint: {ck_y}");
    assert!(
        git_commit_pinned(home, repo, "2026-01-02T03:04:05 +0000", &msg)
            .status
            .success()
    );
    let head = git(home, repo, &["rev-parse", "HEAD"]);
    String::from_utf8_lossy(&head.stdout).trim().to_owned()
}

/// Anchor recorded, ids untouched by it (the anchoring-commit rule), then
/// both shards through the production writer.
fn archive_both_shards(
    repo: &Path,
    anchor: &str,
    ck_x: &mut CheckpointArchive,
    ck_y: &mut CheckpointArchive,
) {
    anchor.clone_into(&mut ck_x.commit);
    anchor.clone_into(&mut ck_y.commit);
    let git_repo = rift_git::discover(repo).unwrap();
    let backend = rift_shadow::NativeBackend::new(rift_git::PlumbingGitWrite::new(git_repo));
    backend.archive_checkpoint(ck_x).unwrap();
    backend.archive_checkpoint(ck_y).unwrap();
}

/// The REAL bridge derives the projection — `checkpoint.created_at` comes
/// from each shard's own `metadata.json`, never from this call's clock — and
/// the rows the live capture path writes go in beside it.
fn derive_and_seed_projection(home: &Path, repo: &Path, ck_x: &str, ck_y: &str) {
    let repo_id = rift_git::repo_id_for(repo).unwrap();
    let db = rift_store::projection_db_under(&home.join("cache"), &repo_id);
    let store = rift_store::open_capture_projection(&db).unwrap();
    let reader = rift_recall_substrate::GixSubstrate::open(repo).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(rift_shadow::ingest_from_substrate(
        &store,
        &reader,
        &repo_id.to_string(),
        "claude-code",
        1_756_000_300_000,
    ))
    .unwrap();
    rt.block_on(async {
        seed_sessions(&store, &repo_id.to_string()).await;
        seed_file_attribution(&store, ck_x, ck_y).await;
        seed_costed_call(&store).await;
    });
}

/// Deterministic session metadata for both captures, started five seconds
/// before the instant each shard records.
async fn seed_sessions(store: &SqliteStore, repo_id: &str) {
    for (native, started) in [("01JG3LOCKA", LOCK_T_OLD), ("01JG3LOCKB", LOCK_T_NEW)] {
        store
            .insert(
                "session",
                &[
                    "session_id",
                    "agent",
                    "model_primary",
                    "repo_id",
                    "started_at",
                    "schema_version",
                ],
                &[
                    text(&session_row_id(native)),
                    text("claude-code"),
                    text("claude-sonnet-4-5"),
                    text(repo_id),
                    rift_store::Value::Int(started - 5_000),
                    rift_store::Value::Int(1),
                ],
            )
            .await
            .unwrap();
    }
}

/// Per-file attribution for BOTH claims, so rank criterion 1 ties and
/// criterion 3 — the capture instant — is what decides the file.
async fn seed_file_attribution(store: &SqliteStore, ck_x: &str, ck_y: &str) {
    rift_store::record_file_revision(
        store,
        &rift_store::FileRevisionRow {
            file_revision_id: "fr-g3".into(),
            path: "f.rs".into(),
            blob_oid: "b0".into(),
            content_sha256: "c0".into(),
            schema_version: 1,
        },
    )
    .await
    .unwrap();
    for (ck, agent_added, human_added, conf) in [(ck_x, 3, 0, 900), (ck_y, 2, 1, 850)] {
        rift_store::record_attribution(
            store,
            &rift_store::AttributionRow {
                checkpoint_id: ck.to_owned(),
                file_revision_id: "fr-g3".into(),
                agent_added,
                human_added,
                human_modified_agent: 0,
                agent_removed_by_human: 0,
                confidence_milli: conf,
                method_version: "m1".into(),
                supersedes_pk: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }
}

/// One costed model call on Y's session — the turn, the call and the exact
/// token counts `why` and `explain` report as cost.
async fn seed_costed_call(store: &SqliteStore) {
    store
        .insert(
            "turn",
            &[
                "turn_id",
                "session_id",
                "ordinal",
                "started_at",
                "schema_version",
            ],
            &[
                text("turn-g3"),
                text(&session_row_id("01JG3LOCKB")),
                rift_store::Value::Int(1),
                rift_store::Value::Int(LOCK_T_NEW - 4_000),
                rift_store::Value::Int(1),
            ],
        )
        .await
        .unwrap();
    store
        .insert(
            "model_call",
            &[
                "call_id",
                "turn_id",
                "model",
                "started_at",
                "schema_version",
            ],
            &[
                text("mc-g3"),
                text("turn-g3"),
                text("claude-sonnet-4-5"),
                rift_store::Value::Int(LOCK_T_NEW - 3_000),
                rift_store::Value::Int(1),
            ],
        )
        .await
        .unwrap();
    store
        .insert(
            "token_usage",
            &[
                "call_id",
                "uncached_input",
                "cached_input",
                "cache_creation",
                "output",
                "counter_method",
                "schema_version",
            ],
            &[
                text("mc-g3"),
                rift_store::Value::Int(64),
                rift_store::Value::Int(0),
                rift_store::Value::Int(0),
                rift_store::Value::Int(8),
                text("exact"),
                rift_store::Value::Int(1),
            ],
        )
        .await
        .unwrap();
}

/// A text column value.
fn text(v: &str) -> rift_store::Value {
    rift_store::Value::text(v.to_owned())
}

/// The projection's row id for one of the fixture's native session ids.
fn session_row_id(native: &str) -> String {
    rift_core::capture_session_row_id("claude-code", native)
}

/// Read one committed golden, comparing BYTES — the shard-goldens discipline:
/// no bless switch, no normalization; a deliberate contract change re-freezes
/// the file in its own commit, with the diff in review.
fn assert_matches_golden(name: &str, got: &[u8]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("goldens-provenance")
        .join(name);
    let want = std::fs::read(&path).unwrap_or_else(|_| {
        panic!(
            "{name}: no frozen golden. There is no bless switch — place \
             this file by hand after reading it:\n----8<----\n{}\n----8<----",
            String::from_utf8_lossy(got)
        )
    });
    assert!(
        want == got,
        "the locked {name} document moved.\n--- frozen ---\n{}\n--- got ---\n{}",
        String::from_utf8_lossy(&want),
        String::from_utf8_lossy(got),
    );
}

/// **The populated `--json` documents are byte-locked.** The
/// earlier locks froze the EMPTY documents — `"checkpoints": {}`, `"prompt":
/// null` — so the populated shape could drift without a byte moving. These
/// three goldens freeze the answering shape: resolved spans, attribution
/// counts, sessions with labels, a non-null redacted prompt served from the
/// shard, cost, and a real `created_at_ms` — the injected capture instant,
/// which is also the proof the writer fix reaches the documents.
#[test]
fn the_populated_blame_why_explain_documents_are_byte_locked() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let (_ck_x, ck_y) = populated_provenance_fixture(home.path(), repo.path());

    let blame = rift(home.path(), repo.path(), &["blame", "f.rs", "--json"]);
    assert_eq!(code(&blame), 0, "{blame:?}");
    assert_matches_golden("blame.golden", &blame.stdout);

    let why = rift(home.path(), repo.path(), &["why", "f.rs:2", "--json"]);
    assert_eq!(code(&why), 0, "{why:?}");
    assert_matches_golden("why.golden", &why.stdout);

    let explain = rift(
        home.path(),
        repo.path(),
        &["checkpoint", "explain", &ck_y, "--json"],
    );
    assert_eq!(code(&explain), 0, "{explain:?}");
    assert_matches_golden("explain.golden", &explain.stdout);

    // Beside the frozen bytes, the load-bearing populated facts in the clear —
    // so a legitimate future re-freeze cannot silently drop them.
    let w: serde_json::Value = serde_json::from_slice(&why.stdout).unwrap();
    assert_eq!(w["prompt"]["text"], "wire the alpha parser", "{w}");
    assert_eq!(w["tag"], "mixed", "{w}");
    assert!(w["cost"].is_object(), "{w}");
    // The checkpoint's strategy token rides both documents:
    // a live archive says `manual-commit`; an imported transcript would say
    // `import`.
    assert_eq!(w["checkpoint"]["strategy"], "manual-commit", "{w}");
    let b: serde_json::Value = serde_json::from_slice(&blame.stdout).unwrap();
    assert_eq!(b["checkpoints"][&ck_y]["strategy"], "manual-commit", "{b}");
    // The unknown rule in the clear, beside the frozen bytes: this fixture's
    // archives are hand-built and carry NO `worktree` member, so all three
    // documents answer null — the shape every shard written before worktree
    // provenance existed is in.
    // Null, not the string `main`: a re-freeze that turned absence into the
    // main work tree would be a claim, and it would pass a bytes-only lock.
    assert_eq!(w["checkpoint"]["worktree"], serde_json::Value::Null, "{w}");
    assert_eq!(
        b["checkpoints"][&ck_y]["worktree"],
        serde_json::Value::Null,
        "{b}"
    );
    let e: serde_json::Value = serde_json::from_slice(&explain.stdout).unwrap();
    assert_eq!(e["created_at_ms"], LOCK_T_NEW, "{e}");
    assert_eq!(e["prompt"]["text"], "wire the alpha parser", "{e}");
    assert_eq!(
        e["metadata"]["created_at"], "2025-08-24T01:50:00.000Z",
        "{e}"
    );
    assert_eq!(e["worktree"], serde_json::Value::Null, "{e}");
}

/// **The squash tie-break prefers creation time over trailer order**
/// (proven at the verb): one commit claims X then Y, X is
/// older, both recorded touching the file — the legacy `candidates[0]` rule
/// answers X, the created-at rule answers Y, and the shipped verb must answer
/// Y. Before the writer fix every native checkpoint carried `created_at =
/// -1`, so this exact rule silently degraded to trailer order and no test
/// could tell the two apart.
#[test]
fn a_multi_trailer_commit_resolves_by_created_at_not_trailer_order() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let (ck_x, ck_y) = populated_provenance_fixture(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["blame", "f.rs", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    // The claims really are in trailer order, X before Y…
    let commit = v["spans"][0]["commit"].as_str().unwrap();
    let claims = v["commits"][commit]["claims"].as_array().unwrap();
    assert_eq!(claims.len(), 2, "{v}");
    assert_eq!(claims[0]["checkpoint_id"], ck_x.as_str(), "{v}");
    assert_eq!(claims[0]["ordinal"], 0, "{v}");
    assert_eq!(claims[1]["checkpoint_id"], ck_y.as_str(), "{v}");

    // …and the selection is Y anyway: the newer capture wins the file.
    let sel = &v["spans"][0]["selection"];
    assert_eq!(sel["checkpoint_id"], ck_y.as_str(), "{v}");
    assert_eq!(sel["considered"], 2, "{v}");
    assert_eq!(sel["basis"], "file_touched", "{v}");
    assert_eq!(sel["resolved"], true, "{v}");
    assert_eq!(
        v["checkpoints"][ck_y.as_str()]["created_at_ms"],
        LOCK_T_NEW,
        "the winner's recorded instant, not a sentinel: {v}"
    );
}
