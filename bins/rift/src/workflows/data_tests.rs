//! **The data layer of `rift dispatch` / `rift recap`, executed.**
//!
//! # Why this file exists
//!
//! Every test that shipped with the two verbs drove `Window::resolve`, the four
//! renderers and the pack compiler over **hand-built** [`WindowFacts`]. The
//! eleven queries in [`super::gather`] (whose query core now lives in
//! [`rift_store::window`]), [`super::tool_mix`] and [`super::serve_calls`] —
//! ten tables across two databases — were executed by nothing. So a window filter that compared
//! against a sentinel and matched no row anywhere passed a suite of 2,375
//! tests, and answered `checkpoints: 0`, `files: []` and an all-zero
//! attribution split on a repository holding 103 checkpoints and 178
//! attribution rows.
//!
//! A test that never touches the surface cannot fail on it. This file touches
//! it: one seeded projection with **known** rows, and assertions on the real
//! numbers those rows must produce — counts, splits, sums, savings, the
//! bitemporal cost predicate, and the sentinel rule the module docs state.
//!
//! # The fixture, in one paragraph
//!
//! Three sessions, three checkpoints — one with a **recorded** time, one whose
//! time must be **derived** from its session, one **unresolvable** — two files
//! (one of them policy-sensitive), two model calls with four cost facts (two of
//! which the bitemporal predicate must reject), six tool calls over three names
//! and three recall-ledger events (one of them injected spend). Every number
//! asserted below is derivable by hand from that paragraph.

use std::path::Path;

use rift_store::{CanonicalStore, SqliteStore, Value};

use super::{dispatch_in, gather, recap_in, Planes, Window, WindowFacts};

/// The unknown-time sentinel, spelled through `rift-shadow`'s re-export.
///
/// It is DECLARED in [`rift_store::window`] now — the crate that owns the
/// column — and reaching it through `rift_shadow` here is deliberate: it keeps
/// the re-export a path something actually compiles, so removing it fails a
/// build instead of quietly stranding every existing caller.
const UNKNOWN: i64 = rift_shadow::CREATED_AT_UNKNOWN;

/// Schema version stamped on every seeded row (the column is `NOT NULL`).
const V: i64 = 1;

fn t(s: &str) -> Value {
    Value::Text(s.to_owned())
}

fn i(n: i64) -> Value {
    Value::Int(n)
}

/// A current-thread runtime — no test here reads a clock, a network or an RNG.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
}

/// The seeded capture projection, one relation group at a time in the order
/// the fixture paragraph in the module docs names them.
async fn capture() -> SqliteStore {
    let s = migrated_projection();
    seed_sessions(&s).await;
    // --- checkpoints: recorded / derivable / unresolvable --------------------
    for (id, created) in [("c1", 2_000), ("c2", UNKNOWN), ("c3", UNKNOWN)] {
        seed_checkpoint(&s, id, created).await;
    }
    seed_session_links(&s).await;
    seed_files(&s).await;
    seed_model_calls(&s).await;
    seed_tool_calls(&s).await;
    s
}

/// An empty in-memory capture projection at the current schema.
fn migrated_projection() -> SqliteStore {
    let s = SqliteStore::open_in_memory().expect("store");
    rift_store::apply_migrations(&s, &rift_store::default_migrations()).expect("migrations");
    s
}

/// A projection holding exactly `checkpoints`, each `(id, created_at)`.
async fn checkpoint_projection(checkpoints: &[(&str, i64)]) -> SqliteStore {
    let s = migrated_projection();
    for (id, created) in checkpoints {
        seed_checkpoint(&s, id, *created).await;
    }
    s
}

/// One `checkpoint` row with the fixture's fixed strategy and root.
async fn seed_checkpoint(s: &SqliteStore, id: &str, created: i64) {
    s.insert(
        "checkpoint",
        &[
            "checkpoint_id",
            "strategy",
            "created_at",
            "merkle_root",
            "schema_version",
        ],
        &[t(id), t("manual-commit"), i(created), t("00"), i(V)],
    )
    .await
    .expect("checkpoint");
}

/// Sessions: two agents, two models, one of them ended.
async fn seed_sessions(s: &SqliteStore) {
    for (id, agent, model, started) in [
        ("s1", "claude-code", "opus", 1_000),
        ("s2", "codex", "sonnet", 5_000),
        ("s3", "claude-code", "opus", 9_000),
    ] {
        s.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[t(id), t(agent), t(model), t("repo"), i(started), i(V)],
        )
        .await
        .expect("session");
    }
    s.insert(
        "session_event",
        &[
            "event_id",
            "session_id",
            "kind",
            "occurred_at",
            "schema_version",
        ],
        &[t("e1"), t("s1"), t("end"), i(1_500), i(V)],
    )
    .await
    .expect("session_event");
}

/// Which session each checkpoint archives.
///
/// c1 -> s1 (started 1_000; c1 records 2_000 and the record wins);
/// c2 -> s2 (started 5_000; c2 records nothing, so 5_000 is its time);
/// c3 -> nothing: no time exists for it anywhere.
async fn seed_session_links(s: &SqliteStore) {
    for (cp, sess, ord) in [("c1", "s1", 0), ("c2", "s2", 0)] {
        s.insert(
            "session_in_checkpoint",
            &["checkpoint_id", "session_id", "ordinal", "schema_version"],
            &[t(cp), t(sess), i(ord), i(V)],
        )
        .await
        .expect("session_in_checkpoint");
    }
}

/// Files and their attribution.
async fn seed_files(s: &SqliteStore) {
    for (id, path) in [("fr1", "src/a.rs"), ("fr2", ".env")] {
        s.insert(
            "file_revision",
            &[
                "file_revision_id",
                "path",
                "blob_oid",
                "content_sha256",
                "schema_version",
            ],
            &[t(id), t(path), t(id), t("aa"), i(V)],
        )
        .await
        .expect("file_revision");
    }
    for (cp, fr, ai, hu, mx, rm, conf) in [
        ("c1", "fr1", 10, 2, 1, 0, 900),
        ("c2", "fr1", 20, 3, 2, 1, 700),
        ("c2", "fr2", 5, 0, 0, 0, 500),
        // On the UNRESOLVABLE checkpoint: it must never reach a bounded
        // window's rollup, and its file must never reach a bounded window's
        // list — the "excluded" half of the sentinel rule, asserted rather
        // than assumed.
        ("c3", "fr2", 9_999, 0, 0, 0, 100),
    ] {
        s.insert(
            "attribution",
            &[
                "checkpoint_id",
                "file_revision_id",
                "agent_added",
                "human_added",
                "human_modified_agent",
                "agent_removed_by_human",
                "confidence_milli",
                "method_version",
                "schema_version",
            ],
            &[
                t(cp),
                t(fr),
                i(ai),
                i(hu),
                i(mx),
                i(rm),
                i(conf),
                t("v1"),
                i(V),
            ],
        )
        .await
        .expect("attribution");
    }
}

