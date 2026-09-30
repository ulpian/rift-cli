//! The pull verb: the changed lines against the revision a caller holds, every
//! named way of not having them, and the older ranked shape.

use rift_core::Timestamp;
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::MemorySubstrate;
use rift_recall_types::{NoAncestry, NoProvenance, Scope};
use rift_store::SqliteStore;

use super::fixtures::*;
use crate::*;

/// The hunk verb ranks candidates through the same chokepoint, so it owes the
/// same self-declaration — and owes it most on the exit that found nothing.
///
/// `changed_hunks` picks the top *in-scope* path and reports its hunks. When
/// containment holds the family's other line back there is no top path at all,
/// and the payload it used to return (`hunks: []`, `omitted: 0`) is
/// indistinguishable from "your file has not changed". The block is what makes
/// the two different sentences; its absence when nothing was held back is the
/// same economy every other manifest keeps.
#[test]
fn the_hunk_verb_reports_what_containment_held_back() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let server = ServeDispatcher::new(PolicyEngine::new(&cfg));

    // An unknown HEAD: the fixture's one checkpoint is off the caller's line,
    // so no path survives ranking and the early exit is the one taken.
    let held = RecallContext {
        store: &store,
        reader: &reader,
        snapshots: None,
        scope: Scope::for_worktree(
            "repo",
            u32::MAX,
            None,
            rift_recall_types::WorktreeScope::Any,
        ),
        ancestry: &NoAncestry,
        provenance: &NoProvenance,
        now: Timestamp::from_millis(1_000_000_000),
    };
    let mut budget = TurnBudget::default();
    let resp =
        block(server.dispatch(ToolName::ChangedHunks, &ranking_req(), &held, &mut budget)).unwrap();
    assert_eq!(
        resp.payload["hunks"],
        serde_json::json!([]),
        "nothing off the caller's line is served: {}",
        resp.payload
    );
    let containment = &resp.payload["containment"];
    assert_eq!(containment["unknown_head"], serde_json::json!(true));
    assert!(
        containment["out_of_branch"].as_u64().unwrap() > 0,
        "an empty hunk list must say why it is empty: {}",
        resp.payload
    );
    assert_eq!(containment["cross_branch_grant"], serde_json::json!(false));

    // Nothing held back, nothing said — on this verb too.
    let mut budget = TurnBudget::default();
    let open = block(server.dispatch(
        ToolName::ChangedHunks,
        &ranking_req(),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    assert!(
        open.payload.get("containment").is_none(),
        "{}",
        open.payload
    );
}

/// Two revisions of one file, wired the way ingest wires them: a candidate row
/// at the current revision, the content→blob bridge for both, and a substrate
/// holding both blobs. `path` is the candidate's path, so a caller can ask
/// about it by name.
pub(super) fn two_revisions(
    path: &str,
    old: &[u8],
    new: &[u8],
    est_tokens: i64,
) -> (SqliteStore, MemorySubstrate) {
    let address = rift_recall_substrate::worktree::sha256_hex(new);
    two_revisions_addressed(path, old, new, est_tokens, &address)
}

/// The same fixture with the **recorded address** of the current revision
/// chosen by the caller, so a row that carries something other than a content
/// address — the shape a foreign or half-written projection has — can be built
/// on purpose.
fn two_revisions_addressed(
    path: &str,
    old: &[u8],
    new: &[u8],
    est_tokens: i64,
    current_address: &str,
) -> (SqliteStore, MemorySubstrate) {
    use rift_recall_substrate::worktree::{git_blob_oid, sha256_hex, WorktreeBlob};
    let s = open_store();
    seed_checkpoint(&s, "c1", 1);
    seed_revision(
        &s,
        "rev_new",
        path,
        &git_blob_oid(new),
        current_address,
        i64::try_from(new.len()).unwrap(),
    );
    seed_read(&s, "c1", "rev_new", est_tokens, 1);
    block(rift_recall_ledger::blob_map::persist_blob_oid_map(
        &s,
        &[
            WorktreeBlob {
                path: path.to_owned(),
                content_sha256: sha256_hex(old),
                blob_oid: git_blob_oid(old),
            },
            WorktreeBlob {
                path: path.to_owned(),
                content_sha256: current_address.to_owned(),
                blob_oid: git_blob_oid(new),
            },
        ],
        0,
    ))
    .unwrap();
    let reader = MemorySubstrate::local()
        .with_blob(&git_blob_oid(old), old)
        .with_blob(&git_blob_oid(new), new);
    (s, reader)
}

/// A source file of twenty small indented blocks. Two properties matter: a
/// change inside one block has an edge to grow to, and the rest of the file is
/// large enough that not sending it is worth more than the answer costs —
/// which is the whole claim this verb makes.
pub(super) fn source_file(marker: &str) -> Vec<u8> {
    let mut out = String::from("use std::io;\n\n");
    for b in 0..20 {
        out.push_str(&format!("fn f{b}() {{\n"));
        for n in 1..=6 {
            let mark = if b == 0 { marker } else { "" };
            out.push_str(&format!("    let x{b}_{n} = {mark}{n};\n"));
        }
        out.push_str("}\n\n");
    }
    out.into_bytes()
}

pub(super) fn pull(path: &str, basis: &str) -> ToolRequest {
    ToolRequest {
        target: Some(path.to_owned()),
        known_hashes: vec![basis.to_owned()],
        ..Default::default()
    }
}

/// The answer is about the file, not about the last checkpoint of it.
///
/// This is the live shape the verb was reported with: a caller holds the
/// newest revision the projection carries, the file has been edited since, and
/// the old current-side — the projection's newest revision — made the two
/// hashes equal, so the verb said "you already hold this" about a file that had
/// moved. A caller believes that and skips the re-read. The current side is now
/// the checkout, so the same call serves the changed lines instead.
#[test]
fn the_pull_diffs_the_basis_against_the_current_file_not_a_checkpoint() {
    use rift_recall_substrate::worktree::sha256_hex;
    let old = source_file("");
    let recorded = source_file("9");
    let edited = source_file("7");
    let (store, reader) = two_revisions("src/a.rs", &old, &recorded, 400);

    // The checkout has moved past every revision anyone recorded.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/a.rs"), &edited).unwrap();
    let reader = reader.with_worktree_root(tmp.path());

    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    let held = sha256_hex(&recorded);
    let mut budget = TurnBudget::new(8, 4000);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &held[..basis::MIN_BASIS_HEX]),
        &c,
        &mut budget,
    ))
    .unwrap();

    assert_eq!(
        resp.payload["state"],
        serde_json::json!("exact_included"),
        "the file moved, so there are changed lines to serve: {:?}",
        resp.payload
    );
    assert_eq!(
        resp.payload["current_hash"],
        serde_json::json!(sha256_hex(&edited)),
        "the current revision is the one on disk"
    );
    assert_eq!(resp.payload["basis_sha256"], serde_json::json!(held));
    let content = resp.payload["content"].as_str().expect("the changed lines");
    assert!(content.contains("let x0_1 = 71;"), "{content}");
    assert!(
        !content.contains("= 91;"),
        "the recorded revision is the basis, not the answer: {content}"
    );
    assert!(resp.estimated_tokens_avoided > 0);

    // The counterfactual, on the same store: a reader with no checkout to look
    // in has only the projection to answer from, and there the caller's hash IS
    // the newest revision — so it is told it holds the current bytes. That is
    // the fallback arm, and it is the shape the checkout removes.
    let (_, blind_reader) = two_revisions("src/a.rs", &old, &recorded, 400);
    let blind = ctx(&store, &blind_reader);
    let mut budget = TurnBudget::new(8, 4000);
    let stale = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &held[..basis::MIN_BASIS_HEX]),
        &blind,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(stale.payload["state"], serde_json::json!("known_unchanged"));
}

