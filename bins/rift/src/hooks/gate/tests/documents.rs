//! The document gate: a read of a container nobody can read as lines is
//! answered with the Markdown Rift holds for it, or redirected to it, or left
//! alone — and what each of those books.
//!
//! The module itself is declared behind the `doc` feature, so nothing in here
//! repeats that gate per item.

use std::path::{Path, PathBuf};

use rift_shadow::hooks::{AdapterRegistry, RawHook};

use super::fixtures::*;
use crate::hooks::doc_gate::{doc_gate_in, DocAnswer, DocOutcome};

/// A converter that answers whatever this test says, and counts how often
/// it was asked.
///
/// The counting is half the claim in two of the tests below: "decided
/// against once, for the whole family" and "the family's answer is linked,
/// not re-parsed" are both statements about how many times a parser ran.
struct StandInConverter {
    answer: std::result::Result<rift_recall_doc::Markdown, rift_recall_doc::DocError>,
    calls: std::sync::atomic::AtomicUsize,
}

impl StandInConverter {
    fn markdown(text: &str) -> Self {
        Self::answering(Ok(rift_recall_doc::Markdown {
            text: text.to_owned(),
            ..rift_recall_doc::Markdown::default()
        }))
    }

    fn answering(
        answer: std::result::Result<rift_recall_doc::Markdown, rift_recall_doc::DocError>,
    ) -> Self {
        Self {
            answer,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl rift_recall_doc::DocConverter for StandInConverter {
    fn convert(
        &self,
        _bytes: &[u8],
        _ext: &str,
        _caps: &rift_recall_doc::ConvertCaps,
    ) -> std::result::Result<rift_recall_doc::Markdown, rift_recall_doc::DocError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.answer.clone()
    }
}

/// A checkout holding one document, and the family store beside it.
fn documents(name: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf, crate::doc_store::DocStore) {
    let tmp = tempfile::tempdir().expect("a work tree");
    let work_tree = tmp.path().join("main");
    let common_dir = tmp.path().join("common");
    std::fs::create_dir_all(&work_tree).expect("the checkout");
    std::fs::create_dir_all(&common_dir).expect("the common dir");
    let document = work_tree.join(name);
    std::fs::write(&document, bytes).expect("the document");
    let store = crate::doc_store::DocStore::for_member(&common_dir, &work_tree);
    (tmp, document, store)
}

/// The document levers as this repository ships them, for one family.
fn document_levers() -> (
    rift_recall_policy::ServePolicy,
    rift_recall_policy::Effective,
) {
    let policy = rift_recall_policy::ServePolicy::default();
    let effective = rift_recall_policy::resolve(
        &policy,
        "claude-code",
        crate::recall::family_capability(&rift_core::AgentId::CLAUDE_CODE),
    );
    (policy, effective)
}

/// One document read through the gate, against a store and a converter the
/// test owns.
fn doc_gate_for(
    raw: &RawHook,
    work_tree: &Path,
    store: &crate::doc_store::DocStore,
    converter: Option<&dyn rift_recall_doc::DocConverter>,
) -> DocOutcome {
    let (policy, effective) = document_levers();
    doc_gate_in(
        &crate::hooktime::HookTimer::new("test", false),
        &AdapterRegistry::with_builtin(),
        raw,
        &work_tree.join(".git"),
        work_tree,
        &policy,
        &effective,
        store,
        converter,
    )
}

/// ★ A read of a document is answered with the Markdown Rift holds for it:
/// the family store gains the answer, this member gains its view, and the
/// response the client actually receives names that view in the one field
/// the pinned channel allows to be amended.
#[test]
fn a_read_of_a_docx_is_rewritten_to_its_sidecar() {
    let (tmp, document, store) = documents("spec.docx", b"PK\x03\x04 a document");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Spec\n\nThe body.\n");
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-1",
        read_call(&document, None, None),
    );

    let outcome = doc_gate_for(&call, &work_tree, &store, Some(&converter));
    let Some(DocAnswer::Rewrite {
        tool_input,
        sidecar,
    }) = outcome.answer.clone()
    else {
        panic!("a document read is answered with its sidecar: {outcome:?}");
    };
    assert_eq!(converter.calls(), 1, "converted once");
    assert!(
        Path::new(&sidecar).starts_with(work_tree.join(".rift/state/doc")),
        "the agent is pointed at a path inside its own checkout: {sidecar}"
    );
    assert!(
        std::fs::read_to_string(&sidecar)
            .expect("this member's view")
            .contains("The body."),
        "the view carries the conversion"
    );

    // The whole response, composed where every response is composed: the
    // one amended field is the path, and nothing else of the call moved.
    let response = rift_agent::respond::gate(
        &rift_core::AgentId::CLAUDE_CODE,
        "PreToolUse",
        &rift_agent::respond::GateDecision::Rewrite(
            rift_agent::respond::Rewrite::DocumentSidecar {
                tool_input: &tool_input,
                file_path: &sidecar,
            },
        ),
    )
    .expect("the amended call");
    assert_eq!(
        response.pointer("/hookSpecificOutput/updatedInput/file_path"),
        Some(&serde_json::Value::from(sidecar.as_str()))
    );
    assert_eq!(
        response.pointer("/hookSpecificOutput/permissionDecision"),
        Some(&serde_json::Value::from("allow")),
        "a substitution declines nothing"
    );
    assert!(
        !response.to_string().contains("The body."),
        "a hook response carries a path and never the bytes at it"
    );

    // A second read of the same document is one `stat` and a link that is
    // already there: the family paid the parser once.
    let again = doc_gate_for(&call, &work_tree, &store, Some(&converter));
    assert!(matches!(again.answer, Some(DocAnswer::Rewrite { .. })));
    assert_eq!(converter.calls(), 1, "the family's answer is not re-parsed");
}

/// ★ Two documents no substitution may touch. A PDF whose conversion is
/// page images rather than text is decided against once for the whole
/// family and never parsed again; a read that addresses `pages` is not a
/// read of the document at all, and carries a range the sidecar has no
/// answer for.
#[test]
fn a_read_of_a_scanned_pdf_or_with_pages_is_left_alone() {
    let (tmp, document, store) = documents("scan.pdf", b"%PDF-1.7 pages of pictures");
    let work_tree = tmp.path().join("main");
    // Four pages, and the text a scanner's OCR-less extract leaves.
    let scanned = StandInConverter::answering(Ok(rift_recall_doc::Markdown {
        text: "1\n2\n3\n4\n".to_owned(),
        pages: Some(4),
        thin_pages: vec![1, 2, 3, 4],
        ..rift_recall_doc::Markdown::default()
    }));
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-1",
        read_call(&document, None, None),
    );

