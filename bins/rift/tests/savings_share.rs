//! End-to-end: **the savings share is one number, on every surface that
//! publishes it.**
//!
//! The owner judges the recall plane by a single figure — of the tokens this
//! repository's agents would have read, what share did the plane avoid — and
//! reads it in four places: the reread report, the `savings` verb, the recall
//! status table and the dashboard's Results-reuse card. Four readers over
//! three stores is exactly the shape that produced the recorded dashboard
//! defect (a panel reading recall.db under the capture id while the writer
//! wrote under the recall id), so the agreement is asserted here against one
//! seeded family rather than assumed from a shared function's existence.
//!
//! The three CLI surfaces are driven through the shipped binary, because the
//! store resolution is half of what is being tested. The dashboard's surface
//! is its data plane's own feed, called with the same cache root the binary
//! was given.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

use rift_core::id::FamilyIds;
use rift_git::{FamilyState, MintPolicy};
use rift_store::{CanonicalStore, SqliteStore, Value};

mod common;
use common::rift_cmd;

/// What the fixture seeds, in tokens.
const AVOIDED_CLAUDE: i64 = 300;
const AVOIDED_CODEX: i64 = 700;
const RETURNED: i64 = 40;
const READ_TOKENS: i64 = 9_000;

/// A seeded family: a repository, a cache root, and both of its stores.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    cache: PathBuf,
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

/// Seed one family with decisions in the serving stores and reads in the
/// capture projection.
///
/// Codex decides and never has a captured read — the recorded case the
/// excluding variant exists for — so the fixture carries the asymmetry the
/// surfaces have to report rather than a tidy one they could round away.
fn fixture() -> Fixture {
    fixture_with(0)
}

