use super::facts::commit_oid_from_metadata;
use super::*;
use rift_recall_substrate::MemorySubstrate;
use rift_store::SqliteStore;

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

pub(super) fn valid_report() -> String {
    format!(
        r#"{{"total_spans":1,"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
        "c".repeat(64)
    )
}

fn full_jsonl() -> String {
    [
        r#"{"session_id":"native-1","turn":0,"event":0,"kind":"session_start","verb":"SessionStart","occurred_at_ms":10,"payload":{"model":"claude-sonnet-4-5"}}"#,
        r#"{"session_id":"native-1","turn":1,"event":0,"kind":"tool_call","verb":"PreToolUse","occurred_at_ms":11,"payload":{"tool_name":"Bash"}}"#,
        r#"{"session_id":"native-1","turn":1,"event":0,"kind":"turn_end","verb":"Stop","occurred_at_ms":12,"payload":{}}"#,
        r#"{"session_id":"native-1","turn":2,"event":0,"kind":"session_end","verb":"SessionEnd","occurred_at_ms":13,"payload":{}}"#,
    ]
    .join("\n")
}

fn shard_id(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn shard(id: &str, ordinal: u64) -> CheckpointShard {
    CheckpointShard {
        checkpoint_id: id.to_owned(),
        dialect: rift_recall_substrate::Dialect::Rift,
        prev: None,
        ordinal,
    }
}

fn substrate_with_shard(id: &str, redaction: Option<&str>) -> MemorySubstrate {
    let mut sub = MemorySubstrate::local().with_shard(shard(id, 0));
    if let Some(r) = redaction {
        sub = sub.with_artifact(id, "redaction_report.json", r.as_bytes());
    }
    sub.with_artifact(
        id,
        "metadata.json",
        format!(
            r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"]}}"#,
            "d".repeat(64)
        )
        .as_bytes(),
    )
    .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
}

/// A valid shard whose header additionally spells `commit` as `claim`
/// (verbatim JSON, so a test can put a non-string or a junk token there).
fn substrate_claiming_commit(id: &str, claim: &str) -> MemorySubstrate {
    MemorySubstrate::local()
        .with_shard(shard(id, 0))
        .with_artifact(id, "redaction_report.json", valid_report().as_bytes())
        .with_artifact(
            id,
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"],"commit":{claim}}}"#,
                "d".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
}

async fn linked_commits(s: &SqliteStore) -> Vec<(String, String)> {
    let rows = s
        .query(
            "SELECT checkpoint_id, commit_oid FROM checkpoint_commit ORDER BY checkpoint_id",
            &[],
        )
        .await
        .unwrap();
    rows.iter()
        .map(|r| (r.get_text(0).unwrap(), r.get_text(1).unwrap()))
        .collect()
}

/// **The linkage, end to end.** The commit a shard names lands as a row,
/// keyed to that checkpoint — and a second ingest of the same substrate
/// (what a REBUILD is) re-derives it without a duplicate and without an
/// UPDATE. Deleting the `record_checkpoint_commit_if_new` call reds the
/// first assertion; making the write non-idempotent reds the second.
#[tokio::test]
async fn the_commit_a_shard_names_lands_and_a_rebuild_re_derives_it() {
    let s = store();
    let id = shard_id(0x61);
    let oid = "ab".repeat(20); // 40 hex = a SHA-1 commit
    let sub = substrate_claiming_commit(&id, &format!("\"{oid}\""));

    let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
        .await
        .unwrap();
    assert_eq!(r.parse_issues, 0);
    assert_eq!(linked_commits(&s).await, vec![(id.clone(), oid.clone())]);

    // The rebuild: same substrate, same store, set-union no-op.
    ingest_from_substrate(&s, &sub, "repo", "claude-code", 2)
        .await
        .unwrap();
    assert_eq!(
        linked_commits(&s).await,
        vec![(id, oid)],
        "a rebuild re-derives the same row, it does not duplicate or update it"
    );
}

/// A shard that names no commit gets no row — the absence is the answer,
/// which is exactly what a separate relation buys over a nullable column.
#[tokio::test]
async fn a_shard_that_names_no_commit_gets_no_row() {
    let s = store();
    let id = shard_id(0x62);
    let sub = substrate_with_shard(&id, Some(&valid_report()));
    let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
        .await
        .unwrap();
    assert_eq!(r.parse_issues, 0, "silence is not an issue");
    assert!(linked_commits(&s).await.is_empty());
}

/// Writer to reader, the anchorless case: the `metadata.json` the shard
/// writer produces for an archive with no anchor carries no `commit` member,
/// and the bridge takes that as the answer — no `checkpoint_commit` row and
/// no issue — where a malformed claim (next test) is refused and named.
#[tokio::test]
async fn an_absent_anchor_is_omitted_from_metadata_and_names_no_commit_row() {
    let s = store();
    let id = shard_id(0x64);
    let mut anchorless = crate::backend::fixtures::archive();
    anchorless.commit = None;
    anchorless.checkpoint_id = id.clone();
    anchorless.sessions = vec!["native-1".to_owned()];
    let metadata = anchorless.metadata_json();
    let doc: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
    assert!(
        doc.get("commit").is_none(),
        "the writer omits the member: {doc}"
    );
    let sub = MemorySubstrate::local()
        .with_shard(shard(&id, 0))
        .with_artifact(&id, "redaction_report.json", valid_report().as_bytes())
        .with_artifact(&id, "metadata.json", &metadata)
        .with_artifact(&id, "full.jsonl", full_jsonl().as_bytes());
    let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
        .await
        .unwrap();
    assert_eq!(
        r.parse_issues, 0,
        "absence is not an issue: {:?}",
        r.parse_notes
    );
    assert!(
        linked_commits(&s).await.is_empty(),
        "no anchor row is invented"
    );
}

/// A header claiming something that cannot be an object id is REFUSED and
/// SAID — no row, and a note naming the shard. The checkpoint itself still
/// projects: one bad field does not cost the chain row.
#[tokio::test]
async fn a_claim_that_cannot_be_an_object_id_is_refused_and_named() {
    let s = store();
    let id = shard_id(0x63);
    let sub = substrate_claiming_commit(&id, "\"HEAD~1\"");
    let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
        .await
        .unwrap();
    assert!(linked_commits(&s).await.is_empty(), "no invented linkage");
    assert_eq!(r.parse_issues, 1);
    assert!(
        r.parse_notes.iter().any(|n| n.contains("not an object id")),
        "the refusal names itself: {:?}",
        r.parse_notes
    );
    let ckpts = s
        .query("SELECT COUNT(*) FROM checkpoint", &[])
        .await
        .unwrap();
    assert_eq!(
        ckpts.first().unwrap().get_i64(0).unwrap(),
        1,
        "the chain row is unaffected by a bad commit claim"
    );
}

/// The screen, exhaustively. Both git oid widths pass, in either case, and
/// storage is canonical lowercase; everything shaped like a revision
/// expression, a truncated id, or an empty string is refused.
#[test]
fn commit_oid_screen_accepts_object_ids_and_nothing_else() {
    let sha1 = "a".repeat(40);
    let sha256 = "b".repeat(64);
    assert_eq!(commit_oid_from_metadata(Some(&sha1)), Some(sha1.clone()));
    assert_eq!(commit_oid_from_metadata(Some(&sha256)), Some(sha256));
    assert_eq!(
        commit_oid_from_metadata(Some(&"AB".repeat(20))),
        Some("ab".repeat(20)),
        "stored lowercase — the registry's rule for every hash and oid"
    );
    assert_eq!(
        commit_oid_from_metadata(Some(&format!("  {sha1}  "))),
        Some(sha1.clone()),
        "surrounding whitespace is not part of the id"
    );
    for junk in [
        "",
        "   ",
        "HEAD",
        "HEAD~1",
        "refs/heads/main",
        &sha1[..39],         // truncated
        &format!("{sha1}0"), // 41
        &"g".repeat(40),     // hex-shaped but not hex
        &format!("{}z", &sha1[..39]),
    ] {
        assert_eq!(
            commit_oid_from_metadata(Some(junk)),
            None,
            "{junk:?} is not an object id"
        );
    }
    assert_eq!(commit_oid_from_metadata(None), None);
}

/// A shard whose `full.jsonl` is exactly `lines` (everything else valid).
fn substrate_with_events(id: &str, lines: &[&str]) -> MemorySubstrate {
    MemorySubstrate::local()
        .with_shard(shard(id, 0))
        .with_artifact(id, "redaction_report.json", valid_report().as_bytes())
        .with_artifact(
            id,
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"]}}"#,
                "d".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(id, "full.jsonl", lines.join("\n").as_bytes())
}

/// **The dot-drop, closed.** The committed read-compat fixtures under
/// `legacy-fixtures/` record `"kind":"session.start"`, and the bridge used
/// to answer that with `from_token` → `None` → skip: a whole archived
/// dialect discarded into an anonymous counter, on a repository Rift
/// promises to read forever.
///
/// Both spellings project now, and — the half that matters — what lands in
/// the store is ONE spelling regardless of which the shard used.
#[tokio::test]
async fn a_dot_spelled_record_projects_and_is_stored_under_the_canonical_spelling() {
    let s = store();
    let id = shard_id(0x51);
    let sub = substrate_with_events(
        &id,
        &[
            r#"{"session_id":"native-1","turn":0,"event":0,"kind":"session.start","verb":"SessionStart","occurred_at_ms":10,"payload":{}}"#,
            r#"{"session_id":"native-1","turn":1,"event":0,"kind":"tool.call","verb":"PreToolUse","occurred_at_ms":11,"payload":{}}"#,
        ],
    );
    let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
        .await
        .unwrap();
    assert_eq!(r.events_derived, 2, "neither record was dropped");
    assert_eq!(r.parse_issues, 0, "and neither counted as an issue");
    assert!(r.parse_notes.is_empty());

    let rows = s
        .query("SELECT kind FROM captured_event ORDER BY kind", &[])
        .await
        .unwrap();
    let kinds: Vec<String> = rows
        .iter()
        .map(|r| r.get_text(0).unwrap_or_default().clone())
        .collect();
    assert_eq!(
        kinds,
        vec!["session_start".to_owned(), "tool_call".to_owned()],
        "the store carries the canonical spelling, never the wire dialect"
    );
}

/// **The silent drop dies loudly.** A kind nothing recognises is still
/// refused — guessing is worse — but it is NAMED, with its token, its
/// shard and its position, instead of adding 1 to a number.
#[tokio::test]
async fn an_unrecognised_kind_becomes_a_named_parse_note() {
    let s = store();
    let id = shard_id(0x52);
    let sub = substrate_with_events(
        &id,
        &[
            r#"{"session_id":"native-1","turn":0,"event":0,"kind":"session_start","verb":"SessionStart","occurred_at_ms":10,"payload":{}}"#,
            r#"{"session_id":"native-1","turn":3,"event":7,"kind":"banana_split","verb":"Nope","occurred_at_ms":11,"payload":{}}"#,
        ],
    );
    let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
        .await
        .unwrap();
    assert_eq!(r.events_derived, 1, "the good record still projects");
    assert_eq!(r.parse_issues, 1);
    assert_eq!(r.parse_notes.len(), 1, "and the bad one is named");
    let note = &r.parse_notes[0];
    for expected in ["banana_split", "native-1", "turn 3", "event 7", &id] {
        assert!(note.contains(expected), "note names {expected}: {note}");
    }
    assert!(
        r.render().contains("banana_split"),
        "and it reaches the rendered report:\n{}",
        r.render()
    );
}

/// The naming is bounded and says so; the COUNT never is.
#[tokio::test]
async fn parse_notes_are_capped_and_the_report_says_how_many_were_not_named() {
    let s = store();
    let id = shard_id(0x53);
    let bad: Vec<String> = (0..MAX_PARSE_NOTES + 3)
        .map(|i| {
            format!(
                r#"{{"session_id":"native-1","turn":{i},"event":0,"kind":"unknown_{i}","verb":"X","occurred_at_ms":1,"payload":{{}}}}"#
            )
        })
        .collect();
    let lines: Vec<&str> = bad.iter().map(String::as_str).collect();
    let r = ingest_from_substrate(&s, &substrate_with_events(&id, &lines), "repo", "cc", 1)
        .await
        .unwrap();
    assert_eq!(r.parse_issues, MAX_PARSE_NOTES + 3, "every drop is counted");
    assert_eq!(r.parse_notes.len(), MAX_PARSE_NOTES, "the naming is capped");
    assert!(
        r.render().contains("and 3 more not named"),
        "the cap is disclosed:\n{}",
        r.render()
    );
}

/// A shard-supplied token is bounded before it is quoted back: the note is
/// built from untrusted bytes and is printed by a hook.
#[tokio::test]
async fn a_hostile_kind_token_is_truncated_in_the_note() {
    let s = store();
    let id = shard_id(0x54);
    let huge = "z".repeat(4000);
    let line = format!(
        r#"{{"session_id":"native-1","turn":0,"event":0,"kind":"{huge}","verb":"X","occurred_at_ms":1,"payload":{{}}}}"#
    );
    let r = ingest_from_substrate(&s, &substrate_with_events(&id, &[&line]), "repo", "cc", 1)
        .await
        .unwrap();
    assert_eq!(r.parse_notes.len(), 1);
    assert!(
        r.parse_notes[0].len() < 300,
        "the note is bounded: {} bytes",
        r.parse_notes[0].len()
    );
    assert!(r.parse_notes[0].contains('…'), "and says it was cut");
}

#[tokio::test]
async fn bridge_projects_valid_shard_and_rerun_is_noop() {
    let s = store();
    let id = shard_id(0xab);
    let sub = substrate_with_shard(&id, Some(&valid_report()));

    let r1 = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!(r1.shards_projected, 1);
    assert!(r1.refused.is_empty());
    assert_eq!(r1.events_derived, 4);
    assert_eq!(r1.events_inserted, 4);
    // Normalized rows landed through the shared projector.
    assert_eq!(s.count("session").await.unwrap(), 1);
    assert_eq!(s.count("session_event").await.unwrap(), 2);
    assert_eq!(s.count("tool_call").await.unwrap(), 1);
    assert_eq!(s.count("checkpoint").await.unwrap(), 1);
    assert_eq!(s.count("session_in_checkpoint").await.unwrap(), 1);
    assert_eq!(s.count("ingest_watermark").await.unwrap(), 1);

    // Re-run: zero inserts anywhere (crash-resume / idempotency).
    let r2 = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 200)
        .await
        .unwrap();
    assert_eq!(r2.events_inserted, 0, "re-ingest is a set-union no-op");
    assert_eq!(s.count("captured_event").await.unwrap(), 4);
    assert_eq!(s.count("ingest_watermark").await.unwrap(), 1);
}

#[tokio::test]
async fn shard_without_valid_redaction_report_is_refused() {
    let s = store();
    let missing = shard_id(0x01);
    let invalid = shard_id(0x02);
    let sub = MemorySubstrate::local()
        .with_shard(shard(&missing, 0))
        .with_artifact(&missing, "full.jsonl", full_jsonl().as_bytes())
        .with_shard(shard(&invalid, 1))
        .with_artifact(&invalid, "redaction_report.json", b"{}")
        .with_artifact(&invalid, "full.jsonl", full_jsonl().as_bytes());

    let r = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 5)
        .await
        .unwrap();
    assert_eq!(r.shards_projected, 0);
    assert_eq!(r.refused.len(), 2);
    assert_eq!(
        s.count("captured_event").await.unwrap(),
        0,
        "a shard without a valid redaction report is NEVER projected"
    );
}

#[tokio::test]
async fn non_local_reader_is_refused_outright() {
    struct RemoteReader(MemorySubstrate);
    impl SubstrateReader for RemoteReader {
        fn is_local_only(&self) -> bool {
            false
        }
        fn checkpoints(&self) -> rift_core::Result<Vec<CheckpointShard>> {
            self.0.checkpoints()
        }
        fn blob(&self, id: &str) -> rift_core::Result<Option<Vec<u8>>> {
            self.0.blob(id)
        }
        fn read_artifact(&self, c: &str, rel: &str) -> rift_core::Result<Option<Vec<u8>>> {
            self.0.read_artifact(c, rel)
        }
    }
    let s = store();
    let sub = RemoteReader(MemorySubstrate::local());
    let out = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 1).await;
    assert!(matches!(out, Err(RiftError::PolicyDenied(_))));
}