/// The pull the manifest's own hint teaches, on a twenty-block file whose
/// first block moved: the two revisions, and the response to sixteen hex —
/// exactly what `rift recall changed-hunks <path> --known-hash <16hex> --json`
/// puts on the command line.
struct HeldHashPull {
    store: SqliteStore,
    reader: MemorySubstrate,
    old: Vec<u8>,
    new: Vec<u8>,
    resp: ToolResponse,
}

fn held_hash_pull() -> HeldHashPull {
    use rift_recall_substrate::worktree::sha256_hex;
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("src/a.rs", &old, &new, 400);
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let held = sha256_hex(&old);
    let mut budget = TurnBudget::new(8, 4000);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &held[..basis::MIN_BASIS_HEX]),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    HeldHashPull {
        store,
        reader,
        old,
        new,
        resp,
    }
}

/// ★ The verb the manifest's own hint teaches: a path, the revision the caller
/// holds, and back come the changed lines of the current file — numbered in
/// the numbering the caller will see if it opens the file, with the unchanged
/// runs counted rather than sent. No task, no focus set: the call the hint
/// teaches is accepted as it is written, and nothing about it ranked.
#[test]
fn changed_hunks_against_the_held_hash_serves_the_diff_content() {
    use rift_recall_substrate::worktree::sha256_hex;
    let HeldHashPull { old, new, resp, .. } = held_hash_pull();

    assert_eq!(resp.payload["state"], serde_json::json!("exact_included"));
    assert_eq!(
        resp.payload["basis_sha256"],
        serde_json::json!(sha256_hex(&old))
    );
    assert_eq!(
        resp.payload["current_hash"],
        serde_json::json!(sha256_hex(&new))
    );
    assert_eq!(resp.payload["path"], serde_json::json!("src/a.rs"));

    let content = resp.payload["content"].as_str().expect("the changed lines");
    // The changed lines, in the CURRENT numbering, with their window.
    assert!(content.contains("4:     let x0_1 = 91;"), "{content}");
    assert!(content.contains("9:     let x0_6 = 96;"), "{content}");
    assert_eq!(
        resp.payload["hunks"][0]["window"],
        serde_json::json!("block")
    );
    assert_eq!(resp.payload["hunks"][0]["lo"], serde_json::json!(3));
    assert_eq!(resp.payload["hunks"][0]["hi"], serde_json::json!(10));
    assert_eq!(resp.payload["omitted"], serde_json::json!(0));
    assert_eq!(resp.payload["truncated"], serde_json::json!(false));
    assert_eq!(resp.payload["shift"], serde_json::json!(0));
    // The unchanged half of the file is counted, not sent.
    assert!(content.contains("… (2 unchanged lines)"), "{content}");
    assert!(content.contains("… (172 unchanged lines)"), "{content}");
    assert!(!content.contains("let x9_1"), "{content}");
}