/// The same fixture, plus `prevented` tokens booked to claude-code under the
/// verb a read that never happened is booked under.
fn fixture_with(prevented: i64) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&repo).unwrap();
    rift_git::test_support::fake_repo(&repo).unwrap();
    let (_, state) = rift_git::family_of(&repo, MintPolicy::Allow).unwrap();
    let FamilyState::Minted(family) = state else {
        panic!("the fixture repository must mint a family identity");
    };

    let recall_db = rift_store::recall_db_for_family_under(&cache, &family);
    std::fs::create_dir_all(recall_db.parent().unwrap()).unwrap();
    let recall = rift_store::open_migrated(&recall_db, &rift_store::recall_migrations()).unwrap();
    let rt = rt();
    for (i, (family_name, avoided)) in [("claude-code", AVOIDED_CLAUDE), ("codex", AVOIDED_CODEX)]
        .iter()
        .enumerate()
    {
        rt.block_on(recall.insert(
            "memd_audit_event",
            &[
                "audit_id",
                "verb",
                "handle",
                "reason",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(format!("a{i}")),
                Value::text("memd.read_intercepted"),
                Value::text("h"),
                Value::Null,
                Value::Int(RETURNED),
                Value::Int(*avoided),
                Value::Int(1_000 + i64::try_from(i).unwrap()),
                Value::Int(1),
            ],
        ))
        .unwrap();
        rt.block_on(recall.insert(
            "serve_call_event",
            &[
                "call_id",
                "channel",
                "tool_name",
                "terminal_verb",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(format!("c{i}")),
                Value::text("hook_inject"),
                Value::text(format!("{family_name}:read-gate")),
                Value::text("memd.read_intercepted"),
                Value::Int(RETURNED),
                Value::Int(*avoided),
                Value::Int(1_000 + i64::try_from(i).unwrap()),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    seed_prevented(&recall, &rt, prevented);

    seed_reads(&cache, &family);

    Fixture {
        _tmp: tmp,
        repo,
        cache,
    }
}

/// Seed the rows a read that never happened leaves behind, in both relations.
fn seed_prevented(recall: &SqliteStore, rt: &tokio::runtime::Runtime, prevented: i64) {
    if prevented <= 0 {
        return;
    }
    rt.block_on(recall.insert(
        "memd_audit_event",
        &[
            "audit_id",
            "verb",
            "handle",
            "reason",
            "tokens_returned",
            "tokens_avoided",
            "recorded_at",
            "schema_version",
        ],
        &[
            Value::text("a-prevented"),
            Value::text(rift_recall_ledger::savings::PREVENTED_VERB),
            Value::text("file:src/lib.rs@sha256:c"),
            Value::text("prevented"),
            Value::Int(0),
            Value::Int(prevented),
            Value::Int(1_000),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(recall.insert(
        "serve_call_event",
        &[
            "call_id",
            "channel",
            "tool_name",
            "terminal_verb",
            "tokens_returned",
            "tokens_avoided",
            "recorded_at",
            "schema_version",
        ],
        &[
            Value::text("c-prevented"),
            Value::text("hook_inject"),
            Value::text("claude-code:prevented"),
            Value::text(rift_recall_ledger::savings::PREVENTED_VERB),
            Value::Int(0),
            Value::Int(prevented),
            Value::Int(1_000),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

/// The six capture-projection rows one read joins through: the measurement
/// query's own joins, so the seeded total is the total the report classifies.
fn seed_reads(cache: &Path, family: &impl FamilyIds) {
    let projection_db = rift_store::projection_db_for_family_under(cache, family);
    std::fs::create_dir_all(projection_db.parent().unwrap()).unwrap();
    let capture = SqliteStore::open(&projection_db).unwrap();
    capture.create_canonical_schema().unwrap();
    let rt = rt();
    let insert = |table: &str, cols: &[&str], vals: &[Value]| {
        rt.block_on(capture.insert(table, cols, vals)).unwrap();
    };
    insert(
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
            Value::text("s1"),
            Value::text("claude-code"),
            Value::text("m"),
            Value::text(family.family_id().to_string()),
            Value::Int(900),
            Value::Int(1),
        ],
    );
    insert(
        "turn",
        &[
            "turn_id",
            "session_id",
            "ordinal",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text("t1"),
            Value::text("s1"),
            Value::Int(0),
            Value::Int(900),
            Value::Int(1),
        ],
    );
    insert(
        "tool_call",
        &[
            "tool_call_id",
            "turn_id",
            "tool_name",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text("tc1"),
            Value::text("t1"),
            Value::text("Read"),
            Value::Int(950),
            Value::Int(1),
        ],
    );
    insert(
        "file_revision",
        &[
            "file_revision_id",
            "path",
            "blob_oid",
            "content_sha256",
            "schema_version",
        ],
        &[
            Value::text("fr1"),
            Value::text("src/lib.rs"),
            Value::text("b1"),
            Value::text("c".repeat(64)),
            Value::Int(1),
        ],
    );
    insert(
        "tool_reads_file",
        &["tool_call_id", "file_revision_id"],
        &[Value::text("tc1"), Value::text("fr1")],
    );
    insert(
        "file_read_tokens",
        &[
            "tool_call_id",
            "input_tokens",
            "tokenizer",
            "schema_version",
        ],
        &[
            Value::text("tc1"),
            Value::Int(READ_TOKENS),
            Value::text(rift_tokens::TOKENIZER),
            Value::Int(1),
        ],
    );
}

/// Run `rift` inside the fixture's family with its own cache root.
fn rift(f: &Fixture, args: &[&str]) -> (i32, String, String) {
    let out = rift_cmd(&f.repo)
        .args(args)
        .env("RIFT_CACHE_DIR", &f.cache)
        .env("RIFT_CONFIG_DIR", f.repo.join(".rift-conf"))
        .env("RIFT_HEADLESS", "1")
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .output()
        .expect("rift runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json(f: &Fixture, args: &[&str]) -> serde_json::Value {
    let (code, out, err) = rift(f, args);
    assert_eq!(code, 0, "rift {args:?} failed: {out}{err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("rift {args:?} json: {e}: {out}"))
}

/// The share as the dashboard's data plane computes it, from the same cache
/// root the binary was handed.
fn canopy_share(f: &Fixture) -> Option<f64> {
    let update = rift_canopy_data::feeds::compute_recall(
        &f.cache,
        &f.repo.to_string_lossy(),
        2_000,
        rift_canopy::RecallWindow::AllTime,
    );
    let rift_canopy::FeedPayload::Recall { savings, .. } = update.payload else {
        panic!("expected the Recall feed");
    };
    // Disabled/quiet is the card's honest absence: no store, no served event,
    // and therefore no share — never a zero line.
    let rift_canopy::RecallView::Data(sd) = savings else {
        return None;
    };
    sd.share.overall
}

/// The one number, on all four surfaces.
#[test]
fn the_savings_share_is_one_number_everywhere() {
    let f = fixture();
    let avoided = AVOIDED_CLAUDE + AVOIDED_CODEX;
    let expected = rift_recall_ledger::savings::share(
        u64::try_from(avoided).unwrap(),
        u64::try_from(READ_TOKENS).unwrap(),
    )
    .expect("the fixture read and avoided tokens");

    // 1. the verb.
    let verb = json(&f, &["savings", "--json"]);
    let verb_share = verb["overall"]["share_pct"].as_f64().expect("a share");
    assert!(
        (verb_share - expected).abs() < 1e-9,
        "the verb: {verb_share} vs {expected}"
    );
    assert_eq!(verb["overall"]["read_tokens"], READ_TOKENS);
    assert_eq!(verb["overall"]["tokens_avoided"], avoided);

    // 2. the recall status table.
    let status = json(&f, &["recall", "status", "--json"]);
    let status_share = status["savings"]["overall"]["share_pct"]
        .as_f64()
        .expect("a share");
    assert!(
        (status_share - verb_share).abs() < f64::EPSILON,
        "status: {status_share} vs {verb_share}"
    );

    // 3. the dashboard's data plane.
    let canopy = canopy_share(&f).expect("a share on the card");
    assert!(
        (canopy - verb_share).abs() < f64::EPSILON,
        "the card: {canopy} vs {verb_share}"
    );

    // 4. the reread report, whose savings line is the same figure. Run last:
    // it is the only one of the four that writes (a metric snapshot).
    let (code, out, err) = rift(&f, &["memory", "reread-report"]);
    assert_eq!(code, 0, "the report failed: {out}{err}");
    let line = out
        .lines()
        .find(|l| l.trim_start().starts_with("savings: avoided"))
        .unwrap_or_else(|| panic!("the report has a savings line: {out}"));
    assert!(
        line.contains(&format!("{expected:.1}%")),
        "the report: {line} (expected {expected:.1}%)"
    );

    // 5. and with a window in force, the three surfaces that publish the share
    // move together to the windowed figure, while the report — which is the
    // WHOLE record's document and reaches its denominator by its own route —
    // keeps the whole record's figure and says so in the same breath. One
    // number per named period; never two periods under one name.
    let (code, out, err) = rift(&f, &["savings", "reset", "--at", "1001"]);
    assert_eq!(code, 0, "reset failed: {out}{err}");
    // From 1001: codex's decision alone, and no read at all.
    let windowed = rift_recall_ledger::savings::share(u64::try_from(AVOIDED_CODEX).unwrap(), 0);
    assert_eq!(
        shares(&f),
        (windowed, windowed, windowed),
        "the verb, the status table and the card follow the marker together"
    );
    let (code, out, err) = rift(&f, &["memory", "reread-report"]);
    assert_eq!(code, 0, "the report failed: {out}{err}");
    assert!(
        out.contains(&format!("{expected:.1}%")),
        "the report still covers the whole record: {out}"
    );
    assert!(
        out.contains("covers the whole record; `rift savings` reports"),
        "and says so where a reader of the two would otherwise compare them: {out}"
    );
}

/// The excluding variant, on the surfaces that publish it: Codex decided and
/// this repository captured none of its reads, so its avoided tokens have no
/// denominator of their own and both sides drop it.
#[test]
fn a_family_with_decisions_and_no_reads_is_excluded_from_the_excluding_variant() {
    let f = fixture();
    let verb = json(&f, &["savings", "--json"]);
    assert_eq!(
        verb["excluded"],
        serde_json::json!(["codex"]),
        "codex decided and never had a captured read"
    );
    let plain = verb["families_share_pct"].as_f64().expect("a share");
    let excluding = verb["share_excluding_undenominated_pct"]
        .as_f64()
        .expect("a share");
    let expect_excluding = rift_recall_ledger::savings::share(
        u64::try_from(AVOIDED_CLAUDE).unwrap(),
        u64::try_from(READ_TOKENS).unwrap(),
    )
    .expect("claude-code both decided and read");
    assert!(
        (excluding - expect_excluding).abs() < 1e-9,
        "{excluding} vs {expect_excluding}"
    );
    assert!(
        excluding < plain,
        "dropping a numerator with no denominator can only lower the share: \
         {excluding} vs {plain}"
    );

    // The row itself is published rather than quietly dropped: the reader can
    // see which family was excluded and why.
    let rows = verb["by_family"].as_array().expect("per-family rows");
    let codex = rows
        .iter()
        .find(|r| r["family"] == "codex")
        .expect("codex has a row of its own");
    assert_eq!(codex["read_tokens"], 0);
    assert_eq!(codex["tokens_avoided"], AVOIDED_CODEX);

    // And the status table names it on the same terms.
    let (code, out, err) = rift(&f, &["recall", "status"]);
    assert_eq!(code, 0, "recall status failed: {out}{err}");
    assert!(
        out.contains("codex") && out.contains("decided, no captured reads"),
        "the status table names the excluded family: {out}"
    );
}

/// A family with nothing measured answers "unanswerable", never `0 %`: the two
/// mean opposite things and only one of them is a measurement.
#[test]
fn an_unmeasured_family_publishes_no_share_rather_than_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    rift_git::test_support::fake_repo(&repo).unwrap();
    let f = Fixture {
        repo,
        cache: tmp.path().join("cache"),
        _tmp: tmp,
    };
    let verb = json(&f, &["savings", "--json"]);
    assert!(verb["overall"]["share_pct"].is_null(), "{verb}");
    assert_eq!(canopy_share(&f), None);
}

/// The fixture's own guard: the seeded stores are the family's, under the two
/// distinct id spaces. If this ever resolves to one id, the agreement above
/// would be trivially true over two empty files.
#[test]
fn the_fixture_seeds_the_family_under_both_of_its_id_spaces() {
    let f = fixture();
    let (_, state) = rift_git::family_of(&f.repo, MintPolicy::Allow).unwrap();
    let FamilyState::Minted(family) = state else {
        panic!("minted");
    };
    let recall = rift_store::recall_db_for_family_under(&f.cache, &family);
    let projection = rift_store::projection_db_for_family_under(&f.cache, &family);
    assert!(recall.exists() && projection.exists());
    assert_ne!(
        recall.parent().map(Path::to_path_buf),
        projection.parent().map(Path::to_path_buf),
        "the recall id and the capture id are different id spaces"
    );
}

// --- the savings window ----------------------------------------------------
//
// The share is `avoided ÷ (avoided + read)` **since the reset**, and until the
// boundary marker existed "the reset" could only mean the moment the stores
// were emptied. These are the assertions that the boundary is now a thing a
// person can move, that moving it moves every surface at once, and that
// nothing else about the record changes when they do.

/// What the late half of the window fixture seeds, in tokens.
const LATE_AVOIDED: i64 = 100;
const LATE_READ: i64 = 1_000;

/// The instant the late rows are recorded at, and a boundary before them.
const LATE_MS: i64 = 3_000;
const BOUNDARY_MS: i64 = 2_000;

/// A second read and a second decision, both after [`BOUNDARY_MS`].
///
/// The window tests need a cut with rows on BOTH sides of it: a boundary that
/// only ever produces `n/a` would be satisfied by a surface that had simply
/// stopped answering, which is the opposite of what is being asserted.
fn seed_late_half(f: &Fixture) {
    let (_, state) = rift_git::family_of(&f.repo, MintPolicy::Allow).unwrap();
    let FamilyState::Minted(family) = state else {
        panic!("minted");
    };
    seed_late_decision(f, &family);
    seed_late_read(f, &family);
}

/// The late decision, in both serving relations.
fn seed_late_decision(f: &Fixture, family: &impl FamilyIds) {
    let rt = rt();
    let recall_db = rift_store::recall_db_for_family_under(&f.cache, family);
    let recall = SqliteStore::open(&recall_db).unwrap();
    for (table, id_col, id, extra) in [
        ("memd_audit_event", "audit_id", "late-a", "handle"),
        ("serve_call_event", "call_id", "late-c", "channel"),
    ] {
        let mut cols = vec![id_col];
        let mut vals = vec![Value::text(id)];
        if table == "memd_audit_event" {
            cols.push("verb");
            vals.push(Value::text("memd.read_intercepted"));
            cols.push(extra);
            vals.push(Value::text("h"));
        } else {
            cols.push(extra);
            vals.push(Value::text("hook_inject"));
            cols.push("tool_name");
            vals.push(Value::text("claude-code:read-gate"));
            cols.push("terminal_verb");
            vals.push(Value::text("memd.read_intercepted"));
        }
        cols.extend([
            "tokens_returned",
            "tokens_avoided",
            "recorded_at",
            "schema_version",
        ]);
        vals.extend([
            Value::Int(0),
            Value::Int(LATE_AVOIDED),
            Value::Int(LATE_MS),
            Value::Int(1),
        ]);
        rt.block_on(recall.insert(table, &cols, &vals)).unwrap();
    }
}

/// The late read, through the six rows the measurement query joins.
fn seed_late_read(f: &Fixture, family: &impl FamilyIds) {
    let rt = rt();
    let projection_db = rift_store::projection_db_for_family_under(&f.cache, family);
    let capture = SqliteStore::open(&projection_db).unwrap();
    let insert = |table: &str, cols: &[&str], vals: &[Value]| {
        rt.block_on(capture.insert(table, cols, vals)).unwrap();
    };
    insert(
        "turn",
        &[
            "turn_id",
            "session_id",
            "ordinal",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text("t2"),
            Value::text("s1"),
            Value::Int(1),
            Value::Int(LATE_MS),
            Value::Int(1),
        ],
    );
    insert(
        "tool_call",
        &[
            "tool_call_id",
            "turn_id",
            "tool_name",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text("tc2"),
            Value::text("t2"),
            Value::text("Read"),
            Value::Int(LATE_MS),
            Value::Int(1),
        ],
    );
    insert(
        "file_revision",
        &[
            "file_revision_id",
            "path",
            "blob_oid",
            "content_sha256",
            "schema_version",
        ],
        &[
            Value::text("fr2"),
            Value::text("src/late.rs"),
            Value::text("b2"),
            Value::text("d".repeat(64)),
            Value::Int(1),
        ],
    );
    insert(
        "tool_reads_file",
        &["tool_call_id", "file_revision_id"],
        &[Value::text("tc2"), Value::text("fr2")],
    );
    insert(
        "file_read_tokens",
        &[
            "tool_call_id",
            "input_tokens",
            "tokenizer",
            "schema_version",
        ],
        &[
            Value::text("tc2"),
            Value::Int(LATE_READ),
            Value::text(rift_tokens::TOKENIZER),
            Value::Int(1),
        ],
    );
}

/// The family's common directory — where the marker lives, and the one path
/// every member of the family shares.
fn common_dir(repo: &Path) -> PathBuf {
    let (_, state) = rift_git::family_of(repo, MintPolicy::Allow).unwrap();
    let FamilyState::Minted(family) = state else {
        panic!("minted");
    };
    family.common_dir
}

/// The window the dashboard's card says it is reporting, when one was set.
fn canopy_window(f: &Fixture) -> Option<String> {
    let update = rift_canopy_data::feeds::compute_recall(
        &f.cache,
        &f.repo.to_string_lossy(),
        2_000,
        rift_canopy::RecallWindow::AllTime,
    );
    let rift_canopy::FeedPayload::Recall { savings, .. } = update.payload else {
        panic!("expected the Recall feed");
    };
    let rift_canopy::RecallView::Data(sd) = savings else {
        return None;
    };
    sd.share.window
}

/// The share every surface publishes, as the four of them see it.
fn shares(f: &Fixture) -> (Option<f64>, Option<f64>, Option<f64>) {
    let verb = json(f, &["savings", "--json"])["overall"]["share_pct"].as_f64();
    let status =
        json(f, &["recall", "status", "--json"])["savings"]["overall"]["share_pct"].as_f64();
    (verb, status, canopy_share(f))
}

/// **The one number stays one number when the window moves.**
///
/// A boundary that reached the verb but not the card, or the card but not the
/// status table, would put two different periods on the screen under one
/// name — which is the whole defect this file exists to catch, in its newest
/// form.
#[test]
fn a_reset_moves_the_window_and_every_surface_follows() {
    let f = fixture();
    seed_late_half(&f);

    // Before: everything the record holds.
    let whole = rift_recall_ledger::savings::share(
        u64::try_from(AVOIDED_CLAUDE + AVOIDED_CODEX + LATE_AVOIDED).unwrap(),
        u64::try_from(READ_TOKENS + LATE_READ).unwrap(),
    );
    assert_eq!(shares(&f), (whole, whole, whole), "before the reset");

    // Move the boundary to between the two halves.
    let (code, out, err) = rift(&f, &["savings", "reset", "--at", &BOUNDARY_MS.to_string()]);
    assert_eq!(code, 0, "reset failed: {out}{err}");
    assert!(out.contains("savings window moved"), "{out}");
    assert!(
        rift_recall_ledger::savings_window::marker_path(&common_dir(&f.repo)).is_file(),
        "the marker is a file in the family's common dir"
    );

    // After: the late half alone, on all three share surfaces.
    let late = rift_recall_ledger::savings::share(
        u64::try_from(LATE_AVOIDED).unwrap(),
        u64::try_from(LATE_READ).unwrap(),
    );
    assert_ne!(late, whole, "the fixture's two windows differ");
    assert_eq!(shares(&f), (late, late, late), "after the reset");

    // And every one of them says which boundary it used.
    let verb = json(&f, &["savings", "--json"]);
    assert_eq!(verb["window"]["boundary_ms"], BOUNDARY_MS);
    assert_eq!(verb["window"]["marker_set_by"], "rift savings reset");
    assert!(
        verb["window"]["boundary_source"]
            .as_str()
            .unwrap()
            .contains("boundary marker"),
        "{verb}"
    );
    let (_, text, _) = rift(&f, &["recall", "status"]);
    assert!(text.contains("boundary marker"), "{text}");
    // The card captions its own number rather than leaving the old sentence
    // ("since the projection was reset") standing over a moved window.
    assert_eq!(
        canopy_window(&f).as_deref(),
        Some("since the window was reset 1970-01-01T00:00:02.000Z"),
        "the card names the window it is reporting"
    );

    // A second reset moves it on again, and names the one it replaced.
    let (code, out, err) = rift(&f, &["savings", "reset"]);
    assert_eq!(code, 0, "the second reset failed: {out}{err}");
    assert!(out.contains("previous:              "), "{out}");
    // Now the window starts after every seeded row: unanswerable, never 0 %.
    assert_eq!(shares(&f), (None, None, None), "past the last row");
    let (_, out, _) = rift(&f, &["savings"]);
    assert!(out.contains("n/a") && !out.contains("0.0%"), "{out}");
}

/// `--since` answers one question and changes nothing.
#[test]
fn since_overrides_for_one_call_and_writes_nothing() {
    let f = fixture();
    seed_late_half(&f);
    let marker = rift_recall_ledger::savings_window::marker_path(&common_dir(&f.repo));

    let late = rift_recall_ledger::savings::share(
        u64::try_from(LATE_AVOIDED).unwrap(),
        u64::try_from(LATE_READ).unwrap(),
    );
    let verb = json(
        &f,
        &["savings", "--json", "--since", &BOUNDARY_MS.to_string()],
    );
    assert_eq!(verb["overall"]["share_pct"].as_f64(), late);
    assert_eq!(verb["window"]["boundary_ms"], BOUNDARY_MS);
    assert!(
        verb["window"]["marker_set_by"].is_null(),
        "no marker was involved"
    );
    assert!(
        verb["window"]["boundary_source"]
            .as_str()
            .unwrap()
            .contains("nothing was written"),
        "{verb}"
    );
    assert!(!marker.exists(), "an override writes no marker");

    // The unqualified figure is untouched by the question having been asked.
    let whole = rift_recall_ledger::savings::share(
        u64::try_from(AVOIDED_CLAUDE + AVOIDED_CODEX + LATE_AVOIDED).unwrap(),
        u64::try_from(READ_TOKENS + LATE_READ).unwrap(),
    );
    assert_eq!(shares(&f), (whole, whole, whole));

    // RFC3339 is the other spelling, and it means the same instant.
    let stamp = rift_recall_ledger::savings_window::format_rfc3339_ms(BOUNDARY_MS);
    let same = json(&f, &["savings", "--json", "--since", &stamp]);
    assert_eq!(same["window"]["boundary_ms"], BOUNDARY_MS);
    assert_eq!(same["overall"]["share_pct"].as_f64(), late);

    // With a marker in place, the override wins for the one call and leaves it.
    rift(&f, &["savings", "reset", "--at", &BOUNDARY_MS.to_string()]);
    let widened = json(&f, &["savings", "--json", "--since", "0"]);
    assert_eq!(widened["overall"]["share_pct"].as_f64(), whole);
    let after = rift_recall_ledger::savings_window::read_marker(&common_dir(&f.repo)).unwrap();
    assert_eq!(after.boundary_ms, BOUNDARY_MS, "the marker is where it was");
}

/// `--last` answers over the dashboard's own periods, writes nothing, and
/// composes with the marker instead of replacing it.
///
/// The fixture's rows are recorded at the epoch, so every trailing window is
/// deliberately empty here — which is the honest answer and the deterministic
/// one. What this pins is the *window*: which period was used, that it was
/// spelled by the card's own vocabulary, and that asking left the family's
/// boundary exactly where it was.
#[test]
fn last_reads_a_trailing_window_and_writes_nothing() {
    let f = fixture();
    let marker = rift_recall_ledger::savings_window::marker_path(&common_dir(&f.repo));

    let seven = json(&f, &["savings", "--json", "--last", "7d"]);
    let source = seven["window"]["boundary_source"].as_str().unwrap_or("");
    assert!(source.contains("trailing 7d"), "{seven}");
    assert!(source.contains("nothing was written"), "{seven}");
    assert!(
        seven["window"]["marker_set_by"].is_null(),
        "no marker was involved: {seven}"
    );
    assert!(!marker.exists(), "a trailing window writes no marker");
    // Epoch-recorded rows are outside every trailing window: the share has no
    // answer rather than a zero, and the boundary is a real instant.
    assert!(seven["overall"]["share_pct"].is_null(), "{seven}");
    assert!(
        seven["window"]["boundary_ms"].as_i64().unwrap_or(0) > 0,
        "{seven}"
    );

    // `all` is the family's own window — the default, spelled — so it answers
    // exactly what no flag answers.
    let all = json(&f, &["savings", "--json", "--last", "all"]);
    let bare = json(&f, &["savings", "--json"]);
    assert_eq!(all["overall"], bare["overall"]);
    assert_eq!(all["window"]["boundary_ms"], bare["window"]["boundary_ms"]);

    // Composition, both ways round. An OLD marker (this fixture's, at the
    // epoch) is earlier than any trailing window, so the window's own cut
    // stands…
    rift(&f, &["savings", "reset", "--at", &BOUNDARY_MS.to_string()]);
    let thirty = json(&f, &["savings", "--json", "--last", "30d"]);
    assert!(
        thirty["window"]["boundary_ms"].as_i64().unwrap_or(0) > BOUNDARY_MS,
        "a trailing window later than the marker keeps its own cut: {thirty}"
    );
    // …and a marker set NOW is later than a thirty-day window, so the marker
    // wins and the sentence says which of the two did. A period button can
    // never draw a row `rift savings` refuses to count.
    rift(&f, &["savings", "reset"]);
    let moved = rift_recall_ledger::savings_window::read_marker(&common_dir(&f.repo))
        .expect("the reset wrote a marker");
    let thirty = json(&f, &["savings", "--json", "--last", "30d"]);
    assert_eq!(
        thirty["window"]["boundary_ms"].as_i64(),
        Some(moved.boundary_ms)
    );
    assert!(
        thirty["window"]["boundary_source"]
            .as_str()
            .unwrap_or("")
            .contains("boundary marker"),
        "{thirty}"
    );

    // A spelling that is not one of the card's four is refused, naming them,
    // and the two window flags are mutually exclusive.
    let (code, out, err) = rift(&f, &["savings", "--last", "1w"]);
    assert_ne!(code, 0, "{out}");
    assert!(
        format!("{out}{err}").contains("24h, 7d, 30d, or all"),
        "{out}{err}"
    );
    let (code, out, err) = rift(&f, &["savings", "--last", "7d", "--since", "0"]);
    assert_ne!(code, 0, "one window per call: {out}{err}");
}

/// A window that starts in the future would exclude the very sessions it was
/// opened to measure. Refused, on both spellings, writing nothing.
#[test]
fn a_future_boundary_is_refused() {
    let f = fixture();
    let marker = rift_recall_ledger::savings_window::marker_path(&common_dir(&f.repo));
    let future = "2099-01-01T00:00:00Z";

    let (code, out, err) = rift(&f, &["savings", "reset", "--at", future]);
    assert_ne!(code, 0, "a future boundary exits non-zero: {out}");
    assert!(
        format!("{out}{err}").contains("cannot start in the future"),
        "{out}{err}"
    );
    assert!(!marker.exists(), "a refused reset writes nothing");

    // Nonsense is refused as nonsense rather than guessed at.
    let (code, out, err) = rift(&f, &["savings", "reset", "--at", "yesterday"]);
    assert_ne!(code, 0, "{out}");
    assert!(
        format!("{out}{err}").contains("is not an instant"),
        "{out}{err}"
    );
    assert!(!marker.exists());

    let (code, out, err) = rift(&f, &["savings", "--since", "the-other-day"]);
    assert_ne!(code, 0, "{out}");
    assert!(
        format!("{out}{err}").contains("is not an instant"),
        "{out}{err}"
    );

    // And `--since` is not a way to move the boundary by accident.
    let (code, out, err) = rift(&f, &["savings", "--since", "0", "reset"]);
    assert_ne!(code, 0, "{out}{err}");
    assert!(!marker.exists());
}

/// **One family, one window.** A linked worktree is a member of its
/// repository, not a repository of its own: the marker lives in the common
/// directory both members share, so a subagent working in a temporary
/// checkout measures the period the person set — and a boundary set from
/// inside that checkout is the family's.
#[test]
fn a_marker_from_another_member_of_the_family_is_the_same_marker() {
    let f = fixture();
    seed_late_half(&f);
    let member = f.repo.parent().unwrap().join("member");
    rift_git::test_support::fake_worktree(&f.repo.join(".git"), &member, "member").unwrap();
    let from_member = Fixture {
        _tmp: tempfile::tempdir().unwrap(),
        repo: member.clone(),
        cache: f.cache.clone(),
    };

    // Set the window from the member.
    let (code, out, err) = rift(
        &from_member,
        &["savings", "reset", "--at", &BOUNDARY_MS.to_string()],
    );
    assert_eq!(code, 0, "reset from the member failed: {out}{err}");

    // One marker, in the family's common dir — not one per checkout.
    let marker = rift_recall_ledger::savings_window::marker_path(&common_dir(&f.repo));
    assert!(marker.is_file());
    assert!(
        !member.join(".git").is_dir(),
        "the member's .git is a file, so it has no directory of its own to hide a marker in"
    );
    assert_eq!(
        rift_recall_ledger::savings_window::read_marker(&common_dir(&member))
            .map(|m| m.boundary_ms),
        Some(BOUNDARY_MS),
        "both members resolve the same common dir, so both read one marker"
    );

    // And both members publish the same windowed figure.
    let late = rift_recall_ledger::savings::share(
        u64::try_from(LATE_AVOIDED).unwrap(),
        u64::try_from(LATE_READ).unwrap(),
    );
    assert_eq!(shares(&f), (late, late, late), "the main work tree");
    assert_eq!(
        json(&from_member, &["savings", "--json"])["overall"]["share_pct"].as_f64(),
        late,
        "the member"
    );
}

/// Reading a measurement must not change it, and moving the window must not
/// touch the record: the marker is the only byte any of this writes.
#[test]
fn the_window_verbs_write_only_the_marker() {
    let f = fixture();
    seed_late_half(&f);
    let (_, state) = rift_git::family_of(&f.repo, MintPolicy::Allow).unwrap();
    let FamilyState::Minted(family) = state else {
        panic!("minted");
    };
    let stores = [
        rift_store::recall_db_for_family_under(&f.cache, &family),
        rift_store::projection_db_for_family_under(&f.cache, &family),
    ];
    let stat = || -> Vec<(u64, std::time::SystemTime)> {
        stores
            .iter()
            .map(|p| {
                let m = std::fs::metadata(p).unwrap();
                (m.len(), m.modified().unwrap())
            })
            .collect()
    };
    let before = stat();
    for args in [
        vec!["savings"],
        vec!["savings", "--json"],
        vec!["savings", "--since", "0"],
        vec!["savings", "reset", "--at", &BOUNDARY_MS.to_string()],
        vec!["savings"],
    ] {
        let (code, out, err) = rift(&f, &args);
        assert_eq!(code, 0, "rift {args:?}: {out}{err}");
    }
    assert_eq!(before, stat(), "neither projection was written");
    for p in &stores {
        for suffix in ["-wal", "-shm"] {
            let side = p.with_file_name(format!(
                "{}{suffix}",
                p.file_name().unwrap().to_string_lossy()
            ));
            assert!(!side.exists(), "{} was left behind", side.display());
        }
    }
}

/// **A prevented read is inside the headline and separable from it, on every
/// surface.** The owner's ruling is that a read Rift caused not to happen is a
/// saving like any other; the honesty rule is that it must never read as a
/// second, additional one. So it enters `tokens_avoided` — the share moves —
/// and it is reported beside it under its own name, per family and overall.
#[test]
fn a_prevented_row_enters_avoided_under_its_own_tag_on_every_surface() {
    const PREVENTED: i64 = 500;
    let f = fixture_with(PREVENTED);
    let avoided = AVOIDED_CLAUDE + AVOIDED_CODEX + PREVENTED;
    let expected = rift_recall_ledger::savings::share(
        u64::try_from(avoided).unwrap(),
        u64::try_from(READ_TOKENS).unwrap(),
    )
    .expect("the fixture read and avoided tokens");

    let verb = json(&f, &["savings", "--json"]);
    assert_eq!(
        verb["overall"]["tokens_avoided"], avoided,
        "the prevented read is inside the headline"
    );
    assert_eq!(
        verb["overall"]["tokens_prevented"], PREVENTED,
        "and separable from it"
    );
    let verb_share = verb["overall"]["share_pct"].as_f64().expect("a share");
    assert!(
        (verb_share - expected).abs() < 1e-9,
        "the verb: {verb_share} vs {expected}"
    );

    // Per family, from the relation that names one.
    let rows = verb["by_family"].as_array().expect("per-family rows");
    let cc = rows
        .iter()
        .find(|r| r["family"] == "claude-code")
        .expect("the family that prevented a read");
    assert_eq!(cc["tokens_prevented"], PREVENTED);
    assert_eq!(cc["tokens_avoided"], AVOIDED_CLAUDE + PREVENTED);
    let codex = rows
        .iter()
        .find(|r| r["family"] == "codex")
        .expect("the other deciding family");
    assert_eq!(codex["tokens_prevented"], 0, "it prevented nothing");

    // The rendered report says it in words, under its own line.
    let (code, out, err) = rift(&f, &["savings"]);
    assert_eq!(code, 0, "savings failed: {out}{err}");
    assert!(
        out.contains(&format!("of it prevented:       {PREVENTED}")),
        "the figure is published beside the total it is part of: {out}"
    );

    // And the one number is still one number: the status table and the card
    // read the same computation, prevented rows included.
    let status = json(&f, &["recall", "status", "--json"]);
    let status_share = status["savings"]["overall"]["share_pct"]
        .as_f64()
        .expect("a share");
    assert!(
        (status_share - verb_share).abs() < f64::EPSILON,
        "status: {status_share} vs {verb_share}"
    );
    let canopy = canopy_share(&f).expect("a share on the card");
    assert!(
        (canopy - verb_share).abs() < f64::EPSILON,
        "the card: {canopy} vs {verb_share}"
    );
}