#[tokio::test]
async fn bridge_report_snapshot() {
    let s = store();
    let ok = shard_id(0xab);
    let bad = shard_id(0xcd);
    // one refused shard alongside the good one
    let sub = substrate_with_shard(&ok, Some(&valid_report()))
        .with_shard(shard(&bad, 1))
        .with_artifact(&bad, "full.jsonl", full_jsonl().as_bytes());
    let r = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 42)
        .await
        .unwrap();
    insta::assert_snapshot!("bridge_report", r.render());
}
/// A shard whose header carries a verbatim `worktree` member.
fn substrate_with_worktree(id: &str, member: &str) -> MemorySubstrate {
    MemorySubstrate::local()
        .with_shard(shard(id, 0))
        .with_artifact(id, "redaction_report.json", valid_report().as_bytes())
        .with_artifact(
            id,
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"],"worktree":{member}}}"#,
                "d".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
}

async fn worktree_rows(s: &SqliteStore) -> Vec<(String, String, String, i64, Option<String>)> {
    s.query(
        "SELECT checkpoint_id, worktree_token, worktree_name, is_linked, branch \
         FROM checkpoint_worktree ORDER BY checkpoint_id",
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| {
        (
            r.get_text(0).unwrap(),
            r.get_text(1).unwrap(),
            r.get_text(2).unwrap(),
            r.get_i64(3).unwrap(),
            r.get_opt_text(4).unwrap(),
        )
    })
    .collect()
}

/// **Rebuild parity, which is the whole point.** Every shard written
/// before the member existed carries no `worktree` member, and the
/// projection must invent NO row for it — back-filling a sealed shard is
/// forbidden, and readers render the absent row as `unknown`, never as
/// `main`.
/// The shard that DOES carry one projects exactly it, and a rebuild
/// re-derives the same row rather than duplicating or updating it.
#[tokio::test]
async fn a_shard_without_worktree_provenance_projects_no_row() {
    let s = store();
    let old = shard_id(0x51);
    let sub = substrate_with_shard(&old, Some(&valid_report()));
    let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
        .await
        .unwrap();
    assert_eq!(r.shards_projected, 1, "the shard itself still projects");
    assert_eq!(
        r.parse_issues, 0,
        "a memberless shard is ordinary, not a note"
    );
    assert!(
        worktree_rows(&s).await.is_empty(),
        "no member, no row — unknown stays unknown"
    );

    // A labelled shard projects the label it carries.
    let new = shard_id(0x52);
    let labelled = substrate_with_worktree(
        &new,
        r#"{"name":"swt-a","token":"0123456789ab","linked":true,"branch":"swt/a-foundations"}"#,
    );
    ingest_from_substrate(&s, &labelled, "repo", "claude-code", 2)
        .await
        .unwrap();
    assert_eq!(
        worktree_rows(&s).await,
        vec![(
            new.clone(),
            "0123456789ab".to_owned(),
            "swt-a".to_owned(),
            1,
            Some("swt/a-foundations".to_owned())
        )]
    );

    // The rebuild: same substrate, same store, set-union no-op.
    ingest_from_substrate(&s, &labelled, "repo", "claude-code", 3)
        .await
        .unwrap();
    assert_eq!(
        worktree_rows(&s).await.len(),
        1,
        "a rebuild re-derives, never duplicates"
    );
}

/// A member this reader will not believe is NAMED rather than silently
/// dropped — an anonymous counter is how an archived dialect once vanished
/// — and still writes no row, because a half-trusted label in an
/// append-only table cannot be corrected.
#[tokio::test]
async fn a_worktree_member_the_reader_refuses_is_named_and_writes_nothing() {
    let s = store();
    let id = shard_id(0x53);
    let sub = substrate_with_worktree(&id, r#"{"name":"swt-a","token":"../../etc","linked":true}"#);
    let r = ingest_from_substrate(&s, &sub, "repo", "claude-code", 1)
        .await
        .unwrap();
    assert!(worktree_rows(&s).await.is_empty());
    assert_eq!(r.parse_issues, 1);
    assert!(
        r.parse_notes.iter().any(|n| n.contains("worktree member")),
        "{:?}",
        r.parse_notes
    );
}

/// A shard listing two subagent subtrees, one of whose metadata does not parse.
fn substrate_with_tasks(id: &str) -> MemorySubstrate {
    MemorySubstrate::local()
        .with_shard(shard(id, 0))
        .with_artifact(id, "redaction_report.json", valid_report().as_bytes())
        .with_artifact(
            id,
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"],"tasks":["toolu_a","toolu_b","toolu_c"]}}"#,
                "d".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
        .with_artifact(
            id,
            "tasks/toolu_a/metadata.json",
            br#"{"schema_version":1,"tool_use_id":"toolu_a","agent_id":"a1","spawn":"matched"}"#,
        )
        .with_artifact(
            id,
            "tasks/toolu_b/metadata.json",
            br#"{"schema_version":1,"tool_use_id":"toolu_b","agent_id":"a2","spawn":"unmatched"}"#,
        )
        .with_artifact(id, "tasks/toolu_c/metadata.json", b"{not json")
}

/// A rebuild parses and counts the subagent subtrees a shard lists — the same
/// count from every wiped projection — and an unreadable one is a parse issue,
/// not a subtree.
#[tokio::test]
async fn a_rebuild_counts_the_same_task_subtrees_from_a_wiped_projection() {
    let sub = substrate_with_tasks(&shard_id(7));
    let mut counts = Vec::new();
    for _ in 0..2 {
        let report = ingest_from_substrate(&store(), &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert_eq!(report.shards_projected, 1);
        assert!(report.parse_issues >= 1, "the broken subtree is named");
        assert!(
            report.render().contains("task subtrees: 2"),
            "{}",
            report.render()
        );
        counts.push(report.task_subtrees);
    }
    assert_eq!(counts, [2, 2]);
}

/// A shard whose session transcript sealed one spill and lost one, and whose
/// one subtree sealed another; `toolu_c`'s body was edited after the seal.
fn substrate_with_spills(id: &str) -> MemorySubstrate {
    let digest = |b: &[u8]| crate::sha256_hex(&[b]);
    let own = format!(
        r#"[{{"bytes":3,"sha256":"{}","tool_use_id":"toolu_a"}},{{"spill":"spill_missing","tool_use_id":"toolu_gone"}},{{"bytes":3,"sha256":"{}","tool_use_id":"toolu_c"}}]"#,
        digest(b"aaa"),
        digest(b"ccc"),
    );
    MemorySubstrate::local()
        .with_shard(shard(id, 0))
        .with_artifact(id, "redaction_report.json", valid_report().as_bytes())
        .with_artifact(
            id,
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"],"spills":{own},"tasks":["toolu_t"]}}"#,
                "d".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
        .with_artifact(id, "tool-results/toolu_a.txt", b"aaa")
        .with_artifact(id, "tool-results/toolu_c.txt", b"cc!")
        .with_artifact(
            id,
            "tasks/toolu_t/metadata.json",
            format!(
                r#"{{"schema_version":1,"tool_use_id":"toolu_t","agent_id":"a1","spills":[{{"bytes":3,"sha256":"{}","tool_use_id":"toolu_in"}}]}}"#,
                digest(b"iii")
            )
            .as_bytes(),
        )
        .with_artifact(id, "tasks/toolu_t/tool-results/toolu_in.txt", b"iii")
}

/// A rebuild counts the spills beside both transcripts — the same from every
/// wiped projection — names a recorded loss as lost, and turns a body that no
/// longer matches its digest into a parse issue rather than a spill.
#[tokio::test]
async fn a_rebuild_counts_the_same_spills_from_a_wiped_projection() {
    let sub = substrate_with_spills(&shard_id(8));
    let mut counts = Vec::new();
    for _ in 0..2 {
        let report = ingest_from_substrate(&store(), &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert_eq!(report.shards_projected, 1);
        assert!(report.parse_issues >= 1, "the edited body is named");
        assert!(
            report
                .render()
                .contains("spills: 2 sealed, 1 recorded lost"),
            "{}",
            report.render()
        );
        counts.push((report.spills_sealed, report.spills_lost));
    }
    assert_eq!(counts, [(2, 1), (2, 1)]);
}

/// A shard whose session transcript sealed one original and recorded one
/// unsealed, and whose subtree sealed an orphan; `toolu_c`'s body was edited
/// after the seal.
fn substrate_with_originals(id: &str) -> MemorySubstrate {
    let digest = |b: &[u8]| crate::sha256_hex(&[b]);
    let own = format!(
        r#"[{{"bytes":3,"form":"folded_output","line":1,"replaced":[],"rewritten":"folded_output","sha256":"{}","tool_use_id":"toolu_a"}},{{"original":"original_unsealed","replaced":[],"tool_use_id":"toolu_u"}},{{"bytes":3,"sha256":"{}","tool_use_id":"toolu_c"}}]"#,
        digest(b"aaa"),
        digest(b"ccc"),
    );
    MemorySubstrate::local()
        .with_shard(shard(id, 0))
        .with_artifact(id, "redaction_report.json", valid_report().as_bytes())
        .with_artifact(
            id,
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"],"originals":{own},"tasks":["toolu_t"]}}"#,
                "d".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
        .with_artifact(id, "tool-originals/toolu_a.json", b"aaa")
        .with_artifact(id, "tool-originals/toolu_c.json", b"cc!")
        .with_artifact(
            id,
            "tasks/toolu_t/metadata.json",
            format!(
                r#"{{"schema_version":1,"tool_use_id":"toolu_t","agent_id":"a1","originals":[{{"bytes":3,"original":"original_line_absent","sha256":"{}","tool_use_id":"toolu_in"}}]}}"#,
                digest(b"iii")
            )
            .as_bytes(),
        )
        .with_artifact(id, "tasks/toolu_t/tool-originals/toolu_in.json", b"iii")
}

/// A rebuild counts the originals beside both transcripts — the same from
/// every wiped projection, the orphan among the sealed — names an unsealed
/// one, and turns a body that no longer matches its digest into a parse issue.
#[tokio::test]
async fn a_rebuild_counts_the_same_originals_from_a_wiped_projection() {
    let sub = substrate_with_originals(&shard_id(9));
    let mut counts = Vec::new();
    for _ in 0..2 {
        let report = ingest_from_substrate(&store(), &sub, "repo", "claude-code", 1)
            .await
            .unwrap();
        assert_eq!(report.shards_projected, 1);
        assert!(report.parse_issues >= 1, "the edited body is named");
        assert!(
            report.render().contains("originals: 2 sealed, 1 unsealed"),
            "{}",
            report.render()
        );
        counts.push((report.originals_sealed, report.originals_unsealed));
    }
    assert_eq!(counts, [(2, 1), (2, 1)]);
}
