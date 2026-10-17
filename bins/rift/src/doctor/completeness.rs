//! The client-side completeness of the sealed record: what `rift verify`'s
//! completeness audit finds, as a doctor leg.
//!
//! The same audit, through the same function — never a second reading of the
//! shards or the client — rendered as counts per word and one line per
//! source that is not `complete`, naming its checkpoint, session and task.
//! Report-only: it writes nothing, repairs nothing and consumes nothing; a
//! finding here is what the developer looks at, not what doctor fixes.

use std::path::Path;

use rift_recall_substrate::{GixSubstrate, SubstrateReader};

use crate::verify::completeness::{audit, ClientSources, CompletenessSummary, Word};

/// The completeness leg. Silent when no source was audited.
pub(super) fn doctor_capture_completeness(repo: &Path) -> (String, serde_json::Value) {
    let unreadable = || (String::new(), serde_json::json!({ "readable": false }));
    let Ok(reader) = GixSubstrate::open(repo) else {
        return unreadable();
    };
    let Ok(sources) = ClientSources::resolve(repo) else {
        return unreadable();
    };
    completeness_leg(&reader, &sources)
}

/// The leg over any reader and any client, so a test can hand it a shard
/// and a temporary directory without a repository.
pub(super) fn completeness_leg(
    reader: &impl SubstrateReader,
    sources: &ClientSources,
) -> (String, serde_json::Value) {
    match audit(reader, sources) {
        Ok(summary) => render(&summary),
        Err(_) => (
            "\n  completeness: the sealed record could not be read".to_owned(),
            serde_json::json!({ "readable": false }),
        ),
    }
}

/// The counts line, then every source that is not `complete`.
fn render(summary: &CompletenessSummary) -> (String, serde_json::Value) {
    let mut human = String::new();
    if summary.sources_audited > 0 {
        human.push_str(&format!(
            "\n  completeness: {} ({} source(s) audited, {} older shard(s) skipped)",
            summary.human(),
            summary.sources_audited,
            summary.shards_skipped_older
        ));
    }
    for f in summary.findings.iter().filter(|f| f.word != Word::Complete) {
        let task = f
            .task
            .as_deref()
            .map_or(String::new(), |t| format!(" task {t}"));
        let reason = f
            .reason
            .map_or(String::new(), |r| format!(" ({})", r.as_str()));
        human.push_str(&format!(
            "\n  completeness: checkpoint {} session {}{task} {} {}{reason}",
            f.checkpoint,
            f.session.as_deref().unwrap_or("-"),
            f.artifact,
            f.word.as_str()
        ));
    }
    (human, summary.to_json())
}

#[cfg(test)]
mod tests {
    use rift_recall_substrate::MemorySubstrate;

    use super::*;
    use crate::verify::completeness::tests::{sealed, shard, Client, SESSION, SPAWN};
    use crate::verify::completeness::Budget;

    /// The leg prints the counts and names every source that is not
    /// complete — the checkpoint, the session, the task — and no byte of any
    /// file; an unaudited repository prints nothing.
    #[test]
    fn the_leg_names_each_incomplete_source_by_its_identifiers() {
        let tmp = tempfile::tempdir().unwrap();
        let client = Client::new(tmp.path());
        let empty = MemorySubstrate::local();
        assert_eq!(
            completeness_leg(&empty, &client.sources(Budget::default())).0,
            ""
        );

        // No session cache: both sources are unverifiable, no address.
        let main =
            b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"secret-ish\"}}\n";
        let sub = shard(empty, "cp-doc", 1, &sealed(main), Some(&sealed(main)), None);
        let (human, json) = completeness_leg(&sub, &client.sources(Budget::default()));
        assert!(
            human.contains("completeness: 0 complete, 0 appended after seal, 0 client file gone, 0 mismatch, 2 unverifiable (no_address 2"),
            "{human}"
        );
        assert!(
            human.contains(&format!(
                "checkpoint cp-doc session {SESSION} task {SPAWN} tasks/{SPAWN}/transcript.jsonl unverifiable (no_address)"
            )),
            "{human}"
        );
        assert!(
            human.contains(&format!(
                "checkpoint cp-doc session {SESSION} transcript.jsonl unverifiable (no_address)"
            )),
            "{human}"
        );
        assert!(!human.contains("secret-ish"), "{human}");
        assert_eq!(json["unverifiable"], 2);
        assert_eq!(json["findings"].as_array().unwrap().len(), 2);
    }
}