/// Model calls, their tokens, and four cost facts.
async fn seed_model_calls(s: &SqliteStore) {
    for (call, model, started) in [("mc1", "opus", 1_500), ("mc2", "sonnet", 6_000)] {
        s.insert(
            "model_call",
            &[
                "call_id",
                "turn_id",
                "model",
                "started_at",
                "schema_version",
            ],
            &[t(call), t("turn"), t(model), i(started), i(V)],
        )
        .await
        .expect("model_call");
    }
    for (call, un, ca, cw, out) in [("mc1", 100, 200, 30, 50), ("mc2", 7, 8, 9, 10)] {
        s.insert(
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
            &[t(call), i(un), i(ca), i(cw), i(out), t("exact"), i(V)],
        )
        .await
        .expect("token_usage");
    }
    seed_cost_facts(s).await;
}

/// `cf_old` is superseded, `cf_closed` is no longer believed: the two halves
/// of the bitemporal predicate, each with a row that only IT rejects.
async fn seed_cost_facts(s: &SqliteStore) {
    for (id, call, micros, valid_to, supersedes) in [
        ("cf_old", "mc1", 5_000, None, None),
        ("cf_new", "mc1", 1_000, None, Some("cf_old")),
        ("cf_closed", "mc2", 777, Some(9_999), None),
        ("cf_live", "mc2", 250, None, None),
    ] {
        s.insert(
            "cost_fact",
            &[
                "cost_fact_id",
                "call_id",
                "amount_micros",
                "valid_from",
                "valid_to",
                "recorded_at",
                "supersedes_id",
                "schema_version",
            ],
            &[
                t(id),
                t(call),
                i(micros),
                i(0),
                valid_to.map_or(Value::Null, i),
                i(0),
                supersedes.map_or(Value::Null, t),
                i(V),
            ],
        )
        .await
        .expect("cost_fact");
    }
}

/// Tool calls: three names, six calls.
async fn seed_tool_calls(s: &SqliteStore) {
    for (id, name, started) in [
        ("t1", "Read", 1_200),
        ("t2", "Read", 1_300),
        ("t3", "Read", 6_100),
        ("t4", "Bash", 1_400),
        ("t5", "Bash", 6_200),
        ("t6", "Edit", 6_300),
    ] {
        s.insert(
            "tool_call",
            &[
                "tool_call_id",
                "turn_id",
                "tool_name",
                "started_at",
                "schema_version",
            ],
            &[t(id), t("turn"), t(name), i(started), i(V)],
        )
        .await
        .expect("tool_call");
    }
}