/// ★ The figure is the point, and it rests on a read Rift recorded. The
/// caller's basis here is a revision the bridge maps but no read landed on, so
/// the changed lines are served and nothing is credited: the hash is the
/// caller's word, not evidence the bytes were ever in a window. The caller
/// that holds the current bytes — which this family did read — is told so by
/// the four-state constructor, and the whole file is what it did not re-read,
/// measured at the seam over the exact bytes the transport ships.
#[test]
fn changed_hunks_credits_only_a_basis_this_family_recorded_reading() {
    use rift_recall_substrate::worktree::sha256_hex;
    let HeldHashPull {
        store,
        reader,
        new,
        resp,
        ..
    } = held_hash_pull();

    // The accounting: the payload is measured, and an unrecorded basis
    // earns nothing — the answer says why.
    assert_eq!(
        resp.estimated_tokens_returned,
        rift_tokens::count(&resp.payload.to_string())
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
    assert_eq!(
        resp.payload["avoided_basis"],
        serde_json::json!("stated_unrecorded")
    );
    assert_eq!(resp.event.tokens_avoided(), resp.estimated_tokens_avoided);

    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(8, 4000);
    let same = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &sha256_hex(&new)),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    assert_eq!(same.payload["state"], serde_json::json!("known_unchanged"));
    assert!(same.payload["content"].is_null(), "{}", same.payload);
    assert!(
        same.payload.get("avoided_basis").is_none(),
        "{}",
        same.payload
    );
    assert_eq!(
        same.estimated_tokens_avoided,
        400u32.saturating_sub(same.estimated_tokens_returned)
    );
    assert!(same.estimated_tokens_avoided > 0);
}