    let outcome = doc_gate_for(&call, &work_tree, &store, Some(&scanned));
    assert!(
        outcome.answer.is_none(),
        "a page image is not text, and Markdown that says so is worse than the page"
    );
    assert_eq!(scanned.calls(), 1);
    let outcome = doc_gate_for(&call, &work_tree, &store, Some(&scanned));
    assert!(outcome.answer.is_none());
    assert_eq!(
        scanned.calls(),
        1,
        "the decision against it is the family's, and it is not re-parsed"
    );

    // The same file, converted to real text, read with `pages`.
    let (tmp, document, store) = documents("report.pdf", b"%PDF-1.7 words");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Report\n\nWords.\n");
    let mut input = serde_json::Map::new();
    input.insert(
        "file_path".to_owned(),
        serde_json::json!(document.display().to_string()),
    );
    input.insert("pages".to_owned(), serde_json::json!("1-5"));
    let paged = hook(
        "claude-code",
        "PreToolUse",
        "s-2",
        serde_json::json!({ "tool_name": "Read", "tool_input": input }),
    );
    let outcome = doc_gate_for(&paged, &work_tree, &store, Some(&converter));
    assert!(
        outcome.answer.is_none(),
        "a call that addresses pages is not a call the sidecar can answer"
    );
    assert_eq!(converter.calls(), 0, "and it is not even parsed");
}

