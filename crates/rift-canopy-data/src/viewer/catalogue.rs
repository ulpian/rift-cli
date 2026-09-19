//! The checkpoint catalogue: every durable checkpoint the substrate holds,
//! with the projection's time facts, branch containment and the selection a
//! target resolves to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_canopy::{ViewerCheckpointRow, ViewerOrigin, ViewerTimestampProvenance};
use rift_core::{Result, RiftError};
use rift_recall_substrate::catalogue::{CatalogueIssue, ResolutionKind, TargetCandidate};
use rift_recall_substrate::{
    build_checkpoint_catalogue, resolve_explicit_target, CheckpointCatalogueEntry,
    CheckpointStorageKind, GixSubstrate, SubstrateReader, TimestampProvenance,
    MAX_REACHABILITY_WALK,
};
use rift_store::window::CHECKPOINT_TIME_CTE;
use rift_store::{
    projection_db_for_family_under, recall_db_for_family_under, CanonicalStore, SqliteStore, Value,
    CREATED_AT_UNKNOWN,
};

use super::text::{bounded, compact_utc, short};
use super::CachedCatalogue;

use crate::store::runtime;

#[derive(Default)]
pub(super) struct ProjectionFacts {
    pub(super) ids: BTreeSet<String>,
    pub(super) session_times: BTreeMap<String, i64>,
    /// `checkpoint_id` → the worktree the shard was archived in, recorded by
    /// the first archiver. A checkpoint absent from this map is **unknown**: either its
    /// shard carries no `worktree` member (every shard written before
    /// provenance labelling) or this
    /// projection predates v14. Both render `unknown`, never `main`.
    pub(super) worktrees: BTreeMap<String, String>,
}

/// The **family's** two databases for a project path (family identity): the
/// capture projection and `recall.db`.
///
/// Both are the repository family's, so a viewer opened on a linked worktree
/// reads the checkpoints, projection facts and serve audit the family holds
/// rather than the empty pair the member's own path used to key — the same
/// correction `feeds.rs` made for the panels.
///
/// **The two ids are different id spaces, and this is where that was lost.**
/// The projection is keyed by the capture id and `recall.db` by the recall id;
/// `build_detail` derived the capture id once and passed it to *both*, so the
/// viewer opened a `recall.db` no writer ever writes and reported "Recall store
/// absent" and `0/N` durable ingest on a repository with a full one. It is the
/// same two-id-space defect `feeds.rs` records and pins in
/// `recall_reads_the_writers_recall_repo_id_not_the_capture_id`.
/// `recall_db_for_family_under` makes it unspellable: it takes a `FamilyIds`,
/// which carries both ids and which a bare `RepoId` does not implement.
///
/// # Errors
/// [`RiftError::NotFound`] when the path has no recorded family identity — not
/// a repository, no provable main work tree, or a manifest this reader will not
/// guess at. The viewer's callers already render a read error for the same
/// class of failure `GixSubstrate::open` returns a line above.
pub(super) fn family_stores(root: &Path, project: &Path) -> Result<(PathBuf, PathBuf)> {
    let family = crate::feeds::project_family(project).ok_or_else(|| {
        RiftError::NotFound(format!(
            "no recorded Rift identity for {} — run `rift init` from this \
             repository's main work tree",
            project.display()
        ))
    })?;
    Ok((
        projection_db_for_family_under(root, &family),
        recall_db_for_family_under(root, &family),
    ))
}

