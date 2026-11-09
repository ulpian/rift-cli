//! The checkpoint detail: one assembler and the segments it composes from
//! the catalogue entry, the family's two stores, the shadow refs and the
//! redaction report.

use std::collections::BTreeMap;
use std::path::Path;

use rift_canopy::{
    ViewerDetail, ViewerLine, ViewerLineKind, ViewerTimelineItem, ViewerTimelineSegment,
};
use rift_config::{EffectiveConfig, PolicySource};
use rift_core::{Result, RiftError};
use rift_recall_policy::{load_serve_policy, ServePolicySource};
use rift_recall_substrate::parse;
use rift_recall_substrate::{
    build_checkpoint_catalogue, CheckpointCatalogueEntry, CheckpointOrigin, Dialect, GixSubstrate,
    SubstrateReader, TimestampProvenance,
};

use super::catalogue::{
    branch_containment, family_stores, map_timestamp_provenance, projection_facts,
    storage_kind_label,
};
use super::diff::diff_items;
use super::lifecycle::lifecycle_segment;
use super::seal::turn_seal_segment;
use super::text::{compact_utc, fact_line, list_or, short, warning_item};
use super::transcript::transcript_segment;
use super::{open_existing_read_only, query_count};

/// The most per-rule rows the assembly card lists before folding the rest
/// into one "… and N more" line — a reader wants the headline detectors,
/// not a 30-row table.
const MAX_PER_RULE_ROWS: usize = 8;

pub(super) fn build_detail(
    root: &Path,
    project: &Path,
    entry: &CheckpointCatalogueEntry,
) -> Result<ViewerDetail> {
    let substrate = GixSubstrate::open(project)?;
    let (projection_path, recall_path) = family_stores(root, project)?;
    let projection = projection_facts(&projection_path);
    let session_time = projection.session_times.get(&entry.checkpoint_id).copied();
    let containing = branch_containment(&substrate, std::slice::from_ref(entry))?
        .remove(&entry.checkpoint_id)
        .unwrap_or_default();

    let mut segments = vec![checkpoint_summary_segment(entry, session_time, &containing)];
    segments.push(recall_summary_segment(
        project,
        &projection_path,
        &recall_path,
        entry,
    ));
    segments.push(shadow_context_segment(
        &substrate,
        entry.anchor_commit.as_deref(),
    ));
    segments.extend(member_sessions(project).and_then(|dir| turn_seal_segment(&dir, entry)));
    segments.push(transcript_segment(&substrate, entry));
    segments.push(assembly_segment(
        project,
        &substrate,
        &projection_path,
        entry,
    ));
    segments.push(lifecycle_segment(&projection_path, entry));

    Ok(ViewerDetail {
        checkpoint_id: entry.checkpoint_id.clone(),
        segments,
        notice: None,
    })
}

/// The sessions directory of the member `project` checks out; `None`
/// outside a repository, where no turn seal can have been recorded.
fn member_sessions(project: &Path) -> Option<std::path::PathBuf> {
    let paths = rift_git::discover(project).ok()?;
    Some(crate::session::member_sessions(
        &paths.git_dir,
        &paths.work_tree,
    ))
}

fn checkpoint_summary_segment(
    entry: &CheckpointCatalogueEntry,
    session_time: Option<i64>,
    containing: &[String],
) -> ViewerTimelineSegment {
    let (timestamp, provenance) = entry.effective_timestamp(session_time);
    let mut lines = checkpoint_fact_lines(entry, timestamp, provenance, containing);
    lines.extend(entry.origins.iter().map(origin_line));
    lines.extend(entry.issues.iter().map(|issue| {
        ViewerLine::plain(
            ViewerLineKind::Warning,
            format!("{}: {}", issue.component, issue.detail),
        )
    }));
    ViewerTimelineSegment {
        title: "Checkpoint summary".to_owned(),
        boundary: Some("facts are grouped by field; this is not an event chronology".to_owned()),
        ordered_by_time: false,
        items: vec![ViewerTimelineItem {
            id: "checkpoint-summary".to_owned(),
            title: format!("checkpoint {}", short(&entry.checkpoint_id)),
            timestamp: timestamp.and_then(compact_utc),
            summary: lines,
            body: Vec::new(),
            expanded_body: None,
            collapsed: false,
            full_session_only: false,
            tone: rift_canopy::ViewerItemTone::Neutral,
        }],
    }
}