/// ★ Bytes whose provenance is not a tracked file's blob are counted, never
/// quoted. The trust class comes from the artifact's address, not from the
/// caller and not from the fact that the bytes turned out to be reachable.
///
/// The address here is a git object id recorded where a content address
/// belongs — the shape a half-written or foreign projection row has. It names
/// no tracked blob this tree can vouch for, so the lines it leads to are
/// summarized rather than served, exactly as an untrusted tool output is.
#[test]
fn untrusted_provenance_is_digested_not_served() {
    use rift_recall_substrate::worktree::{git_blob_oid, sha256_hex};
    let old = source_file("");
    let new = source_file("9");
    // The current revision is recorded by its git object id where a content
    // address belongs. The bytes are the same and still reachable; what is
    // gone is the address that made them a code fact.
    let (store, reader) = two_revisions_addressed("src/a.rs", &old, &new, 400, &git_blob_oid(&new));
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    let held = sha256_hex(&old);

    let mut budget = TurnBudget::new(8, 4000);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &held),
        &c,
        &mut budget,
    ))
    .unwrap();

    let rendered = resp.payload.to_string();
    assert_eq!(
        resp.payload["state"],
        serde_json::json!("refused"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("let x0_1 = 91;"),
        "a changed line escaped through an untrusted address: {rendered}"
    );
    assert!(resp.payload["content"].is_null());
    // What the caller gets instead: counts, and the neutral kind tag.
    assert_eq!(
        resp.payload["neutral_digest"]["kind"],
        serde_json::json!("file_content"),
        "{rendered}"
    );
    assert!(
        resp.payload["neutral_digest"]["facts"]
            .as_array()
            .is_some_and(|f| !f.is_empty()),
        "{rendered}"
    );
    // And nothing is claimed as avoided for bytes that were never served.
    assert_eq!(resp.estimated_tokens_avoided, 0);
}

/// A path the policy denies is refused before a candidate is loaded — the same
/// screen every other serve applies, on the verb that would otherwise be the
/// cheapest way around it.
#[test]
fn a_denied_path_is_refused_by_the_pull_verb_too() {
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("../outside/evil.rs", &old, &new, 400);
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    let held = rift_recall_substrate::worktree::sha256_hex(&old);
    let mut budget = TurnBudget::new(8, 4000);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("../outside/evil.rs", &held),
        &c,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(resp.payload["state"], serde_json::json!("refused"));
    assert_eq!(
        resp.payload["reason"],
        serde_json::json!("policy_denied_exact")
    );
    assert!(!resp.payload.to_string().contains("let x0_1 = 91;"));
    assert_eq!(resp.estimated_tokens_avoided, 0);
}