/// The seeded recall ledger, written to `dir/recall.db`.
async fn recall(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("recall.db");
    let s = rift_store::open_migrated(&db, &rift_store::recall_migrations()).expect("recall db");
    for (id, verb, returned, avoided, at) in [
        ("a1", "memd.manifest_served", 120, 900, 1_600),
        ("a2", "memd.manifest_served", 30, 100, 6_200),
        // Injected spend is NEVER credit — it is subtracted.
        ("a3", "memd.manifest_injected", 40, 0, 1_700),
    ] {
        s.insert(
            "memd_audit_event",
            &[
                "audit_id",
                "verb",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[t(id), t(verb), i(returned), i(avoided), i(at), i(V)],
        )
        .await
        .expect("memd_audit_event");
    }
    drop(s);
    db
}

/// A helper that runs `body` against the seeded planes.
fn with_planes<T>(body: impl FnOnce(&tokio::runtime::Runtime, &Planes<'_>) -> T) -> T {
    let rt = rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let store = rt.block_on(capture());
    let db = rt.block_on(recall(dir.path()));
    let planes = Planes {
        capture: &store,
        root: dir.path(),
        recall_db: Some(db),
    };
    body(&rt, &planes)
}

/// **The window that excludes nothing.** All three checkpoints, including the
/// one whose time cannot be resolved — the rule the module docs state, from the
/// only side that can observe it.
#[test]
fn the_unbounded_window_holds_every_checkpoint_including_the_undatable_one() {
    with_planes(|rt, planes| {
        let w = Window::resolve(None, None).unwrap();
        let f = rt.block_on(gather(planes, &w)).unwrap();

        let ids: Vec<&str> = f
            .checkpoints
            .iter()
            .map(|c| c.checkpoint_id.as_str())
            .collect();
        assert_eq!(ids, ["c3", "c1", "c2"], "unresolved sorts first, then time");
        assert_eq!(f.checkpoints.len(), 3);
        assert_eq!(f.unresolved_checkpoints, 1);

        let by_id = |id: &str| {
            f.checkpoints
                .iter()
                .find(|c| c.checkpoint_id == id)
                .expect(id)
                .clone()
        };
        assert_eq!(by_id("c1").at, Some(2_000), "a recorded time is used");
        assert!(!by_id("c1").derived);
        assert_eq!(
            by_id("c2").at,
            Some(5_000),
            "an unrecorded time comes from the session it archives"
        );
        assert!(by_id("c2").derived, "and it is reported as derived");
        assert_eq!(by_id("c3").at, None);
        assert!(!by_id("c3").derived);
    });
}

/// **The number the held pair got wrong.** A bounded window over a projection
/// whose checkpoints record no time of their own must still report them — this
/// is the assertion that would have failed on the real repository.
#[test]
fn a_bounded_window_reports_the_checkpoints_whose_time_is_derived() {
    with_planes(|rt, planes| {
        // 4_000..9_000 contains only c2, and c2 records NO time: it is in the
        // window solely because its session anchors it at 5_000.
        let w = Window::resolve(Some("4000"), Some("9000")).unwrap();
        let f = rt.block_on(gather(planes, &w)).unwrap();
        assert_eq!(f.checkpoints.len(), 1, "got: {:?}", f.checkpoints);
        assert_eq!(f.checkpoints.first().expect("c2").checkpoint_id, "c2");
        assert!(f.checkpoints.first().expect("c2").derived);
        assert_eq!(f.unresolved_checkpoints, 1, "and c3 is still counted");

        // c2's two attribution rows, and only those.
        assert_eq!(
            f.files,
            vec![(".env".to_owned(), 1), ("src/a.rs".to_owned(), 1)]
        );
        assert_eq!(f.attribution.agent_lines, 25);
        assert_eq!(f.attribution.human_lines, 3);
        assert_eq!(f.attribution.mixed_lines, 2);
        assert_eq!(f.attribution.agent_removed_by_human, 1);
        assert_eq!(f.attribution.confidence_milli, Some(600), "(700+500)/2");
        // The policy plane, executed: `.env` matches the non-droppable
        // built-in `**/.env*`, and `src/a.rs` does not.
        //
        // **This is also the reachability guard for the crate split.** The
        // query core lives in `rift-store` and answers `None` here on purpose;
        // `Some` can only appear because `workflows::gather` applied policy
        // afterwards. Drop that line from the wrapper and this assertion is
        // what reddens — `rift-store`'s own
        // `gather_leaves_the_policy_answer_unasked` proves the other half.
        assert_eq!(f.sensitive_paths, Some(vec![".env".to_owned()]));
    });
}

/// The whole-window rollup: every sum, every join, every predicate, against
/// numbers derivable by hand from the fixture paragraph.
#[test]
fn the_rollups_are_the_sums_the_seeded_rows_imply() {
    with_planes(|rt, planes| {
        let w = Window::resolve(Some("0"), Some("100000")).unwrap();
        let f = rt.block_on(gather(planes, &w)).unwrap();

        // Both datable checkpoints, and only those.
        assert_eq!(f.checkpoints.len(), 2);
        assert_eq!(f.unresolved_checkpoints, 1);

        // Attribution over c1 + c2 — c3's 9_999 agent lines must NOT appear.
        assert_eq!(f.attribution.agent_lines, 35);
        assert_eq!(f.attribution.human_lines, 5);
        assert_eq!(f.attribution.mixed_lines, 3);
        assert_eq!(f.attribution.confidence_milli, Some(700), "(900+700+500)/3");

        // Files, ranked by how many checkpoints touched them.
        assert_eq!(
            f.files,
            vec![("src/a.rs".to_owned(), 2), (".env".to_owned(), 1)]
        );

        // Sessions and the open set.
        assert_eq!(f.sessions.len(), 3);
        assert_eq!(f.open_sessions, vec!["s2".to_owned(), "s3".to_owned()]);

        // Tokens, by class.
        assert_eq!(f.tokens.uncached_input, 107);
        assert_eq!(f.tokens.cached_input, 208);
        assert_eq!(f.tokens.cache_creation, 39);
        assert_eq!(f.tokens.output, 60);

        // Cost: `cf_new` (1_000) + `cf_live` (250). `cf_old` is superseded and
        // `cf_closed` is no longer believed; either one leaking in is a
        // double-count of a re-priced call.
        assert_eq!(f.cost_micros, 1_250);

        // Recall's ledger: injected spend is separated, never credited.
        let s = f.savings.expect("the fixture writes a ledger");
        assert_eq!(s.avoided_upper_bound, 1_000);
        assert_eq!(s.returned, 150);
        assert_eq!(s.injected, 40);
    });
}

/// A repository with no recall ledger reports savings as **absent**, and the
/// report does not create the database as a side effect of being read.
#[test]
fn a_repository_without_a_ledger_reports_absent_and_creates_nothing() {
    let rt = rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let store = rt.block_on(capture());
    let planes = Planes {
        capture: &store,
        root: dir.path(),
        recall_db: None,
    };
    let w = Window::resolve(None, None).unwrap();
    let f = rt.block_on(gather(&planes, &w)).unwrap();
    assert!(f.savings.is_none());
    assert!(!dir.path().join("recall.db").exists());
}

/// `rift dispatch --json` over the seeded planes: the document a consumer
/// parses, with the real numbers in it.
#[test]
fn the_dispatch_document_carries_the_seeded_numbers() {
    with_planes(|rt, planes| {
        let out = rt
            .block_on(dispatch_in(
                planes,
                Some("0"),
                Some("100000"),
                None,
                false,
                false,
            ))
            .unwrap();
        let d = &out.json;
        // **2, not 1.** The path-to-ordinal change changed this document's SHAPE — `files[]`
        // lost `path` and gained `evidence`/`sensitive`, and
        // `risk.sensitive_paths_touched` kept its key while changing from
        // `array<string>` to `integer`. A kept key with a changed type at an
        // unchanged version type-confuses a pinned consumer without ever
        // failing loudly, which is strictly worse than the rename the key was
        // kept to avoid.
        assert_eq!(d["schema_version"], 2);
        assert_eq!(d["schema_version"], super::DISPATCH_SCHEMA_VERSION);
        assert_eq!(d["checkpoints"]["count"], 2);
        assert_eq!(d["checkpoints"]["recorded_time"], 1);
        assert_eq!(d["checkpoints"]["derived_time"], 1);
        assert_eq!(d["checkpoints"]["unresolved_time"], 1);
        assert_eq!(d["attribution"]["ai_lines"], 35);
        assert_eq!(d["attribution"]["confidence_milli"], 700);
        assert_eq!(d["tokens"]["total"], 414);
        assert_eq!(d["cost"]["amount_micros"], 1_250);
        assert_eq!(d["recall"]["tokens_avoided_estimated_upper_bound"], 1_000);
        assert_eq!(d["recall"]["net_estimated_upper_bound"], 960);
        // **THE JSON NAMES NO PATH.** `facts_json` is agent-facing
        // by enumeration and the `--for-agent` flag cannot be the boundary — an
        // agent acting on injected instructions simply omits it — so the
        // document carries counts and ordinals, and the ordinal->path map
        // renders on the human surface below.
        assert_eq!(d["risk"]["sensitive_paths_touched"], 1);
        assert_eq!(d["risk"]["sensitive_paths_evidence"][0], 2);
        assert_eq!(d["files"][0]["evidence"], 1);
        assert_eq!(d["files"][0]["checkpoints"], 2);
        assert_eq!(d["files"][0]["sensitive"], false);
        assert!(
            !serde_json::to_string(d)
                .unwrap_or_default()
                .contains("src/a.rs"),
            "a path reached the agent-facing document: {d}"
        );

        // The human render says the same thing, and says the derived count out
        // loud rather than passing a lower bound off as a record.
        assert!(out.human.contains("checkpoints 2"), "got: {}", out.human);
        assert!(out.human.contains("timed from their sessions"));
        assert!(out.human.contains("no resolvable time"));
        // …and it is the ONLY surface that resolves an ordinal back to a file.
        assert!(
            out.human.contains("evidence:1") && out.human.contains("src/a.rs"),
            "the human map must resolve the ordinal: {}",
            out.human
        );
        assert!(
            out.human.contains("evidence:2") && out.human.contains(".env"),
            "the sensitive ordinal must resolve too: {}",
            out.human
        );
    });
}

/// `--for-agent` compiles a pack whose volume is the window's real volume.
/// The held pair emitted a 74-token pack whose `Volume{commits:0, files:0}`
/// was the evidence that the data layer was empty.
#[test]
fn the_agent_pack_carries_a_non_empty_volume() {
    with_planes(|rt, planes| {
        let out = rt
            .block_on(dispatch_in(
                planes,
                Some("0"),
                Some("100000"),
                None,
                false,
                true,
            ))
            .unwrap();
        let pack = &out.json["pack"];
        assert_eq!(pack["kind"], "review");
        let wire = serde_json::to_string(pack).unwrap_or_default();
        // The pack's own words, from the sealed constructors: the volume is the
        // window's, the split is the window's, and the sensitive path rides the
        // critical edges.
        assert!(
            wire.contains("scope: 2 commit(s), 3 session(s), 2 file(s)"),
            "got: {wire}"
        );
        assert!(
            out.human.contains("scope: 2 commit(s)"),
            "got: {}",
            out.human
        );
        // The sensitive fact rides as its ordinal; the name does not ride at all.
        assert!(wire.contains("evidence:2"), "got: {wire}");
        assert!(!wire.contains(".env"), "a path reached the pack: {wire}");
        assert!(
            pack["total_tokens"].as_u64().unwrap_or(0) > 74,
            "a pack over real facts is not the empty pack: {pack}"
        );
    });
}

/// `rift recap --json`: the tool histogram, the serve-call split by verb, the
/// per-agent and per-model session split, and the savings rate with its
/// denominator named.
#[test]
fn the_recap_document_carries_the_seeded_analytics() {
    with_planes(|rt, planes| {
        let out = rt
            .block_on(recap_in(planes, Some("0"), Some("100000"), false))
            .unwrap();
        let d = &out.json;
        assert_eq!(d["schema_version"], 2);
        assert_eq!(d["schema_version"], super::RECAP_SCHEMA_VERSION);
        assert_eq!(d["sessions"], 3);
        assert_eq!(d["checkpoints"], 2);
        assert_eq!(d["tokens"]["total"], 414);

        // Ranked by count, then by name — and CITED, because Rift has no closed
        // vocabulary for a tool name and this document is agent-facing
        // (`crate::screen`). The counts are the fact a consumer acts on; the
        // names resolve on the human render, asserted below.
        assert_eq!(d["tool_mix"][0]["tool"], "cite:1");
        assert_eq!(d["tool_mix"][0]["calls"], 3);
        assert_eq!(d["tool_mix"][1]["tool"], "cite:2");
        assert_eq!(d["tool_mix"][1]["calls"], 2);
        assert_eq!(d["tool_mix"][2]["tool"], "cite:3");
        assert!(
            !serde_json::to_string(d)
                .unwrap_or_default()
                .contains("Read"),
            "a tool name reached the agent-facing document: {d}"
        );

        // A pull and a push are two verbs and are never one "serves" number.
        // The COUNTS are what says so, and they are untouched by the screen.
        assert_eq!(d["serve_calls"][0]["verb"], "cite:4");
        assert_eq!(d["serve_calls"][0]["calls"], 1);
        assert_eq!(d["serve_calls"][1]["verb"], "cite:5");
        assert_eq!(d["serve_calls"][1]["calls"], 2);

        // **The agent axis is NOT cited**, and that is the discriminating half
        // of the screen rather than an oversight: `claude-code` and `codex` are
        // registered adapter tokens, a closed set, so they ride verbatim.
        assert_eq!(d["split"]["by"], "agent");
        assert_eq!(d["split"]["counts"][0]["key"], "claude-code");
        assert_eq!(d["split"]["counts"][0]["sessions"], 2);
        assert_eq!(d["split"]["counts"][1]["key"], "codex");

        // 1_000 avoided over (414 spent + 1_000 avoided) = 70.7%.
        assert_eq!(d["savings"]["rate_percent_of_avoided_plus_spent"], 70.7);

        // The human render resolves every citation the document carries, which
        // is the whole reason citing is not erasing.
        for (ordinal, text) in [
            ("cite:1", "Read"),
            ("cite:4", "memd.manifest_injected"),
            ("cite:5", "memd.manifest_served"),
        ] {
            assert!(
                out.human.contains(ordinal) && out.human.contains(text),
                "the human recap must resolve {ordinal} -> {text}: {}",
                out.human
            );
        }

        let by_model = rt
            .block_on(recap_in(planes, Some("0"), Some("100000"), true))
            .unwrap();
        assert_eq!(by_model.json["split"]["by"], "model");
        // A model name has no closed vocabulary, so the model axis IS cited —
        // the same field, a different closed thing, decided by the axis.
        assert_eq!(by_model.json["split"]["counts"][0]["key"], "cite:6");
        assert_eq!(by_model.json["split"]["counts"][0]["sessions"], 2);
        assert!(
            by_model.human.contains("cite:6") && by_model.human.contains("opus"),
            "the human recap must resolve the model citation: {}",
            by_model.human
        );
    });
}

/// A 64-hex checkpoint id — the shape `parse_checkpoint_claims` admits, which
/// the short fixture ids above deliberately are not.
fn hex_id(seed: &str) -> String {
    seed.repeat(32)
}

/// Run git in `dir`, panicking with its stderr on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        // A fixture repository must not inherit the operator's identity, hooks
        // or template — and must never be re-aimed at the repo under test.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with a `main` commit claiming `main_cp` by trailer and a
/// `feature` branch adding a commit claiming `feature_cp`.
fn two_branch_repo(root: &Path, main_cp: &str, feature_cp: &str) {
    git(root, &["init", "-q", "-b", "main", "."]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    git(root, &["config", "user.name", "Fixture"]);
    std::fs::write(root.join("a.txt"), "a").expect("write");
    git(root, &["add", "a.txt"]);
    git(
        root,
        &[
            "commit",
            "-q",
            "-m",
            &format!("first\n\nRift-Checkpoint: {main_cp}"),
        ],
    );
    git(root, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(root.join("b.txt"), "b").expect("write");
    git(root, &["add", "b.txt"]);
    git(
        root,
        &[
            "commit",
            "-q",
            "-m",
            &format!("second\n\nRift-Checkpoint: {feature_cp}"),
        ],
    );
}

/// Run `body` over the two-branch repository and a projection holding exactly
/// its two checkpoints, both datable; `body` gets the two checkpoint ids.
fn with_two_branches(body: impl FnOnce(&tokio::runtime::Runtime, &Planes<'_>, &str, &str)) {
    let rt = rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let (main_cp, feature_cp) = (hex_id("a"), hex_id("b"));
    two_branch_repo(root, &main_cp, &feature_cp);
    let store = rt.block_on(checkpoint_projection(&[
        (&main_cp, 1_000),
        (&feature_cp, 2_000),
    ]));
    let planes = Planes {
        capture: &store,
        root,
        recall_db: None,
    };
    body(&rt, &planes, &main_cp, &feature_cp);
}

/// **`dispatch --branch`, against the facts that exist.** This flag refused for
/// a release on the premise that no per-checkpoint commit fact was stored. The
/// premise was false: `Rift-Checkpoint:` trailers name checkpoints from the
/// commit side, and every shard records its anchoring commit. This test drives
/// the trailer route end to end on a two-branch repository, and asserts the
/// anchor route degrades honestly when no shard is present locally.
#[test]
fn a_branch_scopes_the_report_to_the_checkpoints_its_commits_claim() {
    with_two_branches(|rt, planes, main_cp, _| {
        let scope_of = |branch: &str| {
            let out = rt
                .block_on(dispatch_in(planes, None, None, Some(branch), false, false))
                .unwrap();
            assert_eq!(out.code, 0, "got: {}", out.human);
            out.json
        };

        // `main` holds one commit and claims one checkpoint.
        let m = scope_of("main");
        assert_eq!(m["branch"]["commits_walked"], 1);
        assert_eq!(m["branch"]["found_by_trailer"], 1);
        assert_eq!(m["checkpoints"]["count"], 1);
        assert_eq!(m["checkpoints"]["ids"][0], serde_json::json!(main_cp));

        // `feature` contains both commits, so both checkpoints are in scope — and
        // the report says which half a branch scoped.
        let f = scope_of("feature");
        assert_eq!(f["branch"]["commits_walked"], 2);
        assert_eq!(f["branch"]["found_by_trailer"], 2);
        assert_eq!(f["checkpoints"]["count"], 2);
        assert_eq!(f["branch"]["scopes"][0], "checkpoints");
        // No shard exists locally for either candidate, so the anchor route found
        // nothing and SAID so rather than silently contributing zero.
        assert_eq!(f["branch"]["found_by_shard_anchor"], 0);
        assert_eq!(f["branch"]["shards_not_present_locally"], 2);
    });
}

/// The refusals that remain are refusals about the REF, not about the
/// schema, and none of them echoes what the caller typed.
#[test]
fn a_branch_refusal_names_the_ref_problem_and_never_echoes_the_argument() {
    with_two_branches(|rt, planes, _, _| {
        for (arg, error) in [
            ("no-such-branch", "unknown_branch"),
            ("\u{1b}[2J;rm -rf /", "bad_branch_name"),
        ] {
            let out = rt
                .block_on(dispatch_in(planes, None, None, Some(arg), false, false))
                .unwrap();
            assert_ne!(out.code, 0);
            assert_eq!(out.json["error"], error);
            assert!(!out.human.contains('\u{1b}'), "got: {}", out.human);
            assert!(!out.human.contains("no-such-branch"), "got: {}", out.human);
        }
    });
}

/// Run git in `dir` with extra environment, returning trimmed stdout.
fn git_out(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A one-branch repository whose first commit (A) carries NO trailer and whose
/// second (B) names `both` and `trailer_only` by trailer; returns `(A, B)`.
///
/// Commit A is the one whose checkpoint only the shard anchor can reach.
fn trailer_and_bare_commits(root: &Path, both: &str, trailer_only: &str) -> (String, String) {
    git(root, &["init", "-q", "-b", "main", "."]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    git(root, &["config", "user.name", "Fixture"]);
    std::fs::write(root.join("a.txt"), "a").expect("write");
    git(root, &["add", "a.txt"]);
    git(root, &["commit", "-q", "-m", "first"]);
    let commit_a = git_out(root, &["rev-parse", "HEAD"], &[]);
    std::fs::write(root.join("b.txt"), "b").expect("write");
    git(root, &["add", "b.txt"]);
    git(
        root,
        &[
            "commit",
            "-q",
            "-m",
            &format!("second\n\nRift-Checkpoint: {both}\nRift-Checkpoint: {trailer_only}"),
        ],
    );
    let commit_b = git_out(root, &["rev-parse", "HEAD"], &[]);
    (commit_a, commit_b)
}

/// Write one shard per `(checkpoint id, anchoring commit)` onto the native
/// checkpoint branch, each carrying only the `metadata.json` whose `commit`
/// the anchor route reads — written with git plumbing, so the shard is a real
/// one rather than a stand-in.
fn write_anchor_shards(root: &Path, shards: &[(&str, &str)]) {
    let index = root.join("shard-index");
    for (id, anchor) in shards {
        let meta = root.join("meta.json");
        std::fs::write(
            &meta,
            format!(r#"{{"canon_version":3,"commit":"{anchor}","sessions":[]}}"#),
        )
        .expect("write");
        let blob = git_out(root, &["hash-object", "-w", "meta.json"], &[]);
        let (lo, hi) = id.split_at(2);
        git_out(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{lo}/{hi}/metadata.json"),
            ],
            &[("GIT_INDEX_FILE", &index.display().to_string())],
        );
        std::fs::remove_file(&meta).expect("rm");
    }
    let tree = git_out(
        root,
        &["write-tree"],
        &[("GIT_INDEX_FILE", &index.display().to_string())],
    );
    let shard_commit = git_out(root, &["commit-tree", &tree, "-m", "shards"], &[]);
    git(
        root,
        &[
            "update-ref",
            "refs/heads/rift/checkpoints/v1",
            &shard_commit,
        ],
    );
}

/// **The anchor route, exercised positively — the half no fixture reached.**
///
/// `--branch` unions two routes, and the trailer route is easy to test because
/// a trailer is just text in a commit message. The ANCHOR route needs a shard
/// on the checkpoint branch carrying `metadata.json`'s `commit`, so the earlier
/// fixture could only assert it degraded honestly (`shards_not_present_locally`)
/// — which is a test of the absence, not of the route. This one writes real
/// shards with git plumbing and pins all three cases: reachable ONLY by
/// trailer, ONLY by anchor, and by BOTH (counted once).
#[test]
fn the_anchor_route_finds_a_checkpoint_no_trailer_names_and_the_union_counts_once() {
    let rt = rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let (both, trailer_only, anchor_only, neither) =
        (hex_id("a"), hex_id("b"), hex_id("c"), hex_id("d"));
    let (commit_a, commit_b) = trailer_and_bare_commits(root, &both, &trailer_only);
    // Two shards on the native checkpoint branch: `both` anchored to B (which
    // also names it by trailer), `anchor_only` anchored to A (which names
    // nothing). `neither` gets no shard and no trailer.
    write_anchor_shards(root, &[(&both, &commit_b), (&anchor_only, &commit_a)]);
    let store = rt.block_on(checkpoint_projection(&[
        (&both, 1_000),
        (&trailer_only, 1_001),
        (&anchor_only, 1_002),
        (&neither, 1_003),
    ]));
    let planes = Planes {
        capture: &store,
        root,
        recall_db: None,
    };
    let out = rt
        .block_on(dispatch_in(&planes, None, None, Some("main"), false, false))
        .unwrap();
    assert_eq!(out.code, 0, "got: {}", out.human);
    let b = &out.json["branch"];

    assert_eq!(b["found_by_trailer"], 2, "both + trailer_only");
    assert_eq!(b["found_by_shard_anchor"], 2, "both + anchor_only");
    // The union counts `both` ONCE: 2 + 2 routes, 3 checkpoints.
    assert_eq!(b["checkpoints_in_scope"], 3, "got: {b}");
    assert_eq!(out.json["checkpoints"]["count"], 3);
    assert_eq!(
        b["claimed_but_not_in_this_report"], 0,
        "every claim here resolves"
    );

    let ids: Vec<&str> = out.json["checkpoints"]["ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(ids.contains(&anchor_only.as_str()), "the anchor-only route");
    assert!(
        ids.contains(&trailer_only.as_str()),
        "the trailer-only route"
    );
    assert!(ids.contains(&both.as_str()));
    assert!(
        !ids.contains(&neither.as_str()),
        "unreachable must stay out"
    );

    // Two shards were present, two candidates had none — the degraded arm is
    // still reported, beside the route that worked.
    assert_eq!(b["shards_not_present_locally"], 2);
}

/// A branch claim that resolves to no checkpoint this report holds is counted
/// under its OWN name. Reporting it as "in scope" overstated the number beside
/// the correct one: 110 against a `checkpoints.count` of 101 on the real
/// repository.
#[test]
fn a_trailer_claiming_an_unknown_checkpoint_is_counted_apart_from_the_scope() {
    let rt = rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let (known, ghost) = (hex_id("a"), hex_id("f"));

    git(root, &["init", "-q", "-b", "main", "."]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    git(root, &["config", "user.name", "Fixture"]);
    std::fs::write(root.join("a.txt"), "a").expect("write");
    git(root, &["add", "a.txt"]);
    git(
        root,
        &[
            "commit",
            "-q",
            "-m",
            &format!("first\n\nRift-Checkpoint: {known}\nRift-Checkpoint: {ghost}"),
        ],
    );

    let store = rt.block_on(checkpoint_projection(&[(&known, 1_000)]));
    let planes = Planes {
        capture: &store,
        root,
        recall_db: None,
    };
    let out = rt
        .block_on(dispatch_in(&planes, None, None, Some("main"), false, false))
        .unwrap();
    let b = &out.json["branch"];
    assert_eq!(b["found_by_trailer"], 2, "the branch claims two");
    assert_eq!(b["checkpoints_in_scope"], 1, "only one resolves");
    assert_eq!(b["claimed_but_not_in_this_report"], 1);
    // The number beside it agrees, which is the property that failed.
    assert_eq!(out.json["checkpoints"]["count"], 1);
}

/// **Determinism, as an exit criterion rather than an aspiration.** The same
/// window over the same rows serializes byte-identically — and a *different*
/// window does not, so the property is not vacuous.
///
/// **This is the one-process half, and it is named as such.** The exit
/// criterion is a claim about INVOCATIONS, and `f(x) == f(x)` inside one
/// process cannot see a per-process hash seed, a value cached on first use, or
/// an address-dependent sort. The claim it cannot make is made by
/// `every_workflow_verb_is_byte_identical_across_two_processes`
/// (`tests/hooks_workflow_e2e.rs`), which drives the shipped binary twice.
///
/// **That sentence used to name `review_and_investigate_…`, which covers two
/// DIFFERENT verbs from the two this test covers** — so a docstring here
/// claimed a gap was closed by a test that never touched `dispatch` or `recap`
/// (2026-08-24 assessment, pattern 4). The cross-process test now drives all
/// four, which is what makes the sentence true rather than merely repaired.
/// This one stays because it is fast and it is the half that runs on every
/// `cargo test` of this module.
#[test]
fn the_same_window_over_the_same_rows_serializes_byte_identically() {
    with_planes(|rt, planes| {
        let run = |since: &str| {
            let out = rt
                .block_on(dispatch_in(
                    planes,
                    Some(since),
                    Some("100000"),
                    None,
                    false,
                    true,
                ))
                .unwrap();
            serde_json::to_string(&out.json).unwrap_or_default()
        };
        assert_eq!(run("0"), run("0"));
        assert_ne!(
            run("0"),
            run("3000"),
            "a different window is a different report"
        );

        let recap_run = || {
            let out = rt
                .block_on(recap_in(planes, Some("0"), Some("100000"), false))
                .unwrap();
            serde_json::to_string(&out.json).unwrap_or_default()
        };
        assert_eq!(recap_run(), recap_run());
    });
}

// ---------------------------------------------------------------------------
// THE CROSS-SURFACE PIN
//
// Canopy's dispatch/recap cards read the same window through the same
// `rift_store::window::gather` these verbs call. That is a design claim, and a
// design claim with no failing test is an argument — so
// these two tests drive BOTH surfaces over ONE seeded projection and assert the
// numbers are equal.
//
// **They live here because only this crate can see both halves**: `dispatch_in`
// is `pub(crate)` in a binary nothing can link, and `rift-canopy-data` is a
// normal dependency of that binary. If the card ever grows a query of its own,
// the two sides can drift and one of these reddens — which is the whole point.
// ---------------------------------------------------------------------------

/// The card side, over the same planes the verb is given: gather, then apply
/// this repository's policy, exactly as `compute_workflow_cards` does around
/// its own projection open.
fn card_facts(rt: &tokio::runtime::Runtime, planes: &Planes<'_>, w: &Window) -> WindowFacts {
    let mut facts = rt
        .block_on(rift_canopy_data::workflows::window_facts(
            planes.capture,
            planes.recall_db.as_deref(),
            w,
        ))
        .expect("the card's own gather");
    rift_canopy_data::workflows::apply_policy(&mut facts, planes.root);
    facts
}

/// The dispatch verb's JSON and the dispatch card, both over the UNBOUNDED
/// window: the card gathers that window, so the verb is asked the same
/// question. Anything else would compare two different reports.
fn dispatch_doc_and_card(
    rt: &tokio::runtime::Runtime,
    planes: &Planes<'_>,
) -> (serde_json::Value, rift_canopy::DispatchData) {
    let w = Window::resolve(None, None).unwrap();
    let out = rt
        .block_on(dispatch_in(planes, None, None, None, false, false))
        .unwrap();
    let facts = card_facts(rt, planes, &w);
    let rift_canopy::DispatchView::Data(card) = rift_canopy_data::workflows::dispatch_view(&facts)
    else {
        panic!("the seeded projection must produce a card");
    };
    (out.json, card)
}

/// **Non-vacuity, by exact value.** A pin over two empty windows proves
/// nothing, and an agreement on zeros proves less. Every number below is
/// derivable by hand from the fixture paragraph in this module's docs, over
/// the UNBOUNDED window — so c3, the checkpoint no bounded window can hold,
/// and its 9,999 agent lines are all in scope here.
#[test]
fn a_dispatch_card_over_the_seeded_projection_carries_the_hand_derived_numbers() {
    with_planes(|rt, planes| {
        let (_, card) = dispatch_doc_and_card(rt, planes);
        assert_eq!(card.checkpoints, 3);
        assert_eq!(card.derived, 1, "c2, placed by the session it archives");
        assert_eq!(card.unresolved, 1, "c3, placed by nothing");
        assert_eq!(card.sessions, 3);
        assert_eq!(card.open_sessions, 2, "s2 and s3 never ended");
        assert_eq!(card.agent_lines, 10_034, "10 + 20 + 5 + 9,999");
        assert_eq!(card.mixed_lines, 3);
        assert_eq!(card.human_lines, 5);
        assert_eq!(card.confidence_milli, Some(550), "(900+700+500+100)/4");
        assert_eq!(card.files_total, 2);
        assert_eq!(
            card.sensitive,
            Some(1),
            ".env, by the non-droppable built-in"
        );
        assert_eq!(card.files[0].label, "\".env\"");
        assert_eq!(card.files[0].checkpoints, 2, "c2 and c3");
        assert!(card.files[0].sensitive);
        assert_eq!(card.files[1].label, "\"src/a.rs\"");
        assert!(!card.files[1].sensitive);
    });
}

#[test]
fn a_dispatch_card_and_the_dispatch_verb_agree_on_a_seeded_projection() {
    with_planes(|rt, planes| {
        let (doc, card) = dispatch_doc_and_card(rt, planes);
        assert_eq!(json_u64(&doc["checkpoints"]["count"]), card.checkpoints);
        assert_eq!(json_u64(&doc["checkpoints"]["derived_time"]), card.derived);
        assert_eq!(
            json_u64(&doc["checkpoints"]["unresolved_time"]),
            card.unresolved
        );
        assert_eq!(json_u64(&doc["sessions"]["count"]), card.sessions);
        assert_eq!(
            json_u64(
                &doc["sessions"]["open"]
                    .as_array()
                    .map_or(0, Vec::len)
                    .into()
            ),
            card.open_sessions
        );

        // The attribution split, line for line.
        assert_eq!(doc["attribution"]["ai_lines"], card.agent_lines);
        assert_eq!(doc["attribution"]["mixed_lines"], card.mixed_lines);
        assert_eq!(doc["attribution"]["human_lines"], card.human_lines);
        assert_eq!(
            doc["attribution"]["confidence_milli"],
            serde_json::json!(card.confidence_milli)
        );

        // The FILE LIST, in the same rank order with the same per-path counts
        // and the same policy verdict. The document names ordinals and the card
        // names paths, so the pin is over the shape they share.
        let files = doc["files"].as_array().expect("files");
        assert_eq!(json_u64(&serde_json::json!(files.len())), card.files_total);
        for (i, f) in files.iter().enumerate() {
            let c = card.files.get(i).expect("the card carries this row");
            assert_eq!(f["checkpoints"], c.checkpoints, "row {i}");
            assert_eq!(f["sensitive"], c.sensitive, "row {i}");
            // …and the ordinal is the document's, never the card's.
            assert_eq!(f["evidence"], serde_json::json!(i + 1));
        }
        assert_eq!(
            Some(json_u64(&doc["risk"]["sensitive_paths_touched"])),
            card.sensitive
        );
    });
}

#[test]
fn a_recap_card_and_the_recap_verb_agree_on_a_seeded_projection() {
    with_planes(|rt, planes| {
        let w = Window::resolve(None, None).unwrap();
        let facts = card_facts(rt, planes, &w);
        for by_model in [false, true] {
            let out = rt.block_on(recap_in(planes, None, None, by_model)).unwrap();
            let doc = &out.json;
            let rift_canopy::RecapView::Data(card) =
                rift_canopy_data::workflows::recap_view(&facts, by_model)
            else {
                panic!("the seeded projection must produce a card");
            };

            // Non-vacuity by exact value, as above.
            assert_eq!(card.sessions, 3);
            assert_eq!(card.checkpoints, 3);
            assert_eq!(card.tokens, 414, "380 on mc1 + 34 on mc2");
            assert_eq!(card.split_total, 2);
            assert_eq!(
                card.split,
                if by_model {
                    vec![("\"opus\"".to_owned(), 2), ("\"sonnet\"".to_owned(), 1)]
                } else {
                    vec![
                        ("\"claude-code\"".to_owned(), 2),
                        ("\"codex\"".to_owned(), 1),
                    ]
                }
            );
            assert_eq!(json_u64(&doc["sessions"]), card.sessions);
            assert_eq!(json_u64(&doc["checkpoints"]), card.checkpoints);
            assert_eq!(json_u64(&doc["tokens"]["total"]), card.tokens);
            assert_eq!(
                doc["split"]["by"],
                if by_model { "model" } else { "agent" },
                "the card and the document are on the same axis"
            );

            // The SPLIT: same keys in the same order with the same counts. The
            // document cites each key and the card renders it, so the pin is
            // over the counts and the arity — a card that grouped differently
            // would change one of them.
            let counts = doc["split"]["counts"].as_array().expect("counts");
            assert_eq!(json_u64(&serde_json::json!(counts.len())), card.split_total);
            for (i, row) in counts.iter().enumerate() {
                let (_, n) = card.split.get(i).expect("the card carries this row");
                assert_eq!(json_u64(&row["sessions"]), *n, "row {i}");
            }

            // Recall's ledger — the same two numbers, and absence spelled the
            // same way on both sides.
            let s = card.savings.expect("the fixture writes a ledger");
            assert_eq!(doc["savings"]["available"], true);
            assert_eq!(
                json_u64(&doc["savings"]["tokens_avoided_estimated_upper_bound"]),
                s.avoided_upper_bound
            );
            assert_eq!(
                json_u64(&doc["savings"]["tokens_injected_spend"]),
                s.injected
            );
        }
    });
}

/// **The two surfaces escape identically, or they disagree about a value while
/// agreeing about the fact.**
///
/// The card cannot call `provenance::safe_path` (it is `pub(crate)` in this
/// binary) and this binary is not linkable from Canopy, so the two functions
/// are necessarily separate code. This is the test that stops them drifting;
/// it is here because here is the only place both are in scope.
#[test]
fn the_card_escapes_exactly_as_the_report_does() {
    for hostile in [
        "src/a.rs",
        "src/\na.rs",
        "x\u{1b}[31mred",
        "a\u{202e}b.rs",
        "quote\"inside",
        "tab\there",
        "café ✓",
        &"a".repeat(300),
    ] {
        assert_eq!(
            rift_canopy::escape_for_terminal(hostile),
            crate::provenance::safe_path(hostile),
            "the card and the report spell this value differently"
        );
    }
}

/// One hostile character, drawn without a regex — so the newline-blind
/// generator walker (`ci_meta::proptest_generators_reach_newlines`) cannot
/// list it, and the alphabet is readable as a list: C0 controls, DEL, C1
/// controls, ESC and a CSI introducer, the bidi overrides and isolates, the
/// three whitespace controls, the two characters `escape_debug` must quote,
/// ordinary path bytes, and non-ASCII.
fn hostile_char() -> impl proptest::strategy::Strategy<Value = char> {
    use proptest::prelude::*;
    prop_oneof![
        (0u32..0x20).prop_map(|u| char::from_u32(u).expect("C0")),
        Just('\u{7f}'),
        (0x80u32..0xa0).prop_map(|u| char::from_u32(u).expect("C1")),
        prop_oneof![Just('\u{1b}'), Just('['), Just('m'), Just('3'), Just('1')],
        (0x202au32..=0x202e).prop_map(|u| char::from_u32(u).expect("bidi override")),
        (0x2066u32..=0x2069).prop_map(|u| char::from_u32(u).expect("bidi isolate")),
        prop_oneof![Just('\t'), Just('\n'), Just('\r')],
        prop_oneof![Just('"'), Just('\\')],
        prop_oneof![
            Just('a'),
            Just('z'),
            Just('0'),
            Just('/'),
            Just('.'),
            Just('-'),
            Just('_'),
            Just(' ')
        ],
        prop_oneof![Just('é'), Just('✓'), Just('日'), Just('\u{1f600}')],
    ]
}

/// A hostile string whose length straddles the 160-cell bound both escapes
/// share: short, around the bound, and well past it — so a twin whose bound
/// drifted by one is reached, not argued about.
fn hostile_string() -> impl proptest::strategy::Strategy<Value = String> {
    use proptest::prelude::*;
    prop_oneof![0usize..8, 150usize..=170, 300usize..=320]
        .prop_flat_map(|n| proptest::collection::vec(hostile_char(), n))
        .prop_map(|chars| chars.into_iter().collect())
}

proptest::proptest! {
    /// The eight examples above, as a property: over the hostile alphabet and
    /// at lengths on both sides of the bound, the card and the report spell
    /// every value identically. The examples stay as the readable corpus;
    /// this is the reach.
    #[test]
    fn the_card_and_the_report_escape_identically_over_a_hostile_alphabet(s in hostile_string()) {
        proptest::prop_assert_eq!(
            rift_canopy::escape_for_terminal(&s),
            crate::provenance::safe_path(&s),
            "the card and the report spell this value differently"
        );
    }
}

/// The property's reach, pinned at the two edges it must cover: one cell
/// over the bound in plain characters, and the same overflow reached through
/// escaping (each ESC escapes to `\u{1b}`, six cells; 26 fit in 156 and the
/// 27th would land at 162, so 27 ESCs is the first escaped overflow — 25, as
/// first planned, is 150 cells and does not cut). Both sides cut, both with `…`.
#[test]
fn both_escapes_cut_at_the_bound_in_plain_and_in_escaped_cells() {
    let plain = "a".repeat(161);
    let escaped = "\u{1b}".repeat(27);
    for (label, s) in [("161 plain", plain.as_str()), ("27 ESC", escaped.as_str())] {
        let card = rift_canopy::escape_for_terminal(s);
        let report = crate::provenance::safe_path(s);
        assert!(card.ends_with('…'), "{label}: the card did not cut: {card}");
        assert!(
            report.ends_with('…'),
            "{label}: the report did not cut: {report}"
        );
        assert_eq!(card, report, "{label}");
    }
}

/// A twin of the two escapes with one knob turned, for the calibration below.
fn twin(p: &str, bound: usize, escape: fn(char) -> String) -> String {
    let mut body = String::new();
    let mut width = 0usize;
    let mut cut = false;
    for c in p.chars() {
        let piece = escape(c);
        let w = piece.chars().count();
        if width + w > bound {
            cut = true;
            break;
        }
        body.push_str(&piece);
        width += w;
    }
    if cut {
        format!("\"{body}\"…")
    } else {
        format!("\"{body}\"")
    }
}

/// **The corpus tells a drifted twin apart** — the calibration that makes the
/// property above a guard rather than a tautology. Two twins of the report's
/// escape, each one knob off (a bound of 161; `escape_default` instead of
/// `escape_debug`), are driven through the same strategy with a deterministic
/// runner, and each must fail against the card. A strategy that never reached
/// cell 161 or never drew a character the two escapes spell differently would
/// pass both, and this test would say so.
#[test]
fn a_twin_with_a_drifted_bound_or_escape_is_told_apart_by_the_corpus() {
    use proptest::test_runner::TestRunner;
    type Twin = fn(&str) -> String;
    let drifted: [(&str, Twin); 2] = [
        ("bound 161", |s| {
            twin(s, 161, |c| c.escape_debug().collect())
        }),
        ("escape_default", |s| {
            twin(s, 160, |c| c.escape_default().collect())
        }),
    ];
    for (label, drifted_twin) in &drifted {
        let mut runner = TestRunner::deterministic();
        let outcome = runner.run(&hostile_string(), |s| {
            proptest::prop_assert_eq!(rift_canopy::escape_for_terminal(&s), drifted_twin(&s));
            Ok(())
        });
        assert!(
            outcome.is_err(),
            "the corpus never told the {label} twin apart from the card — the property's \
             reach has shrunk"
        );
    }
    // And the un-drifted twin is the report: the corpus does not cry wolf.
    let mut runner = TestRunner::deterministic();
    let faithful = runner.run(&hostile_string(), |s| {
        proptest::prop_assert_eq!(
            crate::provenance::safe_path(&s),
            twin(&s, 160, |c| c.escape_debug().collect())
        );
        Ok(())
    });
    assert!(faithful.is_ok(), "{faithful:?}");
}

/// `serde_json`'s numbers are `u64`/`i64`/`f64` unions; the card's are `u64`.
/// One conversion, so a comparison can never silently be `Null == Null`.
fn json_u64(v: &serde_json::Value) -> u64 {
    v.as_u64()
        .unwrap_or_else(|| panic!("expected a number, got {v}"))
}