/// The one spelling of a missing anchor, shared word for word with the
/// catalogue row (`rift-canopy`): a checkpoint minted on an unborn HEAD names
/// no commit, and the detail says so rather than "unavailable", which reads
/// as a read failure.
pub(crate) const NO_ANCHOR_LABEL: &str = "no commit anchor";

fn checkpoint_fact_lines(
    entry: &CheckpointCatalogueEntry,
    timestamp: Option<i64>,
    provenance: TimestampProvenance,
    containing: &[String],
) -> Vec<ViewerLine> {
    let chain = entry.chain_position.map_or_else(
        || "unchained".to_owned(),
        |position| format!("canonical position {position}"),
    );
    let timestamp = timestamp.map_or_else(
        || "unknown".to_owned(),
        |ms| {
            format!(
                "{} ({})",
                compact_utc(ms).unwrap_or_else(|| "unknown".to_owned()),
                map_timestamp_provenance(provenance).label()
            )
        },
    );
    vec![
        fact_line("checkpoint", &entry.checkpoint_id),
        fact_line(
            "anchor commit",
            entry.anchor_commit.as_deref().unwrap_or(NO_ANCHOR_LABEL),
        ),
        fact_line("chain", &chain),
        fact_line("timestamp", &timestamp),
        fact_line(
            "strategy",
            entry.metadata.strategy.as_deref().unwrap_or("unavailable"),
        ),
        fact_line(
            "sessions",
            &list_or(&entry.metadata.sessions, "none recorded"),
        ),
        fact_line("local branches", &list_or(containing, "none proven")),
    ]
}

fn origin_line(origin: &CheckpointOrigin) -> ViewerLine {
    let kind = storage_kind_label(origin.kind);
    fact_line(
        "origin",
        &format!(
            "{} · {} · {kind} · tip {}",
            origin.storage_ref,
            origin.dialect.label(),
            origin
                .tip_oid
                .as_deref()
                .map_or_else(|| "unknown".to_owned(), short,)
        ),
    )
}

fn recall_summary_segment(
    project: &Path,
    projection_path: &Path,
    recall_path: &Path,
    entry: &CheckpointCatalogueEntry,
) -> ViewerTimelineSegment {
    let mut lines = policy_lines(project);
    lines.extend(store_presence_lines(projection_path, recall_path));
    lines.extend(ingest_coverage_lines(project, recall_path));
    if !entry.metadata_present {
        lines.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            "checkpoint metadata is unavailable; independent facts remain shown",
        ));
    }
    ViewerTimelineSegment {
        title: "Recall summary".to_owned(),
        boundary: Some(
            "repository-wide durable facts are shown separately from checkpoint facts".to_owned(),
        ),
        ordered_by_time: false,
        items: vec![ViewerTimelineItem {
            id: "recall-summary".to_owned(),
            title: "durable Recall state".to_owned(),
            timestamp: None,
            summary: lines,
            body: Vec::new(),
            expanded_body: None,
            collapsed: false,
            full_session_only: false,
            tone: rift_canopy::ViewerItemTone::Neutral,
        }],
    }
}

/// The repository's declared policy state: whether Recall is on, where the
/// config policy came from, and the serve policy's provenance.
fn policy_lines(project: &Path) -> Vec<ViewerLine> {
    let config = EffectiveConfig::load(&project.join(".rift"));
    let serve = load_serve_policy(&project.join(".rift"));
    let mut lines = Vec::new();
    match &config {
        Ok(config) => {
            let source = match config.policy_source() {
                PolicySource::Declared => "declared",
                PolicySource::Default => "secure default",
                PolicySource::Refused(_) => "refused; secure floor active",
            };
            lines.push(fact_line(
                "Total Recall feature",
                if config.recall_enabled() {
                    "enabled"
                } else {
                    "disabled"
                },
            ));
            lines.push(fact_line("config policy", source));
        }
        Err(_) => {
            lines.push(ViewerLine::plain(
                ViewerLineKind::Warning,
                "config policy invalid; code views fail closed",
            ));
        }
    }
    let serve_state = match serve.source {
        ServePolicySource::Declared => "declared",
        ServePolicySource::Default => "default",
        ServePolicySource::Invalid => "invalid; fail closed",
        ServePolicySource::Refused => "refused; fail closed",
    };
    lines.push(fact_line("Recall serve policy", serve_state));
    lines
}