/// ★ **The pull verb never serves the live bytes of a span the gate masked in
/// the basis.**
///
/// The second rung of the basis ladder is a redacted snapshot, so a line the
/// gate masked there differs from the live file whether or not the repository
/// touched it — and the "changed lines" this verb would serve for it are the
/// *live* bytes of exactly that span. The answer is the neutral digest every
/// untrusted payload leaves by: counts and a handle, never the text.
#[test]
fn a_hunk_that_rests_on_a_masked_basis_is_served_as_counts() {
    use rift_recall_substrate::worktree::{git_blob_oid, sha256_hex};
    let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
    let mut old = String::from_utf8(source_file("")).unwrap();
    old.push_str(&format!("const TOKEN: &str = \"{secret}\";\n"));
    let new = format!("{old}fn tail() {{}}\n");
    let (store, _) = two_revisions("src/a.rs", old.as_bytes(), new.as_bytes(), 400);
    // The basis is recorded in the bridge — a caller may name it — and its
    // blob is NOT in the substrate: the first rung answers nothing, which is
    // the state an uncommitted revision is always in.
    let reader = MemorySubstrate::local().with_blob(&git_blob_oid(new.as_bytes()), new.as_bytes());

    let tmp = tempfile::tempdir().unwrap();
    let snapshots = crate::basis_store::BasisStore::for_family(tmp.path());
    let sealed = rift_shadow_redact::redact(&rift_core::SecretBytes::new(old.clone().into_bytes()))
        .unwrap()
        .redacted;
    let masked: Vec<(u32, u32)> = rift_recall_ledger::hunks::split_lines(old.as_bytes())
        .iter()
        .zip(rift_recall_ledger::hunks::split_lines(sealed.as_slice()))
        .enumerate()
        .filter(|(_, (a, b))| **a != *b)
        .map(|(i, _)| {
            let n = u32::try_from(i + 1).unwrap();
            (n, n)
        })
        .collect();
    assert!(!masked.is_empty(), "the fixture's token is a secret");
    snapshots
        .put(
            &sealed,
            &crate::basis_store::SnapshotMeta {
                schema_version: crate::basis_store::SNAPSHOT_SCHEMA_VERSION,
                content_sha256: sha256_hex(old.as_bytes()),
                path: "src/a.rs".to_owned(),
                redacted_sha256: sealed.redacted_sha256().to_hex(),
                policy_fingerprint: "b3:fixture".to_owned(),
                rule_packs: sealed.provenance().rule_pack_versions.clone(),
                line_count: crate::basis_store::line_count(old.as_bytes()),
                redacted_lines: masked,
                captured_at_ms: 1,
            },
        )
        .unwrap();

    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = RecallContext {
        store: &store,
        reader: &reader,
        snapshots: Some(&snapshots),
        scope: Scope::unbounded("repo"),
        ancestry: &NoAncestry,
        provenance: &NoProvenance,
        now: Timestamp::from_millis(1_000_000_000),
    };
    let mut budget = TurnBudget::new(8, 4000);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &sha256_hex(old.as_bytes())),
        &c,
        &mut budget,
    ))
    .unwrap();
    let body = resp.payload.to_string();
    assert_eq!(resp.payload["state"], serde_json::json!("refused"));
    assert_eq!(
        resp.payload["reason"],
        serde_json::json!("source_deleted_or_redacted")
    );
    assert!(
        !resp.payload["neutral_digest"].is_null(),
        "counts, not silence: {body}"
    );
    assert!(
        !body.contains(secret),
        "the live secret never ships: {body}"
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
}

/// Every way of not having an answer has its own name, and none of them is
/// silence or a guess.
#[test]
fn each_way_of_not_knowing_the_basis_has_its_own_name() {
    use rift_recall_substrate::worktree::sha256_hex;
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("src/a.rs", &old, &new, 400);
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    let state = |req: &ToolRequest| {
        let mut budget = TurnBudget::new(8, 4000);
        let resp = block(s.dispatch(ToolName::ChangedHunks, req, &c, &mut budget)).unwrap();
        (
            resp.payload["state"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            resp.payload.clone(),
            resp.estimated_tokens_avoided,
        )
    };

    // A prefix nothing carries: the basis is not known, and the file is
    // certainly not reported unchanged.
    let (name, _, avoided) = state(&pull("src/a.rs", &"c".repeat(20)));
    assert_eq!(name, "unknown_basis");
    assert_eq!(avoided, 0);

    // A revision the bridge maps but the substrate no longer holds.
    let empty_substrate = MemorySubstrate::local();
    let forgotten = ctx(&store, &empty_substrate);
    let mut budget = TurnBudget::new(8, 4000);
    let gone = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &sha256_hex(&old)),
        &forgotten,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(
        gone.payload["state"],
        serde_json::json!("basis_unavailable")
    );

    // A path this scope holds no revision of.
    let (name, _, _) = state(&pull("src/nowhere.rs", &sha256_hex(&old)));
    assert_eq!(name, "invalid");

    // A file that moved in more places than a windowed answer stays smaller
    // than the file for: the count is said, the lines are not.
    let scattered: Vec<u8> = (0..40)
        .flat_map(|n| format!("line {n}\n").into_bytes())
        .collect();
    let rewritten: Vec<u8> = (0..40)
        .flat_map(|n| {
            if n % 3 == 0 {
                format!("changed {n}\n").into_bytes()
            } else {
                format!("line {n}\n").into_bytes()
            }
        })
        .collect();
    let (store2, reader2) = two_revisions("src/w.rs", &scattered, &rewritten, 400);
    let c2 = ctx(&store2, &reader2);
    let mut budget = TurnBudget::new(8, 4000);
    let diffuse = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/w.rs", &sha256_hex(&scattered)),
        &c2,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(diffuse.payload["state"], serde_json::json!("diffuse"));
    assert!(diffuse.payload["places"].as_u64().unwrap_or(0) > 8);
    assert!(diffuse.payload["content"].is_null());
    assert!(!diffuse.payload.to_string().contains("changed 3"));
}