/// A conversion that does not finish inside the budget leaves the read
/// exactly as the agent wrote it and hands the work to the detached
/// worker: the deadline is a bound on what an agent waits for, never a
/// reason to answer with something that is not ready.
#[test]
fn a_conversion_over_the_deadline_leaves_the_read_alone_and_journals() {
    let (tmp, document, store) = documents("slow.docx", b"PK\x03\x04 a slow document");
    let work_tree = tmp.path().join("main");
    let timed_out =
        StandInConverter::answering(Err(rift_recall_doc::DocError::Engine { code: "deadline" }));
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-1",
        read_call(&document, None, None),
    );

    let outcome = doc_gate_for(&call, &work_tree, &store, Some(&timed_out));
    assert!(outcome.answer.is_none(), "the read runs as it was written");
    assert!(
        outcome.journaled,
        "and the worker is spawned for what is owed"
    );
    let sha = rift_shadow::sha256_hex(&[std::fs::read(&document).unwrap().as_slice()]);
    let jobs = journaled(&work_tree);
    let owed: Vec<_> = jobs
        .iter()
        .filter_map(|job| match &job.kind {
            crate::agent_queue::JobKind::DocConvert {
                rel_path,
                content_sha256,
            } => Some((rel_path.clone(), content_sha256.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        owed,
        vec![("slow.docx".to_owned(), sha.clone())],
        "the conversion is journaled by the bytes it is about: {jobs:?}"
    );
    let booked: Vec<_> = jobs
        .iter()
        .filter_map(|job| match &job.kind {
            crate::agent_queue::JobKind::ServeAudit(audit) => Some(audit),
            _ => None,
        })
        .collect();
    assert_eq!(booked.len(), 1, "{jobs:?}");
    assert_eq!(
        booked[0].decision,
        crate::agent_queue::ServeAuditKind::DocDeferred
    );
    assert_eq!(booked[0].tag.as_deref(), Some("deadline"));
    assert!(
        booked[0].frame.is_empty(),
        "nothing was put in front of the model, so nothing cost"
    );
    assert_eq!(
        booked[0].basis.as_ref().map(|b| b.content_sha256.clone()),
        Some(sha),
        "the row names the document it is about"
    );
    // Nothing was decided against: the deadline says nothing about the
    // document, so the next pass converts it.
    assert!(!store.is_skipped(
        &crate::doc_store::sidecar_key(
            &booked[0].basis.as_ref().unwrap().content_sha256,
            &rift_recall_policy::DocPolicy::default()
        )
        .unwrap()
    ));
}

/// A window whose amendment for one document was dropped: the client applied
/// some sibling hook's amendment instead, so Rift's path never reached the
/// tool and the read's own result came back naming the document.
struct DroppedRewrite {
    tmp: tempfile::TempDir,
    store: crate::doc_store::DocStore,
    converter: StandInConverter,
    /// The next read of the same document by the same window.
    second: RawHook,
    /// The sidecar the first, amended call named.
    sidecar: String,
    /// What the gate made of the result that proved the drop.
    verified: DocOutcome,
}

impl DroppedRewrite {
    fn work_tree(&self) -> PathBuf {
        self.tmp.path().join("main")
    }

    /// The next document read of this window, through the same gate.
    fn read_again(&self) -> DocOutcome {
        doc_gate_for(
            &self.second,
            &self.work_tree(),
            &self.store,
            Some(&self.converter),
        )
    }

    /// The sentence that read is declined with. One per sidecar, so a test
    /// that wants it builds its own window.
    fn redirect_reason(&self) -> String {
        let redirected = self.read_again();
        let Some(DocAnswer::Redirect { reason }) = redirected.answer else {
            panic!("the window is answered by the decline now: {redirected:?}");
        };
        reason
    }
}

/// Amend one document read, then hand the gate a result naming the document
/// rather than the sidecar — the shape a dropped amendment arrives in.
fn dropped_rewrite() -> DroppedRewrite {
    let (tmp, document, store) = documents("plan.docx", b"PK\x03\x04 the plan");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Plan\n\nSteps.\n");
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-1",
        read_call(&document, None, None),
    );
    let Some(DocAnswer::Rewrite { sidecar, .. }) =
        doc_gate_for(&call, &work_tree, &store, Some(&converter)).answer
    else {
        panic!("the first read is amended");
    };
    // The result comes back naming the document, not the sidecar: some other
    // hook's amendment is the one the client kept.
    let dropped = hook(
        "claude-code",
        "PostToolUse",
        "s-1",
        read_call(&document, None, None),
    );
    let verified = doc_gate_for(&dropped, &work_tree, &store, Some(&converter));
    let second = hook(
        "claude-code",
        "PreToolUse",
        "s-1",
        read_call(&document, None, None),
    );
    DroppedRewrite {
        tmp,
        store,
        converter,
        second,
        sidecar,
        verified,
    }
}

/// ★ The client applies the last amendment it is handed for one event, so a
/// sibling hook amending the same call wins and Rift's path never reaches the
/// tool. The read's own result is where that is found out — it carries the
/// input the tool really ran with — and it is booked once, as a document this
/// window was not given.
#[test]
fn a_dropped_amendment_is_booked_once_as_a_deferred_document() {
    let fixture = dropped_rewrite();
    assert!(
        fixture.verified.answer.is_none(),
        "a result is not answered"
    );
    assert!(fixture.verified.journaled, "and the drop is recorded");
    let dropped_rows: Vec<_> = journaled(&fixture.work_tree())
        .into_iter()
        .filter_map(|job| match job.kind {
            crate::agent_queue::JobKind::ServeAudit(audit)
                if audit.tag.as_deref() == Some("rewrite_dropped") =>
            {
                Some(audit)
            }
            _ => None,
        })
        .collect();
    assert_eq!(dropped_rows.len(), 1, "recorded once");
    assert_eq!(
        dropped_rows[0].decision,
        crate::agent_queue::ServeAuditKind::DocDeferred
    );
}

/// ★ From there this window is answered by the decline, which nothing can
/// overwrite: the sentence names where to read instead, in full — a span
/// inside a pushed frame is bounded, and half a path is a wrong path — and it
/// carries none of the document's words.
#[test]
fn the_read_after_a_dropped_amendment_is_redirected_to_the_sidecar() {
    let fixture = dropped_rewrite();
    let here = sidecar_here(&fixture.work_tree(), &fixture.sidecar);
    let reason = fixture.redirect_reason();
    assert!(
        reason.contains(&here),
        "the frame names the sidecar: {reason}"
    );
    assert!(!reason.contains("Steps."), "and never the document's words");
}

/// ★ That redirect travels as the family's own decline, and a decline never
/// carries an amendment beside it.
#[test]
fn a_document_redirect_travels_as_a_decline_carrying_no_amendment() {
    let reason = dropped_rewrite().redirect_reason();
    let response = rift_agent::respond::gate(
        &rift_core::AgentId::CLAUDE_CODE,
        "PreToolUse",
        &rift_agent::respond::GateDecision::Deny { reason: &reason },
    )
    .expect("the declined call");
    assert_eq!(
        response.pointer("/hookSpecificOutput/permissionDecision"),
        Some(&serde_json::Value::from("deny"))
    );
    assert_eq!(
        response.pointer("/hookSpecificOutput/permissionDecisionReason"),
        Some(&serde_json::Value::from(reason.as_str()))
    );
    assert!(
        response
            .pointer("/hookSpecificOutput/updatedInput")
            .is_none(),
        "a decline never carries an amendment"
    );
}

/// ★ Once, and only once: the repeat is the escape hatch the frame itself
/// promises.
#[test]
fn the_repeat_after_a_document_redirect_runs() {
    let fixture = dropped_rewrite();
    let _redirected = fixture.redirect_reason();
    assert!(
        fixture.read_again().answer.is_none(),
        "one decline per sidecar, and the repeated call runs"
    );
}

/// ★ And nothing this window journaled asks for a declaration. The agent read
/// the container in full, so every row about it claims the zero it avoided —
/// the drop, the decline that followed it, and the repeat that ran after.
#[test]
fn a_document_the_window_really_read_is_claimed_by_no_row() {
    let fixture = dropped_rewrite();
    let _redirected = fixture.redirect_reason();
    let _repeated = fixture.read_again();
    let rows = audits(&fixture.work_tree());
    assert!(
        rows.iter().all(|row| row.declared_avoided == Some(0)),
        "a document the agent really read is claimed by nobody: {rows:?}"
    );
}

/// ★ A substitution is claimed where it is proved, and only there.
///
/// An amendment is a request to the client, not an outcome — the client
/// applies the last one it is handed for an event, and a sibling hook can
/// take the call — so the decision itself books nothing. The read's own
/// result carries the input the tool really ran with: when that input
/// names the sidecar, the read the agent wrote never happened and the one
/// figure this channel may claim is journaled, once, over the document it
/// stood in for. A second result for the same amendment, and a sidecar
/// this window never armed, claim nothing.
#[test]
fn a_substitution_is_booked_only_when_the_executed_call_proves_it_and_only_once() {
    let (tmp, document, store) = documents("spec.docx", b"PK\x03\x04 the spec");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Spec\n\nThe body.\n");
    let sha = rift_shadow::sha256_hex(&[&std::fs::read(&document).expect("the document")]);
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-1",
        read_call(&document, None, None),
    );

    let decided = doc_gate_for(&call, &work_tree, &store, Some(&converter));
    let Some(DocAnswer::Rewrite { sidecar, .. }) = decided.answer else {
        panic!("the read is amended");
    };
    assert!(
        !decided.journaled && audits(&work_tree).is_empty(),
        "a decision the client may still overrule claims nothing"
    );

    // The result names the sidecar: the amendment survived to the tool.
    let executed = hook(
        "claude-code",
        "PostToolUse",
        "s-1",
        read_call(Path::new(&sidecar), None, None),
    );
    let verified = doc_gate_for(&executed, &work_tree, &store, Some(&converter));
    assert!(verified.answer.is_none(), "a result is not answered");
    assert!(verified.journaled, "and the substitution is booked here");
    let booked = audits(&work_tree);
    assert_eq!(booked.len(), 1, "{booked:?}");
    assert_eq!(
        booked[0].decision,
        crate::agent_queue::ServeAuditKind::DocRewritten
    );
    assert_eq!(
        booked[0].tag.as_deref(),
        Some(rift_recall_serve::TAG_DECLARED_BASELINE)
    );
    assert_eq!(
        booked[0].declared_avoided, None,
        "the figure is the worker's to count: a hook counts no tokens"
    );
    assert_eq!(
        booked[0]
            .basis
            .as_ref()
            .map(|b| (b.rel_path.clone(), b.content_sha256.clone())),
        Some(("spec.docx".to_owned(), sha)),
        "the row is about the document, under the bytes the amendment was \
         made over — never about the sidecar"
    );
    assert!(
        booked[0].frame.is_empty(),
        "a substitution puts no sentence in front of a model"
    );

    // The same result again, and the same sidecar read by a window that
    // was never amended: neither is a second read of the document.
    doc_gate_for(&executed, &work_tree, &store, Some(&converter));
    let stranger = hook(
        "claude-code",
        "PostToolUse",
        "s-2",
        read_call(Path::new(&sidecar), None, None),
    );
    doc_gate_for(&stranger, &work_tree, &store, Some(&converter));
    assert_eq!(
        audits(&work_tree).len(),
        1,
        "one amendment, one claim: {:?}",
        audits(&work_tree)
    );
}