fn store_presence_lines(projection_path: &Path, recall_path: &Path) -> Vec<ViewerLine> {
    vec![
        fact_line(
            "capture projection",
            if projection_path.is_file() {
                "present (opened read-only)"
            } else {
                "absent"
            },
        ),
        fact_line(
            "Recall store",
            if recall_path.is_file() {
                "present (opened read-only); no in-memory-live claim"
            } else {
                "absent; no in-memory-live claim"
            },
        ),
    ]
}

/// Durable ingest against what the substrate discovers, and the
/// repository-wide serve totals — which are never attributed to one
/// checkpoint, and say so.
fn ingest_coverage_lines(project: &Path, recall_path: &Path) -> Vec<ViewerLine> {
    let (discovered_ids, discovered_origins) =
        discovered_checkpoint_counts(project).unwrap_or((0, 0));
    let Some(store) = open_existing_read_only(recall_path) else {
        return vec![fact_line(
            "durable ingest coverage",
            &format!(
                "0/{discovered_ids} unioned checkpoint IDs; {discovered_origins} physical storage origin(s); Recall store unavailable"
            ),
        )];
    };
    let ingested = query_count(&store, "SELECT COUNT(*) FROM n_checkpoint", &[]).unwrap_or(0);
    let serve_calls =
        query_count(&store, "SELECT COUNT(*) FROM memd_audit_event", &[]).unwrap_or(0);
    let attributed_calls =
        query_count(&store, "SELECT COUNT(*) FROM serve_call_event", &[]).unwrap_or(0);
    let latest = query_count(&store, "SELECT MAX(recorded_at) FROM memd_audit_event", &[]);
    vec![
        fact_line(
            "durable ingest coverage",
            &format!(
                "{ingested}/{discovered_ids} unioned checkpoint IDs; {discovered_origins} physical storage origin(s) discovered"
            ),
        ),
        fact_line(
            "repository-wide Recall serves/calls",
            &format!(
                "{serve_calls} serve decisions · {attributed_calls} attributed calls · latest {}",
                latest
                    .and_then(compact_utc)
                    .unwrap_or_else(|| "unknown".to_owned())
            ),
        ),
        ViewerLine::plain(
            ViewerLineKind::Dim,
            "serve/call totals are repository-wide and are not attributed to this checkpoint",
        ),
    ]
}

fn discovered_checkpoint_counts(project: &Path) -> Result<(i64, i64)> {
    let substrate = GixSubstrate::open(project)?;
    let catalogue = build_checkpoint_catalogue(&substrate)?;
    let ids = i64::try_from(catalogue.len())
        .map_err(|_| RiftError::storage("checkpoint count exceeds i64"))?;
    let origins = catalogue.iter().try_fold(0i64, |total, entry| {
        let count = i64::try_from(entry.origins.len().max(1))
            .map_err(|_| RiftError::storage("checkpoint origin count exceeds i64"))?;
        total
            .checked_add(count)
            .ok_or_else(|| RiftError::storage("checkpoint origin count exceeds i64"))
    })?;
    Ok((ids, origins))
}

