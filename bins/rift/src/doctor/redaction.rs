//! The checkpoints whose transcripts the redaction gate sealed by its
//! per-line fallback.
//!
//! A line the gate could not settle is sealed flat or replaced by a
//! placeholder record, and the metadata beside its transcript counts it. The
//! record is whole either way; what the developer is owed is the name of every
//! checkpoint where it is whole by a different route, because a line there was
//! over-redacted and a rule pack is probably the cause. Finding only: this leg
//! reads the counts and changes nothing.

use std::path::Path;

use rift_recall_substrate::{GixSubstrate, SubstrateReader};

use crate::verify::fallback::on_checkpoint;

/// The fallback leg. Silent when no checkpoint carries a count.
pub(super) fn doctor_redaction_fallback(repo: &Path) -> (String, serde_json::Value) {
    let Ok(reader) = GixSubstrate::open(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    fallback_leg(&reader)
}

/// The leg over any reader, so a test can hand it a shard without a
/// repository.
fn fallback_leg(reader: &impl SubstrateReader) -> (String, serde_json::Value) {
    let Ok(shards) = reader.checkpoints() else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let mut named = Vec::new();
    let mut inconsistent = Vec::new();
    for shard in &shards {
        match on_checkpoint(reader, &shard.checkpoint_id) {
            Ok(recorded) if recorded.any() => named.push((shard.checkpoint_id.clone(), recorded)),
            Ok(_) => {}
            Err(_) => inconsistent.push(shard.checkpoint_id.clone()),
        }
    }
    let mut human = String::new();
    for (id, r) in &named {
        human.push_str(&format!(
            "\n  redaction fallback: checkpoint {id} sealed {} transcript line(s) by the \
             per-line fallback, {} of them as placeholder records (a rule pack that does not \
             settle is the usual cause)",
            r.lines, r.placeholders
        ));
    }
    for id in &inconsistent {
        human.push_str(&format!(
            "\n  redaction fallback: checkpoint {id} has unreadable or inconsistent count facts"
        ));
    }
    let mut json = serde_json::json!({
        "checkpoints": named
            .iter()
            .map(|(id, r)| serde_json::json!({
                "checkpoint": id,
                "redaction_flat_fallback": r.lines,
                "redaction_fallback_placeholders": r.placeholders,
            }))
            .collect::<Vec<_>>(),
    });
    if let Some(fields) = json.as_object_mut().filter(|_| !inconsistent.is_empty()) {
        fields.insert(
            "inconsistent_checkpoints".into(),
            serde_json::json!(inconsistent),
        );
    }
    (human, json)
}

#[cfg(test)]
mod tests {
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};

    use super::*;

    fn shard(id: &str, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.to_owned(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal,
        }
    }

    /// The checkpoint whose transcript carries a fallback count is named
    /// with its counts; the one without is not, and an all-clear is silent.
    #[test]
    fn a_checkpoint_carrying_a_fallback_count_is_named() {
        let clean = MemorySubstrate::local()
            .with_shard(shard("cp-clean", 0))
            .with_artifact("cp-clean", "metadata.json", br#"{"sessions":["s"]}"#)
            .with_artifact("cp-clean", "redaction_report.json", b"{}");
        assert_eq!(fallback_leg(&clean).0, "");
        let reader = clean.with_shard(shard("cp-fell-back", 1)).with_artifact(
            "cp-fell-back",
            "metadata.json",
            br#"{"redaction_fallback_placeholders":1,"redaction_flat_fallback":1,"sessions":["s"]}"#,
        );
        let reader = reader.with_artifact(
            "cp-fell-back",
            "redaction_report.json",
            br#"{"redaction_fallback_placeholders":1,"redaction_flat_fallback":1}"#,
        );
        let (human, json) = fallback_leg(&reader);
        assert!(
            human.contains("checkpoint cp-fell-back sealed 1"),
            "{human}"
        );
        assert!(!human.contains("cp-clean"), "{human}");
        assert_eq!(json["checkpoints"][0]["checkpoint"], "cp-fell-back");
        assert_eq!(json["checkpoints"][0]["redaction_fallback_placeholders"], 1);
    }
    #[test]
    fn missing_and_malformed_facts_cannot_hide_in_an_all_clear() {
        let reader = MemorySubstrate::local().with_shard(shard("cp-missing", 0));
        let (human, json) = fallback_leg(&reader);
        assert!(human.contains("cp-missing"));
        assert_eq!(json["inconsistent_checkpoints"][0], "cp-missing");
        let reader = reader
            .with_artifact("cp-missing", "metadata.json", b"{}")
            .with_artifact(
                "cp-missing",
                "redaction_report.json",
                br#"{"redaction_flat_fallback":"2"}"#,
            );
        assert!(fallback_leg(&reader).0.contains("inconsistent"));
    }
}
