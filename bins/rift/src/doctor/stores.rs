//! The caches beside the projection: the family's document sidecars, its
//! read-time basis snapshots, and this checkout's window journals. Counts
//! and no verdicts — each is collectable without costing an answer.

#[cfg(feature = "doc")]
use crate::doc_helper;
use crate::{family, session_verbs, window_journal};

/// The family's document sidecars, this member's views of them, and what
/// converts a document here.
///
/// Four numbers and one line. The counts say what the family has already paid
/// for and what this checkout can hand an agent without paying again; the
/// orphan count says how many views point at an answer that is gone (`rift
/// clean` removes those, and until it does they are inert — a lookup asks the
/// family, not the view). The line names the converter: this binary itself,
/// re-spawned on the hidden `hooks doc` route — or, in a build without the
/// document seam, the honest statement that nothing converts. Read-only,
/// fixes nothing, opens no store.
pub(super) fn doctor_document_store(repo: &std::path::Path) -> (String, serde_json::Value) {
    let Some(store) = rift_git::discover(repo)
        .ok()
        .as_ref()
        .and_then(session_verbs::document_store)
    else {
        return (String::new(), serde_json::json!({ "surveyed": false }));
    };
    let survey = store.survey();
    let converter = document_converter_line();
    let mut human = format!(
        "\n  documents:      {} sidecar(s), {} byte(s) shared by the family; {} linked here",
        survey.sidecars, survey.bytes, survey.member_links,
    );
    if survey.orphan_links > 0 {
        human.push_str(&format!(
            "\n    {} view(s) point at an answer the family no longer holds \
             (inert; `rift clean` removes them)",
            survey.orphan_links,
        ));
    }
    match &converter {
        Some(line) => human.push_str(&format!("\n    converter:    {line}")),
        None => human.push_str(
            "\n    converter:    not compiled into this build — documents are read as \
             they are, and nothing is converted",
        ),
    }
    (
        human,
        serde_json::json!({
            "surveyed": true,
            "sidecars": survey.sidecars,
            "bytes": survey.bytes,
            "member_links": survey.member_links,
            "orphan_links": survey.orphan_links,
            "converter": converter,
        }),
    )
}

/// The family's read-time basis snapshots: how many revisions are still
/// reachable as a delta basis, and what they cost on disk.
///
/// Two numbers and no verdict. A store that is empty is not a fault — a
/// repository that has captured no reads yet has nothing to snapshot, and
/// every snapshot is collectable at any time without costing an answer, since
/// the blob route and the file in the checkout are both untouched. What the
/// numbers are for is the one question a person asks about a cache they did
/// not ask for: how big is it. Read-only, fixes nothing, opens no store.
pub(super) fn doctor_basis_store(repo: &std::path::Path) -> (String, serde_json::Value) {
    let Some(family) = rift_git::discover(repo)
        .ok()
        .and_then(|r| family::try_at(&r.work_tree))
    else {
        return (String::new(), serde_json::json!({ "surveyed": false }));
    };
    let survey =
        rift_recall_serve::basis_store::BasisStore::for_family(&family.common_dir).survey();
    (
        format!(
            "\n  basis snapshots: {} revision(s), {} byte(s) shared by the family \
             (collected by `rift clean`)",
            survey.snapshots, survey.bytes,
        ),
        serde_json::json!({
            "surveyed": true,
            "snapshots": survey.snapshots,
            "bytes": survey.bytes,
        }),
    )
}

/// The converter this binary reaches conversion through, named the way the
/// seam itself names it — this executable, re-spawned on the hidden route —
/// rather than by a second lookup that could disagree with the first.
#[cfg(feature = "doc")]
fn document_converter_line() -> Option<String> {
    Some(
        doc_helper::self_exec_converter(rift_recall_doc::DEFAULT_BUDGET).map_or_else(
            || "this executable cannot name its own path — nothing is converted".to_owned(),
            |converter| {
                format!(
                    "{} (re-spawned as `{}`; engine anydoc {})",
                    converter.program().display(),
                    doc_helper::ROUTE.join(" "),
                    rift_recall_doc::ENGINE_VERSION
                )
            },
        ),
    )
}

