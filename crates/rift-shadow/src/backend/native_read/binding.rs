//! One owned native header, preserving raw presence and its parsed view.

use super::manifests::{original_claims, spill_claims, OriginalClaim, SpillClaim};
use rift_core::Result;
use rift_recall_substrate::SubstrateReader;

/// The header both era-dependent passes read, parsed from `metadata.json` once.
pub struct ShardHeader {
    /// The canonicalization epoch the shard declares, or **0** when the field
    /// (or the file) is absent.
    ///
    /// The era a shard is verified under is read off the shard itself and
    /// nowhere else. Never a date, never the branch, never "shards after commit
    /// X": a repository can be cloned, rebased, re-imported or restored from a
    /// bundle, and none of those change what a shard says about itself. What
    /// the era can no longer buy is the pre-Rift *exemption* — that is the
    /// DIALECT's (the ref the shard was read from), because the one thing
    /// editing a shard cannot rewrite is where the shard is.
    pub canon_version: u32,
    /// The native session ids the checkpoint covers.
    pub sessions: Vec<String>,
    /// The `tasks/<tool-use-id>/` subtrees the shard claims to carry —
    /// the reader's enumeration; empty for a taskless
    /// shard and for every pre-task shard.
    pub tasks: Vec<String>,
    /// The recorded `task_tokens` rollup `(input, output)`, when present —
    /// re-summed against the task subtrees' own metadata below.
    pub task_tokens: Option<(u64, u64)>,
    /// The sealed spills the session transcript's `spills` manifest claims —
    /// the reader's enumeration of `tool-results/`; empty on every shard
    /// whose transcript named none.
    pub spills: Vec<SpillClaim>,
    /// The session transcript's `originals` manifest, every entry.
    pub originals: Vec<OriginalClaim>,
    /// Raw metadata presence, retained separately from parse success.
    pub metadata_bytes: Option<Vec<u8>>,
    /// Parsed metadata when present and syntactically valid JSON.
    pub doc: Option<serde_json::Value>,
}

/// Read a shard's header. One `metadata.json` read for both fields — the
/// era pass and the id pass used to be two reads of one file.
///
/// # Errors
/// Propagates the metadata artifact read error.
pub fn shard_header(reader: &impl SubstrateReader, id: &str) -> Result<ShardHeader> {
    use rift_recall_substrate::parse;

    let Some(bytes) = reader.read_artifact(id, "metadata.json")? else {
        return Ok(ShardHeader {
            canon_version: 0,
            sessions: Vec::new(),
            tasks: Vec::new(),
            task_tokens: None,
            spills: Vec::new(),
            originals: Vec::new(),
            metadata_bytes: None,
            doc: None,
        });
    };
    let meta = parse::parse_metadata(&bytes, id).value;
    // The task list rides the same document; parsed leniently and locally
    // (the shared lenient metadata type is another crate's surface, and a
    // malformed list simply enumerates nothing — whose missing leaves the
    // root/id passes then refuse).
    let doc = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    let tasks = doc
        .as_ref()
        .and_then(|v| v.get("tasks").cloned())
        .and_then(|t| serde_json::from_value::<Vec<String>>(t).ok())
        .unwrap_or_default();
    let task_tokens = doc
        .as_ref()
        .and_then(|v| v.get("task_tokens"))
        .and_then(|t| {
            Some((
                t.get("input_tokens").and_then(serde_json::Value::as_u64)?,
                t.get("output_tokens").and_then(serde_json::Value::as_u64)?,
            ))
        });
    Ok(ShardHeader {
        canon_version: meta.canon_version.unwrap_or(0),
        sessions: meta.sessions,
        tasks,
        task_tokens,
        spills: spill_claims(doc.as_ref()),
        originals: original_claims(doc.as_ref()),
        metadata_bytes: Some(bytes),
        doc,
    })
}

/// Check supplied retained bytes against their complete root and path claims.
/// This proves content binding, not native origin or executable queue authority.
/// Absence preserves ordinary historical admission and supplies no gap facts.
///
/// # Errors
/// Refuses unsupported eras, malformed binding or carrier data, invalid reports,
/// and any mismatch between the complete artifact root and checkpoint path.
pub fn check_boundary_gap_binding(
    header: &ShardHeader,
    native: &super::NativeArtifacts,
    report: Option<&[u8]>,
    id: &str,
    dialect: rift_recall_substrate::Dialect,
) -> Result<Option<Vec<rift_core::boundary_gap::GapEnvelope>>> {
    use crate::backend::boundary_gaps::{invalid, strict_session_agents, validate_carriers};
    use rift_core::boundary_gap::MAX_ARTIFACT_BYTES;
    let Some(gaps) = native.boundary_gaps.as_deref() else {
        return Ok(None);
    };
    if dialect != rift_recall_substrate::Dialect::Rift {
        return Err(invalid());
    }
    let raw = header.metadata_bytes.as_deref().ok_or_else(invalid)?;
    if raw.len() > MAX_ARTIFACT_BYTES {
        return Err(invalid());
    }
    // Required fields reject duplicates; unrelated metadata retains compatibility.
    let binding = rift_recall_substrate::parse::boundary_gaps::parse_gap_binding_metadata(raw)?;
    if binding.canon_version != 3
        || binding.sessions != header.sessions
        || binding.merkle_root.len() != 64
        || !binding
            .merkle_root
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid());
    }
    let envelopes = rift_recall_substrate::parse::boundary_gaps::parse_boundary_gaps(gaps)?;
    let agents = strict_session_agents(native.sessions.as_deref().ok_or_else(invalid)?)?;
    validate_carriers(&binding.sessions, &agents, &envelopes)?;
    require_claimed_presence(native)?;
    let report = report.ok_or_else(invalid)?;
    if !rift_recall_substrate::parse::parse_redaction_report(report, id)
        .value
        .is_valid()
    {
        return Err(invalid());
    }
    let content = native.content.as_deref().ok_or_else(invalid)?;
    let tasks = native.task_leaves();
    let sessions = native.session_meta_leaves();
    let spills = super::manifests::spill_leaves(&native.spills);
    let originals = super::manifests::original_leaves(&native.originals);
    let leaves = native.leaves(content, report, &tasks, &sessions, &spills, &originals);
    if crate::backend::shard_merkle_root(&leaves).to_hex() != binding.merkle_root
        || !crate::condense::checkpoint_ids_for_shard(&binding.sessions, 3, &leaves)
            .iter()
            .any(|candidate| candidate == id)
    {
        return Err(invalid());
    }
    Ok(Some(envelopes))
}

fn require_claimed_presence(native: &super::NativeArtifacts) -> Result<()> {
    use crate::backend::boundary_gaps::invalid;
    if native.spills.iter().any(|s| s.body.is_none())
        || native
            .originals
            .iter()
            .any(|o| o.claim.claims_body() && o.body.is_none())
        || native.tasks.iter().any(|t| !t.metadata_present)
        || native
            .session_metas
            .iter()
            .any(|(name, _)| !native.session_meta_presence.contains(name))
    {
        return Err(invalid());
    }
    Ok(())
}