/// The verb without a basis is the verb it was: ranked candidates, the top
/// path's recorded ranges, and the query-aware params it has always required.
#[test]
fn the_verb_without_a_basis_is_unchanged() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);

    let mut budget = TurnBudget::default();
    let ranked =
        block(s.dispatch(ToolName::ChangedHunks, &ranking_req(), &c, &mut budget)).unwrap();
    assert_eq!(ranked.payload["path"], serde_json::json!("src/a.rs"));
    assert!(
        ranked.payload["source_hash"].is_string(),
        "{}",
        ranked.payload
    );

    // A path with no basis still ranks, so it still owes the ranking params.
    let mut budget = TurnBudget::default();
    let bare = ToolRequest {
        target: Some("src/a.rs".into()),
        ..Default::default()
    };
    assert_eq!(
        block(s.dispatch(ToolName::ChangedHunks, &bare, &c, &mut budget)),
        Err(ToolError::QueryParamsRequired)
    );
}

/// ★ A sensitive path is not served exact by the pull verb.
///
/// `config/.env.production` matches the built-in, non-droppable `**/.env*`
/// glob, so it is exactly the class the standing rule covers: its exact source
/// is reached through the audited `retrieve_exact` — which demands a reason
/// and books a grant — or not at all. This verb takes no reason and books no
/// grant, so what it may hand back is the state and the changed places, never
/// the changed lines. The fixture asserts the premise (the path really is
/// sensitive under the shipped config) so the test cannot pass because the
/// glob moved.
#[test]
fn a_sensitive_path_is_digested_by_the_pull_verb() {
    use rift_recall_substrate::worktree::sha256_hex;
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("config/.env.production", &old, &new, 400);
    let cfg = cfg();
    let policy = PolicyEngine::new(&cfg);
    assert!(
        policy.exact_source_required("config/.env.production"),
        "the fixture's premise: the shipped config treats this path as sensitive"
    );
    let s = ServeDispatcher::new(policy);
    let c = ctx(&store, &reader);

    let mut budget = TurnBudget::new(8, 4000);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("config/.env.production", &sha256_hex(&old)),
        &c,
        &mut budget,
    ))
    .unwrap();

    let rendered = resp.payload.to_string();
    assert_ne!(
        resp.payload["state"],
        serde_json::json!("exact_included"),
        "a sensitive path was inlined by a verb that asks for no reason: {rendered}"
    );
    assert!(resp.payload["content"].is_null(), "{rendered}");
    assert!(
        !rendered.contains("let x0_1 = 91;"),
        "a changed line of a sensitive file escaped: {rendered}"
    );
    // And the answer says which gate held them back.
    assert_eq!(
        resp.payload["reason"],
        serde_json::json!("sensitive_path_requires_reason"),
        "{rendered}"
    );
    // The same fixture at an ordinary path still serves, so the screen is the
    // path's sensitivity and not something about this fixture.
    let (store2, reader2) = two_revisions("src/a.rs", &old, &new, 400);
    let c2 = ctx(&store2, &reader2);
    let mut budget = TurnBudget::new(8, 4000);
    let ok = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &sha256_hex(&old)),
        &c2,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(ok.payload["state"], serde_json::json!("exact_included"));
}