fn shadow_context_segment(substrate: &GixSubstrate, anchor: Option<&str>) -> ViewerTimelineSegment {
    let anchor_prefix = anchor.map(|value| value.chars().take(7).collect::<String>());
    let mut refs = [Dialect::Rift, Dialect::Legacy]
        .into_iter()
        .flat_map(|dialect| substrate.walker().shadow_refs(dialect).unwrap_or_default())
        .collect::<Vec<_>>();
    refs.sort_by(|a, b| {
        let a_match = anchor_prefix.as_deref() == Some(a.commit7.as_str());
        let b_match = anchor_prefix.as_deref() == Some(b.commit7.as_str());
        b_match
            .cmp(&a_match)
            .then_with(|| a.full_name.cmp(&b.full_name))
    });
    let mut items = Vec::new();
    for shadow in refs {
        let tip_time = substrate
            .commit_meta(&shadow.tip)
            .ok()
            .flatten()
            .and_then(|meta| compact_utc(meta.committed_ms));
        items.push(ViewerTimelineItem {
            id: format!("shadow:{}", shadow.full_name),
            title: format!("{} · {}", shadow.dialect.label(), shadow.full_name),
            timestamp: tip_time.clone(),
            summary: vec![fact_line(
                "metadata only",
                "raw shadow payload was not read",
            )],
            body: vec![
                fact_line("ref", &shadow.full_name),
                fact_line("base commit discriminator", &shadow.commit7),
                fact_line("worktree discriminator", &shadow.wt6),
                fact_line("tip", &shadow.tip),
                fact_line("tip time", tip_time.as_deref().unwrap_or("unknown")),
            ],
            expanded_body: None,
            collapsed: true,
            full_session_only: false,
            tone: rift_canopy::ViewerItemTone::Neutral,
        });
    }
    if items.is_empty() {
        items.push(ViewerTimelineItem {
            id: "shadow:none".to_owned(),
            title: "no live local shadow refs".to_owned(),
            timestamp: None,
            summary: vec![fact_line(
                "scope",
                "local refs only; remote-tracking refs and raw payloads excluded",
            )],
            body: Vec::new(),
            expanded_body: None,
            collapsed: false,
            full_session_only: false,
            tone: rift_canopy::ViewerItemTone::Neutral,
        });
    }
    ViewerTimelineSegment {
        title: "Shadow context".to_owned(),
        boundary: Some(
            "metadata-only list of live local refs; no raw shadow content is read".to_owned(),
        ),
        ordered_by_time: false,
        items,
    }
}

fn assembly_segment(
    project: &Path,
    substrate: &impl SubstrateReader,
    projection_path: &Path,
    entry: &CheckpointCatalogueEntry,
) -> ViewerTimelineSegment {
    let mut items = Vec::new();
    let mut facts = vec![
        fact_line(
            "Merkle root",
            entry
                .metadata
                .merkle_root
                .as_deref()
                .unwrap_or("unavailable"),
        ),
        fact_line(
            "previous checkpoint hash",
            entry
                .metadata
                .prev_hash
                .as_deref()
                .unwrap_or("genesis or unavailable"),
        ),
        fact_line(
            "canonicalization version",
            &entry.metadata.canon_version.map_or_else(
                || "legacy/unspecified".to_owned(),
                |value| value.to_string(),
            ),
        ),
    ];
    facts.extend(redaction_report_lines(substrate, entry));
    items.push(ViewerTimelineItem {
        id: "assembly-facts".to_owned(),
        title: "digests and capture facts".to_owned(),
        timestamp: None,
        summary: facts,
        body: Vec::new(),
        expanded_body: None,
        collapsed: false,
        full_session_only: false,
        tone: rift_canopy::ViewerItemTone::Neutral,
    });

    match diff_items(project, projection_path, entry) {
        Ok(mut diffs) if !diffs.is_empty() => items.append(&mut diffs),
        Ok(_) => items.push(warning_item(
            "assembly-no-hunks",
            "changed hunks unavailable",
            "no recorded checkpoint/commit hunk facts were found; ranges were not recomputed",
        )),
        Err(error) => items.push(warning_item(
            "assembly-hunk-error",
            "changed hunks unavailable",
            &error.to_string(),
        )),
    }
    ViewerTimelineSegment {
        title: "Checkpoint assembly".to_owned(),
        boundary: Some(
            "ordering unavailable: digest, capture, and hunk facts have no shared chronology"
                .to_owned(),
        ),
        ordered_by_time: false,
        items,
    }
}