/// A repository that would rather be told than amended is told, without
/// anything having to be dropped first.
#[test]
fn a_repository_that_chose_the_redirect_is_never_amended() {
    let (tmp, document, store) = documents("notes.odt", b"PK\x03\x04 notes");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Notes\n");
    let mut policy = rift_recall_policy::ServePolicy::default();
    policy.doc.channel = rift_recall_policy::DocChannel::Redirect;
    let effective = rift_recall_policy::resolve(
        &policy,
        "claude-code",
        crate::recall::family_capability(&rift_core::AgentId::CLAUDE_CODE),
    );
    assert_eq!(effective.doc_rewrite, rift_recall_policy::Mode::Off);
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-1",
        read_call(&document, None, None),
    );
    let outcome = doc_gate_in(
        &crate::hooktime::HookTimer::new("test", false),
        &AdapterRegistry::with_builtin(),
        &call,
        &work_tree.join(".git"),
        &work_tree,
        &policy,
        &effective,
        &store,
        Some(&converter),
    );
    assert!(
        matches!(outcome.answer, Some(DocAnswer::Redirect { .. })),
        "{outcome:?}"
    );
}

/// The document levers as this repository ships them, for one family,
/// with that family's own pinned capability resolved into them.
fn document_levers_for(family: &str) -> rift_recall_policy::Effective {
    rift_recall_policy::resolve(
        &rift_recall_policy::ServePolicy::default(),
        family,
        crate::recall::family_capability(&rift_core::AgentId::new(family)),
    )
}