/// A policy Rift could not read serves no content here either.
///
/// The fail-closed engine carries an EMPTY deny list — it is the state of
/// knowing nothing, not the state of denying nothing — so a verb that consults
/// only the deny list reads an unparseable `serve.toml` as permission. The
/// refusal is the plain denial tag the other verbs use, which says no without
/// saying which half of the question was the problem.
#[test]
fn a_fail_closed_policy_refuses_the_pull_verb() {
    use rift_recall_policy::ServePolicy;
    use rift_recall_substrate::worktree::sha256_hex;
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("src/a.rs", &old, &new, 400);
    let cfg = cfg();
    let unreadable = ServePolicy {
        fail_closed: true,
        ..ServePolicy::default()
    };
    assert!(
        unreadable.denied_paths.is_empty(),
        "the premise: fail-closed denies nothing by name, so the deny list \
         cannot be what refuses this call"
    );
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg).with_serve_policy(unreadable));
    let c = ctx(&store, &reader);

    let mut budget = TurnBudget::new(8, 4000);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &sha256_hex(&old)),
        &c,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(resp.payload["state"], serde_json::json!("refused"));
    assert_eq!(
        resp.payload["reason"],
        serde_json::json!("policy_denied_exact")
    );
    assert!(resp.payload["content"].is_null());
    assert!(!resp.payload.to_string().contains("let x0_1 = 91;"));
    assert_eq!(resp.estimated_tokens_avoided, 0);
}

/// Record one more file's revision in an existing fixture store — a row in
/// both relations a basis can be resolved from, and its bytes in the
/// substrate — so a caller can name an address the family really holds *for a
/// different file* and the answer has to be about which file, not about
/// whether the address is real.
fn with_unrelated_revision(
    store: &SqliteStore,
    reader: MemorySubstrate,
    path: &str,
    bytes: &[u8],
) -> MemorySubstrate {
    use rift_recall_substrate::worktree::{git_blob_oid, sha256_hex, WorktreeBlob};
    seed_revision(
        store,
        &format!("rev_other_{path}"),
        path,
        &git_blob_oid(bytes),
        &sha256_hex(bytes),
        i64::try_from(bytes.len()).unwrap(),
    );
    block(rift_recall_ledger::blob_map::persist_blob_oid_map(
        store,
        &[WorktreeBlob {
            path: path.to_owned(),
            content_sha256: sha256_hex(bytes),
            blob_oid: git_blob_oid(bytes),
        }],
        0,
    ))
    .unwrap();
    reader.with_blob(&git_blob_oid(bytes), bytes)
}

/// ★ An address is half a name. A content address the family really holds —
/// for a different file — is not a basis for this one.
///
/// The caller chooses both halves of `<path> --known-hash <address>`, so a
/// resolver that asks only "does the family know this address" lets any
/// address seen anywhere be paired with any path. The two revisions then have
/// nothing to do with each other, the diff is one whole-file change, and the
/// answer is the named file rendered from line 1. The conservative arm is the
/// one the design already names: the basis is not known.
#[test]
fn a_basis_recorded_for_another_path_is_unknown() {
    use rift_recall_substrate::worktree::sha256_hex;
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("src/a.rs", &old, &new, 400);
    // A real revision of a real, unrelated file, reachable in the substrate —
    // so an unbound resolver would resolve it AND find its bytes.
    let other = b"# notes\n\nnothing to do with src/a.rs\n".to_vec();
    let reader = with_unrelated_revision(&store, reader, "docs/other.md", &other);
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);

    for given in [
        sha256_hex(&other),
        sha256_hex(&other)[..basis::MIN_BASIS_HEX].to_owned(),
    ] {
        let mut budget = TurnBudget::new(8, 4000);
        let resp = block(s.dispatch(
            ToolName::ChangedHunks,
            &pull("src/a.rs", &given),
            &c,
            &mut budget,
        ))
        .unwrap();
        let rendered = resp.payload.to_string();
        assert_eq!(
            resp.payload["state"],
            serde_json::json!("unknown_basis"),
            "another file's address resolved as this file's basis: {rendered}"
        );
        assert!(resp.payload["content"].is_null(), "{rendered}");
        assert!(!rendered.contains("let x0_1 = 91;"), "{rendered}");
        assert_eq!(resp.estimated_tokens_avoided, 0);
    }

    // The control: this file's own old revision, by the same two spellings,
    // still resolves and still serves.
    for given in [
        sha256_hex(&old),
        sha256_hex(&old)[..basis::MIN_BASIS_HEX].to_owned(),
    ] {
        let mut budget = TurnBudget::new(8, 4000);
        let resp = block(s.dispatch(
            ToolName::ChangedHunks,
            &pull("src/a.rs", &given),
            &c,
            &mut budget,
        ))
        .unwrap();
        assert_eq!(
            resp.payload["state"],
            serde_json::json!("exact_included"),
            "{given}"
        );
    }
}