/// A build without the document seam converts nothing, and says so.
#[cfg(not(feature = "doc"))]
fn document_converter_line() -> Option<String> {
    None
}

/// The window journals this checkout holds, one per context window, read-only.
///
/// Two numbers and one warning. The counts say how much local run state the
/// gate has accumulated here; the rows that did not parse say whether anything
/// is writing into the directory that this tool did not write — a row that
/// fails its shape is dropped rather than interpreted, so the cost of one is a
/// saving not taken, but a directory full of them is worth knowing about
/// before anyone asks why nothing is ever declined. Fixes nothing: `rift clean`
/// is what removes them, by age.
pub(super) fn doctor_window_journals(repo: &std::path::Path) -> (String, serde_json::Value) {
    let survey = window_journal::survey(repo);
    if survey.windows == 0 {
        return (
            "\n  window journals: none".to_owned(),
            serde_json::json!({ "windows": 0 }),
        );
    }
    let mut human = format!(
        "\n  window journals: {} context window(s), {} snapshot(s), {} line(s)",
        survey.windows, survey.snapshots, survey.lines,
    );
    if survey.dropped > 0 {
        human.push_str(&format!(
            "\n    {} row(s) did not parse and were dropped (a forged, torn, \
             or newer row costs a saving, never a wrong answer)",
            survey.dropped,
        ));
    }
    if survey.ineffective > 0 {
        human.push_str(&format!(
            "\n    {} read(s) ran in spite of a decision not to (this client \
             does not act on the channel the decision was sent on)",
            survey.ineffective,
        ));
    }
    (
        human,
        serde_json::json!({
            "windows": survey.windows,
            "snapshots": survey.snapshots,
            "lines": survey.lines,
            "dropped": survey.dropped,
            "ineffective": survey.ineffective,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The doctor names the window journals it found, and says out loud when a
    /// row in one of them could not be read, or when a client ran a read in
    /// spite of a decision not to — the two states that are invisible
    /// otherwise, because an unreadable row is dropped rather than reported
    /// and a decision nobody acted on leaves the read looking ordinary.
    #[test]
    fn the_doctor_names_the_window_journals_and_the_rows_it_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let (human, json) = doctor_window_journals(tmp.path());
        assert!(human.contains("window journals: none"), "{human}");
        assert_eq!(json["windows"], 0);

        let reads = window_journal::dir(tmp.path());
        std::fs::create_dir_all(&reads).unwrap();
        std::fs::write(
            reads.join(format!("{}.jsonl", window_journal::key("s", None))),
            b"{\"v\":1,\"k\":\"evict\",\"t\":1,\"reason\":\"clear\",\"at\":1}\nnot a row\n",
        )
        .unwrap();

        let (human, json) = doctor_window_journals(tmp.path());
        assert!(
            human.contains("1 context window(s), 0 snapshot(s), 1 line(s)"),
            "{human}"
        );
        assert!(human.contains("1 row(s) did not parse"), "{human}");
        assert_eq!(json["dropped"], 1);
        assert_eq!(json["ineffective"], 0);

        // A decline, and then a read of exactly those lines: the client ran
        // the call regardless, and the pair is the only trace of it.
        let hash = "a".repeat(64);
        let pair = format!(
            "{{\"v\":1,\"k\":\"deny\",\"t\":1,\"p\":\"a.rs\",\"h\":\"{hash}\",\"lo\":1,\"hi\":9,\"at\":1}}\n\
             {{\"v\":1,\"k\":\"read\",\"t\":1,\"p\":\"a.rs\",\"h\":\"{hash}\",\"lo\":1,\"hi\":9,\"n\":9,\"at\":2}}\n"
        );
        std::fs::write(
            reads.join(format!("{}.jsonl", window_journal::key("s2", None))),
            pair.as_bytes(),
        )
        .unwrap();
        let (human, json) = doctor_window_journals(tmp.path());
        assert!(
            human.contains("1 read(s) ran in spite of a decision not to"),
            "{human}"
        );
        assert_eq!(json["ineffective"], 1);
    }
}