/// One document read through the gate under levers the test states.
fn doc_gate_under(
    raw: &RawHook,
    work_tree: &Path,
    store: &crate::doc_store::DocStore,
    converter: Option<&dyn rift_recall_doc::DocConverter>,
    effective: &rift_recall_policy::Effective,
) -> DocOutcome {
    doc_gate_in(
        &crate::hooktime::HookTimer::new("test", false),
        &AdapterRegistry::with_builtin(),
        raw,
        &work_tree.join(".git"),
        work_tree,
        &rift_recall_policy::ServePolicy::default(),
        effective,
        store,
        converter,
    )
}

/// An Antigravity `view_file` of a whole file, nested the way that family
/// nests arguments and carrying the two envelope keys its recorded runs
/// put on every call.
fn view_of_whole(path: &Path) -> serde_json::Value {
    serde_json::json!({"toolCall": {"name": "view_file", "args": {
        "AbsolutePath": path.display().to_string(),
        "toolAction": "Viewing the document",
        "toolSummary": "View the document",
    }}})
}

/// This member's own spelling of a sidecar, which is how a frame names it.
fn sidecar_here(work_tree: &Path, sidecar: &str) -> String {
    rift_recall_substrate::worktree::repo_relative_slash(work_tree, Path::new(sidecar))
        .expect("the view is inside this member")
}