/// **Both halves of the address, on the pull verb.** The two screens above are
/// one attack when they are combined: an unrelated file's content address,
/// passed as the basis for a sensitive path, with a token budget the caller
/// sets itself. Unbound, that pair diffs as a single whole-file change and
/// renders the sensitive file from line 1 — a whole-file read of a policy-
/// sensitive file through a verb that requires no reason and books no grant.
/// It is the hash-only resolution `retrieve_exact` already refuses, and this
/// row keeps it refused in the handler that serves lines.
#[test]
fn red_team_an_unrelated_address_cannot_render_a_sensitive_file_from_line_one() {
    use rift_recall_substrate::worktree::sha256_hex;
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("config/.env.production", &old, &new, 400);
    let other = b"# notes\n\nnothing to do with the environment file\n".to_vec();
    let reader = with_unrelated_revision(&store, reader, "docs/other.md", &other);
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);

    // The caller's own budget, large enough for the whole file.
    let req = ToolRequest {
        target: Some("config/.env.production".into()),
        known_hashes: vec![sha256_hex(&other)],
        token_budget: 100_000,
        ..Default::default()
    };
    let mut budget = TurnBudget::new(8, 100_000);
    let resp = block(s.dispatch(ToolName::ChangedHunks, &req, &c, &mut budget)).unwrap();
    let rendered = resp.payload.to_string();
    assert_eq!(
        resp.payload["state"],
        serde_json::json!("unknown_basis"),
        "{rendered}"
    );
    assert!(resp.payload["content"].is_null(), "{rendered}");
    // Not one line of the file, from either end of it.
    for span in ["let x0_1 = 91;", "let x19_6 = 6;", "use std::io;"] {
        assert!(
            !rendered.contains(span),
            "a line of a sensitive file escaped through a foreign basis: {rendered}"
        );
    }
    assert_eq!(resp.estimated_tokens_avoided, 0);
}

/// The pull verb answers about the file's CURRENT revision, not whichever
/// revision of it the candidate rows happened to yield first.
///
/// Candidates are keyed by file revision, so a path that was read twice has
/// two rows and the older one is as findable as the newer. A verb that takes
/// either tells a caller holding the current bytes that its basis cannot be
/// reached, and a caller holding the old ones that nothing has changed — both
/// of them confidently, and both of them wrong.
#[test]
fn the_pull_serves_the_newest_revision_of_the_path() {
    use rift_recall_substrate::worktree::{git_blob_oid, sha256_hex};
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("src/a.rs", &old, &new, 400);
    // A SECOND, older read of the same path: an earlier checkpoint, the old
    // revision, wired exactly as ingest would have written it at the time. Its
    // id sorts BEFORE the current one's on purpose — a selection that takes
    // whichever row comes first would take this one, and the fixture would
    // otherwise pass by luck of the alphabet.
    seed_revision(
        &store,
        "rev_a_old",
        "src/a.rs",
        &git_blob_oid(&old),
        &sha256_hex(&old),
        i64::try_from(old.len()).unwrap(),
    );
    seed_checkpoint(&store, "c0", 0);
    seed_read(&store, "c0", "rev_a_old", 400, 1);

    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);

    // The caller holding the OLD bytes is served the changed lines.
    let mut budget = TurnBudget::new(8, 4000);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &sha256_hex(&old)),
        &c,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(
        resp.payload["state"],
        serde_json::json!("exact_included"),
        "{}",
        resp.payload
    );
    assert_eq!(
        resp.payload["current_hash"],
        serde_json::json!(sha256_hex(&new)),
        "the newest revision is the current one"
    );

    // And the caller holding the CURRENT bytes is told so, rather than being
    // told its basis could not be reached.
    let mut budget = TurnBudget::new(8, 4000);
    let same = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &sha256_hex(&new)),
        &c,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(
        same.payload["state"],
        serde_json::json!("known_unchanged"),
        "{}",
        same.payload
    );
}
