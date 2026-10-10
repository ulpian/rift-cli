//! The sidecar redirect, followed to what the client really ran: one
//! document, one amendment, and the two outcomes a claim may be made under.
//!
//! An amendment is a proposal. The client applies the last one it is handed,
//! so the read's own result is where Rift finds out whether the substitution
//! happened at all — and the figure a person is shown must contain a saving
//! only where it did. Both arms are driven here through the shipped binary
//! over the writers in `common/document_fixture.rs`; what a whole session over
//! the corpus removes is `document_savings`.
//!
//! Every claim here is about the document seam, and the binary carries that
//! seam behind the `doc` feature: a build without it converts nothing, so
//! there is no substitution to price. The suite is the feature's, and says so
//! rather than failing a build that never promised one.
#![cfg(feature = "doc")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

mod common;

#[allow(dead_code)] // The document suites share these builders; each file uses a subset.
#[path = "common/document_fixture.rs"]
mod document_fixture;
use document_fixture::{
    document_rows, docx, hook, json_str, plant_the_documents, prose, savings_json, seed_repo,
    state_a_patient_budget, warm_the_family, Member, Noise,
};

/// One container, to watch a single amendment through each of its two
/// outcomes: the same writer the corpus uses, at a size that declares plainly
/// more than nothing.
fn one_document() -> Member {
    let mut noise = Noise::new(0x0D0C_1234_5678_9ABC);
    Member {
        path: "design/plan.docx",
        format: "docx",
        role: "one amendment, followed to what the client really ran",
        prescribed: false,
        bytes: docx(&prose(&mut noise, 24, 14), None),
    }
}

/// A session that reads `member` once through the shipped binary, and the
/// path Rift amended the call to.
fn amended_read(repo: &Path, cache: &Path, member: &Member) -> String {
    state_a_patient_budget(repo);
    plant_the_documents(repo, std::slice::from_ref(member));
    warm_the_family(repo, cache);
    let call = format!(
        r#"{{"session_id":"doc-1","tool_name":"Read","tool_input":{{"file_path":{}}}}}"#,
        json_str(&repo.join(member.path))
    );
    hook(repo, "PreToolUse", &call, cache)
        .pointer("/hookSpecificOutput/updatedInput/file_path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .expect("the read of a document this family holds is amended")
}

/// The result of that read, as the client sends it: the input the tool really
/// ran with.
fn result_of(repo: &Path, cache: &Path, executed: &Path) {
    let payload = format!(
        r#"{{"session_id":"doc-1","tool_name":"Read","tool_input":{{"file_path":{}}},"tool_response":"ok"}}"#,
        json_str(executed)
    );
    hook(repo, "PostToolUse", &payload, cache);
}

/// ★ A substitution the client dropped claims nothing.
///
/// The client applies the last amendment it is handed for one event, so a
/// sibling hook can take a call Rift amended and the agent reads the container
/// in full. The read's own result is where that is found out — and the figure
/// a person is shown must not already contain a saving that result has just
/// contradicted. The document is one this fixture can price, so a zero here is
/// a claim withheld and not a document worth nothing.
#[test]
fn a_dropped_substitution_leaves_the_savings_at_zero_for_the_document_it_did_not_replace() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let member = one_document();
    let declared = rift_recall_serve::declared_document_baseline("docx", &member.bytes);
    assert!(declared > 0, "the document is worth claiming: {declared}");

    amended_read(&repo, &cache, &member);
    // The result names the DOCUMENT: Rift's amendment is not the one the
    // client kept, and the agent read every byte of the container.
    result_of(&repo, &cache, &repo.join(member.path));

    let booked = document_rows(&repo, &cache);
    assert!(
        booked.iter().any(|row| row[1] == "rewrite_dropped"),
        "the drop itself is on the record: {booked:?}"
    );
    assert!(
        booked.iter().all(|row| row[2] == "0"),
        "and no row claims a document the agent read: {booked:?}"
    );
    assert_eq!(
        savings_json(&repo, &cache)["overall"]["tokens_avoided"].as_u64(),
        Some(0),
        "the figure a person is shown credits nothing to a read that happened"
    );
}

/// ★ A substitution the client applied books the document's declared cost,
/// once.
///
/// The other half of the same claim: where the executed call names the sidecar
/// the read the agent wrote never happened, and that is the one arm of this
/// channel that may claim a saving. Two results for one amendment — a client
/// that repeats one, a hook that runs twice — still book it once.
#[test]
fn a_verified_substitution_books_the_declared_figure_once() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let member = one_document();
    let declared = rift_recall_serve::declared_document_baseline("docx", &member.bytes);

    let sidecar = amended_read(&repo, &cache, &member);
    result_of(&repo, &cache, Path::new(&sidecar));
    result_of(&repo, &cache, Path::new(&sidecar));

    let booked = document_rows(&repo, &cache);
    let mine: Vec<&Vec<String>> = booked
        .iter()
        .filter(|row| row[0].starts_with(&format!("file:{}@sha256:", member.path)))
        .collect();
    assert_eq!(mine.len(), 1, "one amendment, one row: {booked:?}");
    assert_eq!(
        mine[0][1],
        rift_recall_serve::TAG_DECLARED_BASELINE,
        "a declared figure says on the row that it is one"
    );
    assert_eq!(
        mine[0][2].parse::<u32>().unwrap(),
        declared,
        "and it is the same quantity this fixture prices the document at"
    );
    assert_eq!(
        savings_json(&repo, &cache)["overall"]["tokens_avoided"].as_u64(),
        Some(u64::from(declared)),
        "which is the figure a person is shown, counted once"
    );
}