/// ★ **A Codex `cat` of a document is declined once, with the sidecar
/// named.** This family's calls cannot be amended by anybody — its own
/// published surface says so — so the channel that answers it is the
/// decline its pinned row documents and a recorded run confirmed. The
/// sentence names where the Markdown is, the response is the object that
/// family is pinned to, and the repeat runs.
#[test]
fn a_codex_cat_of_a_docx_is_redirected_once_to_the_sidecar_when_confirmed() {
    let (tmp, document, store) = documents("spec.docx", b"PK\x03\x04 a document");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Spec\n\nThe body.\n");
    let effective = document_levers_for("codex");
    assert_eq!(
        effective.doc_rewrite,
        rift_recall_policy::Mode::Off,
        "nothing amends a call this family will not have amended"
    );
    assert_eq!(effective.doc_redirect, rift_recall_policy::Mode::Enforce);
    let call = hook(
        "codex",
        "PreToolUse",
        "s-1",
        shell_of(&format!("cat {}", shell_spelling(&document))),
    );

    let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
    let Some(DocAnswer::Redirect { reason }) = outcome.answer.clone() else {
        panic!("a document read this family makes is declined: {outcome:?}");
    };
    assert_eq!(converter.calls(), 1, "converted once, for the family");
    let sidecar = std::fs::read_dir(work_tree.join(".rift/state/doc"))
        .expect("this member's views")
        .flatten()
        .find(|entry| entry.path().extension().is_some_and(|x| x == "md"))
        .expect("one view");
    let here = sidecar_here(&work_tree, &sidecar.path().to_string_lossy());
    assert!(
        reason.contains(&here),
        "the sentence names where to read instead: {reason}"
    );
    assert!(
        !reason.contains("The body."),
        "and never the document's own words: {reason}"
    );

    // The response, composed where every response is composed: this
    // family's own declined envelope, and no amendment in it.
    let response = rift_agent::respond::gate(
        &rift_core::AgentId::CODEX,
        "PreToolUse",
        &rift_agent::respond::GateDecision::Deny { reason: &reason },
    )
    .expect("the run recorded in this family's fixture confirmed the channel");
    assert_eq!(
        response.pointer("/hookSpecificOutput/permissionDecision"),
        Some(&serde_json::Value::from("deny"))
    );
    assert_eq!(
        response.pointer("/hookSpecificOutput/permissionDecisionReason"),
        Some(&serde_json::Value::from(reason.as_str()))
    );
    assert!(
        response
            .pointer("/hookSpecificOutput/updatedInput")
            .is_none(),
        "a decline never carries an amendment: {response}"
    );

    // One decline per sidecar: the repeat is the escape hatch the sentence
    // itself promises, and it costs no second conversion.
    let again = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
    assert!(again.answer.is_none(), "the repeated call runs: {again:?}");
    assert_eq!(converter.calls(), 1);
}