/// The redaction-report facts of the assembly card: validity, counts, packs,
/// input digest, the per-rule rows, and the report's
/// parse issues as warnings; one warning when the artifact is absent.
fn redaction_report_lines(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
) -> Vec<ViewerLine> {
    let Ok(Some(bytes)) = substrate.read_artifact(&entry.checkpoint_id, "redaction_report.json")
    else {
        return vec![ViewerLine::plain(
            ViewerLineKind::Warning,
            "redaction report unavailable",
        )];
    };
    let report = parse::parse_redaction_report(&bytes, &entry.checkpoint_id);
    let mut facts = vec![
        fact_line(
            "redaction structurally valid",
            &report.value.is_valid().to_string(),
        ),
        fact_line("redacted spans", &report.value.total_spans.to_string()),
        fact_line("masked bytes", &report.value.bytes_masked.to_string()),
        fact_line(
            "redaction rule packs",
            &list_or(&report.value.rule_pack_versions, "none recorded"),
        ),
        fact_line(
            "redaction input digest",
            report
                .value
                .input_sha256
                .as_deref()
                .unwrap_or("unavailable"),
        ),
    ];
    facts.extend(per_rule_lines(
        "redacted by",
        "detectors",
        &report.value.by_detector,
    ));
    facts.extend(per_rule_lines(
        "redacted class",
        "classes",
        &report.value.by_class,
    ));
    for issue in report.issues {
        facts.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            format!("redaction report: {}", issue.detail),
        ));
    }
    facts
}