pub(super) fn load_catalogue(root: &Path, project: &Path) -> Result<CachedCatalogue> {
    let substrate = GixSubstrate::open(project)?;
    let entries = build_checkpoint_catalogue(&substrate)?;
    let (projection_path, _) = family_stores(root, project)?;
    let projection = projection_facts(&projection_path);
    let containment = branch_containment(&substrate, &entries)?;
    let mut rows = entries
        .iter()
        .map(|entry| {
            let derived = projection.session_times.get(&entry.checkpoint_id).copied();
            let (effective_at_ms, provenance) = entry.effective_timestamp(derived);
            let local_branches = containment
                .get(&entry.checkpoint_id)
                .cloned()
                .unwrap_or_default();
            let projection_state = if projection.ids.contains(&entry.checkpoint_id) {
                if entry.issues.is_empty() {
                    "projected"
                } else {
                    "partial"
                }
            } else {
                "unprojected"
            };
            checkpoint_row(
                entry,
                effective_at_ms,
                provenance,
                local_branches,
                projection_state,
                projection.worktrees.get(&entry.checkpoint_id).cloned(),
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by(compare_rows);
    Ok(CachedCatalogue { entries, rows })
}

pub(super) fn projection_facts(path: &Path) -> ProjectionFacts {
    if !path.is_file() {
        return ProjectionFacts::default();
    }
    let Ok(store) = SqliteStore::open_read_only(path) else {
        return ProjectionFacts::default();
    };
    let Ok(rt) = runtime() else {
        return ProjectionFacts::default();
    };
    let sql =
        format!("{CHECKPOINT_TIME_CTE}SELECT checkpoint_id, created_at, effective_at FROM ct");
    let Ok(rows) = rt.block_on(store.query(&sql, &[Value::Int(CREATED_AT_UNKNOWN)])) else {
        return ProjectionFacts::default();
    };
    let mut facts = ProjectionFacts::default();
    for row in rows {
        let (Ok(id), Ok(created), Ok(effective)) =
            (row.get_text(0), row.get_i64(1), row.get_opt_i64(2))
        else {
            continue;
        };
        facts.ids.insert(id.clone());
        if created == CREATED_AT_UNKNOWN {
            if let Some(ms) = effective.filter(|ms| *ms >= 0) {
                facts.session_times.insert(id, ms);
            }
        }
    }
    // The catalogue's worktree badge (worktree provenance) — this reader's first
    // use of `checkpoint_worktree`. Its own query, because a projection still
    // on a pre-v14 schema has no such relation and the catalogue must not go
    // dark over a badge: an unreadable table leaves the map empty and every
    // row reads `unknown`, which is the same answer a v14 projection gives
    // for a shard that carries no member.
    if let Ok(rows) = rt.block_on(store.query(
        "SELECT checkpoint_id, worktree_name FROM checkpoint_worktree",
        &[],
    )) {
        for row in rows {
            if let (Ok(id), Ok(name)) = (row.get_text(0), row.get_text(1)) {
                facts.worktrees.insert(id, bounded(&name));
            }
        }
    }
    facts
}

pub(super) fn branch_containment(
    substrate: &impl SubstrateReader,
    entries: &[CheckpointCatalogueEntry],
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut anchors: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in entries {
        if let Some(oid) = &entry.anchor_commit {
            anchors
                .entry(oid.clone())
                .or_default()
                .push(entry.checkpoint_id.clone());
        }
    }
    let mut result: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (branch, tip) in substrate.branch_tips()? {
        let mut seen = BTreeSet::new();
        let mut frontier = vec![tip];
        let mut walked = 0usize;
        while let Some(oid) = frontier.pop() {
            if walked >= MAX_REACHABILITY_WALK || !seen.insert(oid.clone()) {
                continue;
            }
            walked += 1;
            if let Some(checkpoint_ids) = anchors.get(&oid) {
                for checkpoint_id in checkpoint_ids {
                    result
                        .entry(checkpoint_id.clone())
                        .or_default()
                        .push(branch.clone());
                }
            }
            if let Some(meta) = substrate.commit_meta(&oid)? {
                frontier.extend(meta.parents);
            }
        }
    }
    for branches in result.values_mut() {
        branches.sort();
        branches.dedup();
    }
    Ok(result)
}

pub(super) fn checkpoint_row(
    entry: &CheckpointCatalogueEntry,
    effective_at_ms: Option<i64>,
    provenance: TimestampProvenance,
    local_branches: Vec<String>,
    projection: &str,
    worktree: Option<String>,
) -> ViewerCheckpointRow {
    let origins = entry
        .origins
        .iter()
        .map(|origin| ViewerOrigin {
            storage_ref: bounded(&origin.storage_ref),
            dialect: origin.dialect.label().to_owned(),
            kind: storage_kind_label(origin.kind).to_owned(),
            tip_oid: origin.tip_oid.clone(),
        })
        .collect::<Vec<_>>();
    let issues = entry.issues.iter().map(issue_label).collect::<Vec<_>>();
    let mut metadata_search = vec![
        entry.checkpoint_id.clone(),
        entry.anchor_commit.clone().unwrap_or_default(),
        entry.metadata.strategy.clone().unwrap_or_default(),
        projection.to_owned(),
    ];
    metadata_search.extend(local_branches.iter().cloned());
    metadata_search.extend(origins.iter().flat_map(|origin| {
        [
            origin.storage_ref.clone(),
            origin.dialect.clone(),
            origin.kind.clone(),
        ]
    }));
    metadata_search.extend(issues.iter().cloned());
    ViewerCheckpointRow {
        checkpoint_id: entry.checkpoint_id.clone(),
        effective_at_ms,
        timestamp_provenance: map_timestamp_provenance(provenance),
        compact_utc: effective_at_ms.and_then(compact_utc),
        excerpt: None,
        excerpt_loading: true,
        anchor_commit: entry.anchor_commit.clone(),
        origins,
        chain_position: entry.chain_position,
        local_branches,
        strategy: entry.metadata.strategy.as_deref().map(bounded),
        projection: projection.to_owned(),
        worktree,
        issues,
        metadata_search: metadata_search.join(" ").to_lowercase(),
    }
}

/// The one spelling of a storage kind on every surface, so the catalogue
/// badge and the detail's origin line cannot drift apart.
pub(super) fn storage_kind_label(kind: CheckpointStorageKind) -> &'static str {
    match kind {
        CheckpointStorageKind::Branch => "branch",
        CheckpointStorageKind::PerCheckpointRef => "per-checkpoint-ref",
    }
}

pub(super) fn map_timestamp_provenance(value: TimestampProvenance) -> ViewerTimestampProvenance {
    match value {
        TimestampProvenance::Recorded => ViewerTimestampProvenance::Recorded,
        TimestampProvenance::SessionDerived => ViewerTimestampProvenance::SessionDerived,
        TimestampProvenance::AnchorCommit => ViewerTimestampProvenance::AnchorCommit,
        TimestampProvenance::Unknown => ViewerTimestampProvenance::Unknown,
    }
}

pub(super) fn resolve_selection(
    substrate: &impl SubstrateReader,
    cached: &CachedCatalogue,
    target: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Some(target) = target else {
        return (
            cached.rows.first().map(|row| row.checkpoint_id.clone()),
            None,
        );
    };
    let Ok(resolved) = resolve_explicit_target(substrate, &cached.entries, target) else {
        return (
            cached.rows.first().map(|row| row.checkpoint_id.clone()),
            Some("target resolution failed; showing the newest checkpoint".to_owned()),
        );
    };
    let newest = cached.rows.first().map(|row| row.checkpoint_id.clone());
    match resolved.kind {
        ResolutionKind::ExactCheckpoint | ResolutionKind::CheckpointPrefix => (
            resolved
                .candidates
                .first()
                .map(|candidate| candidate.checkpoint_id.clone())
                .or(newest),
            None,
        ),
        ResolutionKind::AmbiguousCheckpointPrefix => (
            newest,
            Some(format!(
                "checkpoint prefix `{}` is ambiguous ({} matches: {}); showing latest",
                bounded(target),
                resolved.candidates.len(),
                candidate_notice(&resolved.candidates, None),
            )),
        ),
        ResolutionKind::GitCommit if resolved.candidates.is_empty() => (
            newest,
            Some(format!(
                "Git target `{}` resolved but names no durable checkpoint; showing latest",
                bounded(target)
            )),
        ),
        ResolutionKind::GitCommit => {
            let candidate_ids = resolved
                .candidates
                .iter()
                .map(|candidate| candidate.checkpoint_id.as_str())
                .collect::<BTreeSet<_>>();
            let selected = cached
                .rows
                .iter()
                .find(|row| candidate_ids.contains(row.checkpoint_id.as_str()))
                .map(|row| row.checkpoint_id.clone())
                .or(newest);
            let notice = (resolved.candidates.len() > 1).then(|| {
                let others = resolved.candidates.len().saturating_sub(1);
                format!(
                    "Git target matched {} checkpoints; selected newest; {others} other candidate(s): {}",
                    resolved.candidates.len(),
                    candidate_notice(&resolved.candidates, selected.as_deref())
                )
            });
            (selected, notice)
        }
        ResolutionKind::Missing => (
            newest,
            Some(format!(
                "target `{}` was not found; showing the newest checkpoint",
                bounded(target)
            )),
        ),
    }
}

fn issue_label(issue: &CatalogueIssue) -> String {
    bounded(&format!("{}: {}", issue.component, issue.detail))
}

fn compare_rows(a: &ViewerCheckpointRow, b: &ViewerCheckpointRow) -> std::cmp::Ordering {
    b.effective_at_ms
        .cmp(&a.effective_at_ms)
        .then_with(|| b.chain_position.cmp(&a.chain_position))
        .then_with(|| b.checkpoint_id.cmp(&a.checkpoint_id))
}

fn candidate_notice(candidates: &[TargetCandidate], selected: Option<&str>) -> String {
    const SHOWN: usize = 5;
    let ids = candidates
        .iter()
        .filter(|candidate| selected != Some(candidate.checkpoint_id.as_str()))
        .collect::<Vec<_>>();
    let mut text = ids
        .iter()
        .take(SHOWN)
        .map(|candidate| short(&candidate.checkpoint_id))
        .collect::<Vec<_>>()
        .join(", ");
    let hidden = ids.len().saturating_sub(SHOWN);
    if hidden > 0 {
        text.push_str(&format!(" +{hidden}"));
    }
    if text.is_empty() {
        "none".to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use rift_recall_substrate::{CommitMeta, MemorySubstrate};

    use super::super::fixtures::entry;
    use super::*;

    /// **The viewer opens the family's stores, and the recall one under the
    /// RECALL id.** `build_detail` derived the capture id once and handed it to
    /// both `projection_db_under` and `recall_db_under`, so every recall fact
    /// the Recall Viewer showed — "Recall store present", durable ingest
    /// coverage, repository-wide serves — was read from a file the writers
    /// never write. The panels had the identical defect and pin it in
    /// `feeds::savings::tests::recall_reads_the_writers_recall_repo_id_not_the_capture_id`;
    /// this is the viewer's half.
    ///
    /// The second assertion is family identity's: a member resolves the same two
    /// files as the main work tree, because there is one family and one pair of
    /// stores under it.
    #[test]
    fn the_viewer_opens_the_familys_recall_db_never_the_capture_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let main_git = rift_git::test_support::fake_repo(&main).unwrap();
        let member = tmp.path().join("member");
        rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();

        let capture_id = rift_git::repo_id_for(&main).unwrap();
        let recall_id = rift_store::recall_repo_id(&main).unwrap();
        assert_ne!(
            capture_id.as_str(),
            recall_id.as_str(),
            "the two id spaces genuinely differ, else this test proves nothing"
        );

        let (projection, recall) = family_stores(&cache, &main).unwrap();
        assert_eq!(
            projection,
            rift_store::projection_db_under(&cache, &capture_id),
            "the projection is the family's, in the capture id space"
        );
        assert_eq!(
            recall,
            rift_store::recall_db_under(&cache, &recall_id),
            "and recall.db is the file the serve/audit writers key"
        );
        assert_ne!(
            recall,
            rift_store::recall_db_under(&cache, &capture_id),
            "never the capture id: that file is nobody's"
        );

        assert_eq!(
            family_stores(&cache, &member).unwrap(),
            (projection, recall),
            "a member reads the family's two stores, not an empty per-path pair"
        );
    }

    /// worktree provenance, the projection reader's half: the catalogue's
    /// worktree badge comes from `checkpoint_worktree`, and a checkpoint with
    /// no row in it stays absent rather than acquiring a default. This is the
    /// first reader of that relation.
    ///
    /// The second half is the degradation: a projection still on a pre-v14
    /// schema has no such table at all, and the catalogue must not go dark
    /// over a badge — the map is simply empty, which is the same answer a
    /// v14 projection gives for a shard that carries no `worktree` member.
    #[test]
    fn the_catalogue_badge_reads_checkpoint_worktree_and_absence_stays_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("projection.db");
        let store = SqliteStore::open(&db).unwrap();
        store.create_canonical_schema().unwrap();
        let rt = crate::store::runtime().unwrap();
        rt.block_on(store.insert(
            "checkpoint_worktree",
            &[
                "checkpoint_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "schema_version",
            ],
            &[
                Value::text("cp-labelled"),
                Value::text("0123456789ab"),
                Value::text("swt-d"),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
        drop(store);

        let facts = projection_facts(&db);
        assert_eq!(
            facts.worktrees.get("cp-labelled").map(String::as_str),
            Some("swt-d")
        );
        assert_eq!(
            facts.worktrees.get("cp-pre-swt"),
            None,
            "a shard with no member projects no row — the badge renders \
             `unknown`, never `main` (worktree provenance)"
        );

        // A projection that never migrated to v14: the relation is gone, and
        // the facts still read.
        let store = SqliteStore::open(&db).unwrap();
        rt.block_on(async {
            let conn = rift_store::Store::conn(&store).await.unwrap();
            conn.execute("DROP TABLE checkpoint_worktree", &[]).await
        })
        .unwrap();
        drop(store);
        assert!(
            projection_facts(&db).worktrees.is_empty(),
            "an unreadable relation costs the badge, never the catalogue"
        );
    }

    /// A path with no recorded identity is a read error the viewer can render,
    /// never a silently empty pair of databases somewhere under the cache root.
    #[test]
    fn a_path_with_no_family_identity_is_a_refusal_not_an_empty_store() {
        let tmp = tempfile::tempdir().unwrap();
        let err = family_stores(&tmp.path().join("cache"), tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("no recorded Rift identity"),
            "{err}"
        );
    }

    #[test]
    fn an_empty_projection_file_yields_no_facts() {
        let repo = tempfile::tempdir().unwrap();
        let projection = repo.path().join("projection.db");
        std::fs::write(&projection, []).unwrap();
        let facts = projection_facts(&projection);
        assert!(facts.ids.is_empty());
    }

    #[test]
    fn batch_branch_walk_keeps_every_checkpoint_sharing_an_anchor() {
        let anchor = "a".repeat(40);
        let tip = "b".repeat(40);
        let substrate = MemorySubstrate::local()
            .with_commit(CommitMeta {
                oid: anchor.clone(),
                parents: Vec::new(),
                committed_ms: 1,
                summary: "anchor".to_owned(),
            })
            .with_commit(CommitMeta {
                oid: tip.clone(),
                parents: vec![anchor.clone()],
                committed_ms: 2,
                summary: "tip".to_owned(),
            })
            .with_branch("main", &tip);
        let entries = [entry("one", &anchor), entry("two", &anchor)];
        let containment = branch_containment(&substrate, &entries).unwrap();
        assert_eq!(containment.get("one"), Some(&vec!["main".to_owned()]));
        assert_eq!(containment.get("two"), Some(&vec!["main".to_owned()]));
    }

    #[test]
    fn a_branch_with_an_unknown_tip_contains_no_checkpoint() {
        let anchor = "d".repeat(40);
        let rich = entry(&"e".repeat(64), &anchor);
        let missing_tip = "f".repeat(40);
        let substrate = MemorySubstrate::local().with_branch("missing", &missing_tip);
        let no_anchor = CheckpointCatalogueEntry {
            anchor_commit: None,
            ..rich.clone()
        };
        assert!(branch_containment(&substrate, &[no_anchor])
            .unwrap()
            .is_empty());
        assert!(branch_containment(&substrate, std::slice::from_ref(&rich))
            .unwrap()
            .is_empty());
    }

    /// Three checkpoints, two sharing an anchor, in a catalogue whose rows
    /// carry recorded times so the newest is unambiguous.
    fn selection_fixture() -> (
        Vec<String>,
        String,
        String,
        CachedCatalogue,
        MemorySubstrate,
    ) {
        let anchor = "a".repeat(40);
        let other = "b".repeat(40);
        let ids = vec![
            format!("{}1", "0".repeat(63)),
            format!("{}2", "0".repeat(63)),
            format!("{}3", "1".repeat(63)),
        ];
        let mut entries = vec![entry(&ids[0], &anchor), entry(&ids[1], &anchor)];
        entries.push(entry(&ids[2], &other));
        let rows = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                checkpoint_row(
                    entry,
                    Some(i64::try_from(index).unwrap_or_default()),
                    TimestampProvenance::Recorded,
                    Vec::new(),
                    "unprojected",
                    None,
                )
            })
            .collect::<Vec<_>>();
        let cached = CachedCatalogue {
            entries: entries.clone(),
            rows,
        };
        let substrate = MemorySubstrate::local()
            .with_commit(CommitMeta {
                oid: anchor.clone(),
                parents: Vec::new(),
                committed_ms: 1,
                summary: "anchor".to_owned(),
            })
            .with_commit(CommitMeta {
                oid: other.clone(),
                parents: Vec::new(),
                committed_ms: 2,
                summary: "other".to_owned(),
            });
        (ids, anchor, other, cached, substrate)
    }

    #[test]
    fn selection_resolves_exact_ids_and_notices_every_ambiguous_or_missing_target() {
        let (ids, anchor, other, cached, substrate) = selection_fixture();
        assert!(resolve_selection(&substrate, &cached, None).1.is_none());
        assert_eq!(
            resolve_selection(&substrate, &cached, Some(&ids[2])).0,
            Some(ids[2].clone())
        );
        assert!(resolve_selection(&substrate, &cached, Some(&ids[0][..63]))
            .1
            .is_some());
        assert!(resolve_selection(&substrate, &cached, Some("not-a-target"))
            .1
            .is_some());
        assert!(resolve_selection(&substrate, &cached, Some(&anchor))
            .1
            .is_some());
        assert!(resolve_selection(&substrate, &cached, Some(&other))
            .0
            .is_some());
    }

    #[test]
    fn candidate_notices_cap_the_list_and_omit_the_selected_one() {
        let candidates = (0..7)
            .map(|index| TargetCandidate {
                checkpoint_id: format!("checkpoint-{index:02}-long"),
                metadata_anchor: index % 2 == 0,
                trailer: index % 2 == 1,
            })
            .collect::<Vec<_>>();
        assert!(candidate_notice(&candidates, None).contains("+2"));
        assert_eq!(
            candidate_notice(&candidates[..1], Some(&candidates[0].checkpoint_id)),
            "none"
        );
    }
}