/// ★ **An Antigravity `view_file` of a document is declined once, with the
/// sidecar named.** The same channel and the same once-per-sidecar rule as
/// the family beside it, over a call whose arguments are nested and carry
/// the client's own envelope keys — which are read past, because they say
/// nothing about which part of the file was asked for.
#[test]
fn a_view_file_of_a_docx_is_redirected_once_to_the_sidecar_when_confirmed() {
    let (tmp, document, store) = documents("brief.docx", b"PK\x03\x04 a brief");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Brief\n\nThe brief.\n");
    let effective = document_levers_for("antigravity");
    assert_eq!(effective.doc_rewrite, rift_recall_policy::Mode::Off);
    assert_eq!(effective.doc_redirect, rift_recall_policy::Mode::Enforce);
    let call = hook("antigravity", "PreToolUse", "s-1", view_of_whole(&document));

    let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
    let Some(DocAnswer::Redirect { reason }) = outcome.answer.clone() else {
        panic!("a document read this family makes is declined: {outcome:?}");
    };
    let sidecar = std::fs::read_dir(work_tree.join(".rift/state/doc"))
        .expect("this member's views")
        .flatten()
        .find(|entry| entry.path().extension().is_some_and(|x| x == "md"))
        .expect("one view");
    assert!(
        reason.contains(&sidecar_here(&work_tree, &sidecar.path().to_string_lossy())),
        "the sentence names where to read instead: {reason}"
    );
    let response = rift_agent::respond::gate(
        &rift_core::AgentId::ANTIGRAVITY,
        "PreToolUse",
        &rift_agent::respond::GateDecision::Deny { reason: &reason },
    )
    .expect("the runs recorded in this family's fixture confirmed the channel");
    assert_eq!(
        response.get("decision"),
        Some(&serde_json::Value::from("deny"))
    );
    assert_eq!(
        response.get("reason"),
        Some(&serde_json::Value::from(reason.as_str()))
    );
    assert!(
        response.get("updatedInput").is_none(),
        "a decline never carries an amendment: {response}"
    );

    // A view that addresses lines is not a whole-document read, so the
    // sidecar cannot answer it and the call is left alone.
    let mut ranged = view_of_whole(&document);
    ranged["toolCall"]["args"]["StartLine"] = serde_json::json!(1);
    let ranged = hook("antigravity", "PreToolUse", "s-2", ranged);
    let outcome = doc_gate_under(&ranged, &work_tree, &store, Some(&converter), &effective);
    assert!(
        outcome.answer.is_none(),
        "a call that addresses lines is not a call the sidecar can answer: {outcome:?}"
    );
}

/// **A family whose reason channel no recorded run has confirmed is never
/// declined on — and still gets its sidecars.** The two halves are one
/// point: conversion is what makes the answer exist for every reader of
/// the family, and speaking to a model is a separate permission that this
/// family has not earned. A decline it could not tell from a tool failure
/// is one it would be right to retry.
#[test]
fn an_unconfirmed_family_is_never_denied_and_still_has_sidecars() {
    let (tmp, document, store) = documents("draft.docx", b"PK\x03\x04 a draft");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Draft\n\nThe draft.\n");
    // The capability an unconfirmed row resolves to, held against the
    // pinned one so this states the difference rather than a whole table.
    let unconfirmed = rift_recall_policy::FamilyCapability {
        confirmed_reason_channel: false,
        ..crate::recall::family_capability(&rift_core::AgentId::CODEX)
    };
    let effective = rift_recall_policy::resolve(
        &rift_recall_policy::ServePolicy::default(),
        "codex",
        unconfirmed,
    );
    assert_eq!(
        effective.doc_redirect,
        rift_recall_policy::Mode::Observe,
        "the lever is capped by the channel, not by the repository"
    );
    assert_eq!(
        effective.doc_redirect_cap,
        Some(rift_recall_policy::CapReason::UnconfirmedReasonChannel)
    );
    let call = hook(
        "codex",
        "PreToolUse",
        "s-1",
        shell_of(&format!("cat {}", shell_spelling(&document))),
    );

    let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
    assert!(
        outcome.answer.is_none(),
        "nothing is declined on a channel nobody has seen a model read: {outcome:?}"
    );
    // And the answer exists anyway: the family store holds it and this
    // member has its view, so the pull verb and every other member are
    // served by this read having happened.
    assert_eq!(converter.calls(), 1);
    let views: Vec<_> = std::fs::read_dir(work_tree.join(".rift/state/doc"))
        .expect("this member's views")
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|x| x == "md"))
        .collect();
    assert_eq!(views.len(), 1, "the sidecar is here");
    assert!(
        std::fs::read_to_string(views[0].path())
            .expect("the view")
            .contains("The draft."),
        "and it is the document as Markdown"
    );
}