/// One `<label> <id>: <count>` line per entry of a per-rule map (which
/// detector or class masked how many spans), sorted by the map's
/// own order, capped at [`MAX_PER_RULE_ROWS`] with the remainder counted.
/// An absent or empty map yields nothing — a report written before the
/// counts were archived is not a report with zero detectors.
fn per_rule_lines(label: &str, noun: &str, counts: &BTreeMap<String, u32>) -> Vec<ViewerLine> {
    let mut lines: Vec<ViewerLine> = counts
        .iter()
        .take(MAX_PER_RULE_ROWS)
        .map(|(id, n)| fact_line(&format!("{label} {id}"), &n.to_string()))
        .collect();
    let more = counts.len().saturating_sub(MAX_PER_RULE_ROWS);
    if more > 0 {
        lines.push(ViewerLine::plain(
            ViewerLineKind::Text,
            format!("… and {more} more {noun}"),
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use rift_recall_substrate::MemorySubstrate;

    use super::super::fixtures::{entry, item_text, rich_entry};
    use super::*;

    /// An anchorless checkpoint's fact line names the absence with the one
    /// label the catalogue row uses, never "unavailable".
    #[test]
    fn an_anchorless_checkpoint_names_no_commit_anchor() {
        let mut anchorless = entry(&"e".repeat(64), &"d".repeat(40));
        anchorless.anchor_commit = None;
        let lines = checkpoint_fact_lines(&anchorless, None, TimestampProvenance::Recorded, &[]);
        let anchor = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .find(|text| text.contains("anchor commit"))
            .expect("the anchor fact line");
        assert!(anchor.contains("no commit anchor"), "{anchor}");
        assert!(!anchor.contains("unavailable"), "{anchor}");
    }

    #[test]
    fn summary_facts_name_the_chain_position_and_absent_sessions() {
        let anchor = "a".repeat(40);
        let rich = rich_entry(&anchor);
        let summary = checkpoint_summary_segment(&rich, Some(2_000), &["main".to_owned()]);
        assert!(item_text(&summary.items[0]).contains("canonical position 7"));
        let sparse = checkpoint_summary_segment(&entry("sparse", &anchor), None, &[]);
        assert!(item_text(&sparse.items[0]).contains("none recorded"));
    }

    #[test]
    fn an_absent_recall_store_is_reported_rather_than_counted_as_empty() {
        let rich = rich_entry(&"a".repeat(40));
        let project = tempfile::tempdir().unwrap();
        let projection = project.path().join("missing-projection.db");
        let recall = project.path().join("missing-recall.db");
        let recall_summary = recall_summary_segment(project.path(), &projection, &recall, &rich);
        assert!(item_text(&recall_summary.items[0]).contains("Recall store unavailable"));
    }

    #[test]
    fn present_stores_read_as_present_and_recorded_metadata_is_not_doubted() {
        let rich = entry(&"e".repeat(64), &"d".repeat(40));
        let repo = tempfile::tempdir().unwrap();
        let projection = repo.path().join("projection.db");
        let recall = repo.path().join("recall.db");
        std::fs::write(&projection, []).unwrap();
        std::fs::write(&recall, []).unwrap();
        let summary = recall_summary_segment(repo.path(), &projection, &recall, &rich);
        let summary_text = item_text(&summary.items[0]);
        assert!(summary_text.contains("present (opened read-only)"));
        assert!(!summary_text.contains("metadata is unavailable"));
    }

    #[test]
    fn the_assembly_card_keeps_its_facts_when_the_projection_is_absent() {
        let rich = rich_entry(&"a".repeat(40));
        let project = tempfile::tempdir().unwrap();
        let projection = project.path().join("missing-projection.db");
        let empty_substrate = MemorySubstrate::local();
        let assembly = assembly_segment(project.path(), &empty_substrate, &projection, &rich);
        assert!(assembly
            .items
            .iter()
            .any(|item| item.id == "assembly-facts"));
        assert!(assembly
            .items
            .iter()
            .any(|item| item.id == "diff-projection-unavailable"));
    }

    #[test]
    fn the_assembly_card_names_the_redaction_rule_packs() {
        let rich = entry(&"e".repeat(64), &"d".repeat(40));
        let repo = tempfile::tempdir().unwrap();
        let projection = repo.path().join("projection.db");
        std::fs::write(&projection, []).unwrap();
        let artifacts = MemorySubstrate::local().with_artifact(
            &rich.checkpoint_id,
            "redaction_report.json",
            format!(
                r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}","total_spans":2,"bytes_masked":8}}"#,
                "a".repeat(64)
            )
            .as_bytes(),
        );
        let assembly = assembly_segment(repo.path(), &artifacts, &projection, &rich);
        assert!(item_text(&assembly.items[0]).contains("builtin@1"));
    }

    /// The per-rule report on the surface: a report carrying per-rule counts
    /// lists one row per detector and per class, capped with a remainder
    /// line; a report without the maps lists no such row at all.
    #[test]
    fn the_assembly_card_lists_per_rule_redaction_counts_when_the_report_carries_them() {
        let anchor = "d".repeat(40);
        let mut rich = entry(&"e".repeat(64), &anchor);
        rich.metadata_present = true;
        let repo = tempfile::tempdir().unwrap();
        let projection = repo.path().join("projection.db");
        std::fs::write(&projection, []).unwrap();

        let detectors: Vec<String> = (0..MAX_PER_RULE_ROWS + 2)
            .map(|i| format!("\"pack.rule_{i:02}\":{}", i + 1))
            .collect();
        let populated = MemorySubstrate::local().with_artifact(
            &rich.checkpoint_id,
            "redaction_report.json",
            format!(
                r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}","total_spans":3,"bytes_masked":8,"by_detector":{{{}}},"by_class":{{"Pii":2,"VendorSecret":1}}}}"#,
                "a".repeat(64),
                detectors.join(",")
            )
            .as_bytes(),
        );
        let text =
            item_text(&assembly_segment(repo.path(), &populated, &projection, &rich).items[0]);
        assert!(text.contains("redacted by pack.rule_00: 1"), "{text}");
        assert!(
            text.contains(&format!(
                "redacted by pack.rule_{:02}: {}",
                MAX_PER_RULE_ROWS - 1,
                MAX_PER_RULE_ROWS
            )),
            "{text}"
        );
        assert!(
            !text.contains(&format!("pack.rule_{MAX_PER_RULE_ROWS:02}")),
            "capped: {text}"
        );
        assert!(text.contains("… and 2 more detectors"), "{text}");
        assert!(
            text.contains("redacted class Pii: 2")
                && text.contains("redacted class VendorSecret: 1"),
            "{text}"
        );
        assert!(!text.contains("more classes"), "{text}");

        let absent = MemorySubstrate::local().with_artifact(
            &rich.checkpoint_id,
            "redaction_report.json",
            format!(
                r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}","total_spans":2,"bytes_masked":8}}"#,
                "a".repeat(64)
            )
            .as_bytes(),
        );
        let text = item_text(&assembly_segment(repo.path(), &absent, &projection, &rich).items[0]);
        assert!(
            !text.contains("redacted by") && !text.contains("redacted class"),
            "{text}"
        );
        assert!(text.contains("redacted spans: 2"), "{text}");
    }
}