/// **A lever in `observe` decides and records; it never declines.** The
/// row it books is the size of the pool the lever would work on — one per
/// document, not one per read of it — and it carries no frame, because
/// nothing was put in front of a model.
#[test]
fn an_observed_document_channel_records_the_decline_it_did_not_make() {
    let (tmp, document, store) = documents("plan.odt", b"PK\x03\x04 a plan");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Plan\n\nSteps.\n");
    let mut policy = rift_recall_policy::ServePolicy::default();
    policy.doc.mode = rift_recall_policy::Mode::Observe;
    let effective = rift_recall_policy::resolve(
        &policy,
        "codex",
        crate::recall::family_capability(&rift_core::AgentId::CODEX),
    );
    assert_eq!(effective.doc_redirect, rift_recall_policy::Mode::Observe);
    let call = hook(
        "codex",
        "PreToolUse",
        "s-1",
        shell_of(&format!("cat {}", shell_spelling(&document))),
    );

    let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
    assert!(outcome.answer.is_none(), "observe declines nothing");
    assert!(outcome.journaled, "and records what it decided");
    let observed: Vec<_> = journaled(&work_tree)
        .into_iter()
        .filter_map(|job| match job.kind {
            crate::agent_queue::JobKind::ServeAudit(audit)
                if audit.tag.as_deref() == Some("redirect_observed") =>
            {
                Some(audit)
            }
            _ => None,
        })
        .collect();
    assert_eq!(observed.len(), 1);
    assert!(
        observed[0].frame.is_empty(),
        "a sentence nobody read costs nothing"
    );
    assert_eq!(observed[0].declared_avoided, Some(0));

    // The pool is documents, not reads: the same document read again in
    // the same window is the same one decision.
    let again = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
    assert!(again.answer.is_none());
    assert!(!again.journaled, "one row per sidecar, as in enforce");
}

/// **A command line the strict grammar does not spell is not a document
/// read.** A pipeline is not a single command; `sed` and `head` deliver
/// part of a file, which is exactly what a whole-document answer cannot
/// stand in for; and a command that reads names has read no file at all.
/// Each of them leaves the call as the agent wrote it.
#[test]
fn a_command_line_outside_the_grammar_is_never_a_document_read() {
    let (tmp, document, store) = documents("sheet.csv", b"a,b\n1,2\n");
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Sheet\n");
    let effective = document_levers_for("codex");
    let named = document.display().to_string();
    for command in [
        format!("cat {named} | head -n 5"),
        format!("cat {named} > copy.csv"),
        format!("head -n 5 {named}"),
        format!("sed -n '1,5p' {named}"),
        format!("tail {named}"),
        format!("nl {named}"),
        format!("cat {named} {named}"),
        format!("ls {}", work_tree.display()),
    ] {
        let call = hook("codex", "PreToolUse", "s-1", shell_of(&command));
        let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
        assert!(
            outcome.answer.is_none(),
            "`{command}` is not a whole-document read: {outcome:?}"
        );
    }
    assert_eq!(
        converter.calls(),
        0,
        "and none of them spent the conversion budget"
    );
}

/// A document too large to hash while an agent waits is not keyed on this
/// path at all: the read runs as it was written and the bounded pass is
/// asked for, so the answer exists for the next reader without this hook
/// having spent the budget to produce it.
///
/// The ceiling is the read gate's own — what a hook may read and hash
/// inline — and not the converter's, which is what will be parsed at all.
#[test]
fn a_document_too_large_to_hash_on_the_hook_path_is_left_to_the_worker() {
    let (policy, effective) = document_levers();
    let over = usize::try_from(effective.max_inline_hash_bytes).unwrap() + 1;
    assert!(
        u64::try_from(over).unwrap() <= effective.max_input_bytes,
        "the fixture is inside what the converter would parse, so the only \
         thing refusing it is the hook's own hashing ceiling"
    );
    let (tmp, document, store) = documents("huge.docx", &vec![b'z'; over]);
    let work_tree = tmp.path().join("main");
    let converter = StandInConverter::markdown("# Huge\n");
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-1",
        read_call(&document, None, None),
    );

    let outcome = doc_gate_in(
        &crate::hooktime::HookTimer::new("test", false),
        &AdapterRegistry::with_builtin(),
        &call,
        &work_tree.join(".git"),
        &work_tree,
        &policy,
        &effective,
        &store,
        Some(&converter),
    );
    assert!(outcome.answer.is_none(), "the read runs as it was written");
    assert!(outcome.journaled, "and the pass is asked for");
    assert_eq!(converter.calls(), 0, "nothing was parsed on this path");
    let asked: Vec<_> = journaled(&work_tree)
        .into_iter()
        .filter(|job| matches!(job.kind, crate::agent_queue::JobKind::DocSweep))
        .collect();
    assert_eq!(asked.len(), 1, "one pass, asked for once");
}

// -------------------------------------------- the basis snapshot ---
