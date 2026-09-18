//! Strictly read-only Recall Viewer data builder.
//!
//! Every Git, projection, policy, transcript, diff, and syntax operation in
//! this module runs on Canopy's data worker. Stores are opened with SQLite's
//! read-only flag and only after an existence check; Git is accessed through
//! the object/ref reader seams. In particular, shadow refs are enumerated for
//! metadata only: no shadow artifact or payload is ever read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_canopy::{
    Event, SyntaxClass, ViewerCheckpointRow, ViewerDetail, ViewerErrorArea, ViewerEvent,
    ViewerFilterProgress, ViewerGeneration, ViewerLine, ViewerLineKind, ViewerOrigin,
    ViewerRequest, ViewerSpan, ViewerTimelineItem, ViewerTimelineSegment,
    ViewerTimestampProvenance,
};
use rift_config::{EffectiveConfig, PolicySource};
use rift_core::{Result, RiftError};
use rift_git::{BlobReader, GixBlobReader};
use rift_recall_policy::{load_serve_policy, PolicyEngine, ServePolicySource};
use rift_recall_substrate::catalogue::{CatalogueIssue, ResolutionKind, TargetCandidate};
use rift_recall_substrate::parse::{self, IssueKind, Locator, ParseIssue};
use rift_recall_substrate::{
    build_checkpoint_catalogue, resolve_explicit_target, CheckpointCatalogueEntry,
    CheckpointStorageKind, Dialect, GixSubstrate, SubstrateReader, TimestampProvenance,
    MAX_REACHABILITY_WALK,
};
use rift_recall_types::checkpoint::{CompactTranscriptRecord, TaskMetadata};
use rift_shadow_attribution::MAX_DIFF_BYTES;
use rift_store::window::CHECKPOINT_TIME_CTE;
use rift_store::{
    projection_db_for_family_under, recall_db_for_family_under, CanonicalStore, SqliteStore, Value,
    CREATED_AT_UNKNOWN,
};

use crate::store::runtime;

const MAX_DISPLAY_CHARS: usize = 4_096;
const FILTER_BATCH: usize = 64;
const DEFAULT_DIFF_CONTEXT: usize = 3;
const EXPANDED_DIFF_CONTEXT: usize = 24;
const MAX_DETAIL_CACHE: usize = 256;

#[derive(Clone)]
struct CachedCatalogue {
    entries: Vec<CheckpointCatalogueEntry>,
    rows: Vec<ViewerCheckpointRow>,
}

struct FilterJob {
    generation: ViewerGeneration,
    query: String,
    next: usize,
    matching_ids: Vec<String>,
}

/// Stateful, immutable-artifact cache owned by one data-worker thread.
///
/// The cache contains only parsed durable checkpoint facts. It has no flush or
/// persistence path; refresh replaces entries by repository identity.
pub struct ViewerWorker {
    root: PathBuf,
    catalogues: BTreeMap<String, CachedCatalogue>,
    filter_jobs: BTreeMap<String, FilterJob>,
    details: BTreeMap<(String, String, String), ViewerDetail>,
}

impl ViewerWorker {
    /// Construct a viewer builder against an explicit Rift cache root.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            catalogues: BTreeMap::new(),
            filter_jobs: BTreeMap::new(),
            details: BTreeMap::new(),
        }
    }

    /// Execute one viewer request and return bounded reducer events.
    ///
    /// Errors are converted to safe category/detail events; artifact bytes are
    /// never included in an error.
    #[must_use]
    pub fn handle(&mut self, request: ViewerRequest) -> Vec<Event> {
        match request {
            ViewerRequest::Catalogue {
                project,
                generation,
                target,
                refresh,
            } => self.catalogue(project, generation, target, refresh),
            ViewerRequest::VisibleExcerpts {
                project,
                generation,
                checkpoint_ids,
            } => self.visible_excerpts(project, generation, checkpoint_ids),
            ViewerRequest::Detail {
                project,
                generation,
                checkpoint_id,
            } => self.detail(project, generation, checkpoint_id),
            ViewerRequest::FilterScan {
                project,
                generation,
                query,
            } => self.filter_scan(project, generation, query),
        }
    }

    /// Whether a progressive filter scan has another bounded batch. The bus
    /// checks its request channel between batches, so a newer generation can
    /// replace an old scan without waiting for the remaining shards.
    #[must_use]
    pub fn has_filter_work(&self) -> bool {
        !self.filter_jobs.is_empty()
    }

    /// Scan at most one bounded transcript batch per active repository.
    #[must_use]
    pub fn advance_filter_scans(&mut self) -> Vec<Event> {
        let projects = self.filter_jobs.keys().cloned().collect::<Vec<_>>();
        projects
            .into_iter()
            .filter_map(|project| self.advance_filter_scan(&project))
            .collect()
    }

    fn catalogue(
        &mut self,
        project: String,
        generation: ViewerGeneration,
        target: Option<String>,
        refresh: bool,
    ) -> Vec<Event> {
        if let Some(job) = self.filter_jobs.get_mut(&project) {
            // A live refresh may replace the immutable catalogue underneath an
            // active query. Restart that same generation from the beginning so
            // its next bounded slice cannot index stale catalogue positions.
            job.next = 0;
            job.matching_ids.clear();
        }
        if refresh || !self.catalogues.contains_key(&project) {
            match load_catalogue(&self.root, Path::new(&project)) {
                Ok(catalogue) => {
                    self.catalogues.insert(project.clone(), catalogue);
                }
                Err(error) => {
                    return vec![viewer_error(
                        project,
                        generation,
                        ViewerErrorArea::Catalogue,
                        &error,
                    )];
                }
            }
        }
        let Some(cached) = self.catalogues.get(&project) else {
            return vec![Event::Viewer(ViewerEvent::Error {
                project,
                generation,
                area: ViewerErrorArea::Catalogue,
                message: "checkpoint catalogue unavailable".to_owned(),
            })];
        };
        let substrate = match GixSubstrate::open(Path::new(&project)) {
            Ok(substrate) => substrate,
            Err(error) => {
                return vec![viewer_error(
                    project,
                    generation,
                    ViewerErrorArea::Catalogue,
                    &error,
                )];
            }
        };
        let (selected, notice) = resolve_selection(&substrate, cached, target.as_deref());
        vec![Event::Viewer(ViewerEvent::Catalogue {
            project,
            generation,
            rows: cached.rows.clone(),
            selected,
            notice,
            complete: cached.rows.is_empty(),
        })]
    }

    fn visible_excerpts(
        &mut self,
        project: String,
        generation: ViewerGeneration,
        checkpoint_ids: Vec<String>,
    ) -> Vec<Event> {
        let Some(cached) = self.catalogues.get_mut(&project) else {
            return vec![Event::Viewer(ViewerEvent::Error {
                project,
                generation,
                area: ViewerErrorArea::Catalogue,
                message: "catalogue changed before excerpt enrichment".to_owned(),
            })];
        };
        let substrate = match GixSubstrate::open(Path::new(&project)) {
            Ok(substrate) => substrate,
            Err(error) => {
                return vec![viewer_error(
                    project,
                    generation,
                    ViewerErrorArea::Catalogue,
                    &error,
                )];
            }
        };
        let wanted: BTreeSet<&str> = checkpoint_ids.iter().map(String::as_str).collect();
        for row in &mut cached.rows {
            if !wanted.contains(row.checkpoint_id.as_str()) {
                continue;
            }
            row.excerpt = transcript_excerpt(&substrate, &row.checkpoint_id);
            row.excerpt_loading = false;
            if let Some(excerpt) = &row.excerpt {
                row.metadata_search.push(' ');
                row.metadata_search.push_str(&excerpt.to_lowercase());
            }
        }
        vec![Event::Viewer(ViewerEvent::Catalogue {
            project,
            generation,
            rows: cached.rows.clone(),
            selected: None,
            notice: None,
            complete: true,
        })]
    }

    fn detail(
        &mut self,
        project: String,
        generation: ViewerGeneration,
        checkpoint_id: String,
    ) -> Vec<Event> {
        let Some(cached) = self.catalogues.get(&project) else {
            return vec![Event::Viewer(ViewerEvent::Error {
                project,
                generation,
                area: ViewerErrorArea::Detail,
                message: "catalogue changed before checkpoint detail loaded".to_owned(),
            })];
        };
        let Some(entry) = cached
            .entries
            .iter()
            .find(|entry| entry.checkpoint_id == checkpoint_id)
            .cloned()
        else {
            return vec![Event::Viewer(ViewerEvent::Error {
                project,
                generation,
                area: ViewerErrorArea::Detail,
                message: "selected checkpoint is no longer present".to_owned(),
            })];
        };
        let identity = detail_identity(&entry);
        let key = (project.clone(), checkpoint_id, identity);
        if let Some(detail) = self.details.get(&key) {
            return vec![Event::Viewer(ViewerEvent::Detail {
                project,
                generation,
                detail: detail.clone(),
                complete: true,
            })];
        }
        match build_detail(&self.root, Path::new(&project), &entry) {
            Ok(detail) => {
                if self.details.len() >= MAX_DETAIL_CACHE {
                    if let Some(oldest_key) = self.details.keys().next().cloned() {
                        self.details.remove(&oldest_key);
                    }
                }
                self.details.insert(key, detail.clone());
                vec![Event::Viewer(ViewerEvent::Detail {
                    project,
                    generation,
                    detail,
                    complete: true,
                })]
            }
            Err(error) => vec![viewer_error(
                project,
                generation,
                ViewerErrorArea::Detail,
                &error,
            )],
        }
    }

    fn filter_scan(
        &mut self,
        project: String,
        generation: ViewerGeneration,
        query: String,
    ) -> Vec<Event> {
        let Some(cached) = self.catalogues.get(&project) else {
            return vec![Event::Viewer(ViewerEvent::Error {
                project,
                generation,
                area: ViewerErrorArea::Filter,
                message: "catalogue changed before transcript scan".to_owned(),
            })];
        };
        if query.is_empty() {
            self.filter_jobs.remove(&project);
            return vec![Event::Viewer(ViewerEvent::FilterProgress {
                project,
                generation,
                matching_ids: Vec::new(),
                progress: ViewerFilterProgress {
                    scanned: 0,
                    total: cached.entries.len(),
                    complete: true,
                },
            })];
        }
        let total = cached.entries.len();
        if total == 0 {
            return vec![Event::Viewer(ViewerEvent::FilterProgress {
                project,
                generation,
                matching_ids: Vec::new(),
                progress: ViewerFilterProgress {
                    scanned: 0,
                    total: 0,
                    complete: true,
                },
            })];
        }
        self.filter_jobs.insert(
            project.clone(),
            FilterJob {
                generation,
                query: query.to_lowercase(),
                next: 0,
                matching_ids: Vec::new(),
            },
        );
        self.advance_filter_scan(&project).into_iter().collect()
    }

    fn advance_filter_scan(&mut self, project: &str) -> Option<Event> {
        let mut job = self.filter_jobs.remove(project)?;
        let Some(cached) = self.catalogues.get(project) else {
            return Some(Event::Viewer(ViewerEvent::Error {
                project: project.to_owned(),
                generation: job.generation,
                area: ViewerErrorArea::Filter,
                message: "catalogue changed before transcript scan".to_owned(),
            }));
        };
        let substrate = match GixSubstrate::open(Path::new(project)) {
            Ok(substrate) => substrate,
            Err(error) => {
                return Some(viewer_error(
                    project.to_owned(),
                    job.generation,
                    ViewerErrorArea::Filter,
                    &error,
                ));
            }
        };
        let total = cached.entries.len();
        let end = job.next.saturating_add(FILTER_BATCH).min(total);
        for entry in cached
            .entries
            .iter()
            .skip(job.next)
            .take(end.saturating_sub(job.next))
        {
            if transcript_search_text(&substrate, entry)
                .is_some_and(|text| text.to_lowercase().contains(&job.query))
            {
                job.matching_ids.push(entry.checkpoint_id.clone());
            }
        }
        job.next = end;
        let complete = end == total;
        let event = Event::Viewer(ViewerEvent::FilterProgress {
            project: project.to_owned(),
            generation: job.generation,
            // Cumulative by design: the outbox keeps only the newest batch
            // under backpressure without losing earlier matches.
            matching_ids: job.matching_ids.clone(),
            progress: ViewerFilterProgress {
                scanned: end,
                total,
                complete,
            },
        });
        if !complete {
            self.filter_jobs.insert(project.to_owned(), job);
        }
        Some(event)
    }
}

fn viewer_error(
    project: String,
    generation: ViewerGeneration,
    area: ViewerErrorArea,
    error: &RiftError,
) -> Event {
    Event::Viewer(ViewerEvent::Error {
        project,
        generation,
        area,
        message: bounded(&error.to_string()),
    })
}

fn detail_identity(entry: &CheckpointCatalogueEntry) -> String {
    let mut parts = vec![
        entry.checkpoint_id.clone(),
        entry.anchor_commit.clone().unwrap_or_default(),
        entry.metadata.merkle_root.clone().unwrap_or_default(),
    ];
    parts.extend(entry.origins.iter().map(|origin| {
        format!(
            "{}:{}:{}",
            origin.storage_ref,
            origin.tip_oid.as_deref().unwrap_or("unknown"),
            origin.dialect.label()
        )
    }));
    parts.join("\u{1f}")
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
fn family_stores(root: &Path, project: &Path) -> Result<(PathBuf, PathBuf)> {
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

fn load_catalogue(root: &Path, project: &Path) -> Result<CachedCatalogue> {
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

#[derive(Default)]
struct ProjectionFacts {
    ids: BTreeSet<String>,
    session_times: BTreeMap<String, i64>,
    /// `checkpoint_id` → the worktree the shard was archived in, recorded by
    /// the first archiver. A checkpoint absent from this map is **unknown**: either its
    /// shard carries no `worktree` member (every shard written before
    /// provenance labelling) or this
    /// projection predates v14. Both render `unknown`, never `main`.
    worktrees: BTreeMap<String, String>,
}

fn projection_facts(path: &Path) -> ProjectionFacts {
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

fn branch_containment(
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

fn checkpoint_row(
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
            kind: match origin.kind {
                CheckpointStorageKind::Branch => "branch",
                CheckpointStorageKind::PerCheckpointRef => "per-checkpoint-ref",
            }
            .to_owned(),
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

fn issue_label(issue: &CatalogueIssue) -> String {
    bounded(&format!("{}: {}", issue.component, issue.detail))
}

fn map_timestamp_provenance(value: TimestampProvenance) -> ViewerTimestampProvenance {
    match value {
        TimestampProvenance::Recorded => ViewerTimestampProvenance::Recorded,
        TimestampProvenance::SessionDerived => ViewerTimestampProvenance::SessionDerived,
        TimestampProvenance::AnchorCommit => ViewerTimestampProvenance::AnchorCommit,
        TimestampProvenance::Unknown => ViewerTimestampProvenance::Unknown,
    }
}

fn compare_rows(a: &ViewerCheckpointRow, b: &ViewerCheckpointRow) -> std::cmp::Ordering {
    b.effective_at_ms
        .cmp(&a.effective_at_ms)
        .then_with(|| b.chain_position.cmp(&a.chain_position))
        .then_with(|| b.checkpoint_id.cmp(&a.checkpoint_id))
}

fn resolve_selection(
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

fn compact_utc(ms: i64) -> Option<String> {
    if ms < 0 {
        return None;
    }
    let seconds = ms / 1_000;
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_from_days(days)?;
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    Some(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}Z"
    ))
}

fn civil_from_days(days: i64) -> Option<(i64, i64, i64)> {
    let shifted = days.checked_add(719_468)?;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted.checked_sub(146_096)?
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Some((year, month, day))
}

fn bounded(input: &str) -> String {
    parse::sanitize_terminal_text(input)
        .chars()
        .take(MAX_DISPLAY_CHARS)
        .collect()
}

fn short(value: &str) -> String {
    value.chars().take(12).collect()
}

fn transcript_excerpt(substrate: &impl SubstrateReader, checkpoint_id: &str) -> Option<String> {
    let (records, boundary, artifact_lines, _) = transcript_records(substrate, checkpoint_id)?;
    let start = boundary.filter(|start| *start <= artifact_lines)?;
    records
        .iter()
        .rev()
        .filter(|record| record.artifact_line >= start)
        .filter(|record| {
            record.record_type.as_deref().is_some_and(|role| {
                role.eq_ignore_ascii_case("user") || role.eq_ignore_ascii_case("assistant")
            })
        })
        .find_map(record_text)
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.chars().take(240).collect())
}

fn transcript_search_text(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
) -> Option<String> {
    let (records, _, _, _) = transcript_records_for_entry(substrate, entry)?;
    let mut out = String::new();
    for record in records {
        if let Some(text) = record_text(&record) {
            out.push_str(&text);
            out.push('\n');
        }
    }
    Some(out)
}

type TranscriptBundle = (
    Vec<CompactTranscriptRecord>,
    Option<usize>,
    usize,
    Vec<ParseIssue>,
);

fn transcript_records(
    substrate: &impl SubstrateReader,
    checkpoint_id: &str,
) -> Option<TranscriptBundle> {
    let metadata = substrate
        .read_artifact(checkpoint_id, "metadata.json")
        .ok()
        .flatten()
        .map(|bytes| parse::parse_metadata(&bytes, checkpoint_id).value)
        .unwrap_or_default();
    transcript_records_for_metadata(substrate, checkpoint_id, &metadata)
}

fn transcript_records_for_entry(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
) -> Option<TranscriptBundle> {
    transcript_records_for_metadata(substrate, &entry.checkpoint_id, &entry.metadata)
}

fn transcript_records_for_metadata(
    substrate: &impl SubstrateReader,
    checkpoint_id: &str,
    metadata: &rift_recall_types::checkpoint::CheckpointMetadata,
) -> Option<TranscriptBundle> {
    let compact_name = match metadata.compact_transcript.as_deref() {
        Some(name) if safe_artifact_component(name) => name,
        Some(_) => {
            return Some((
                Vec::new(),
                metadata.compact_transcript_start,
                0,
                vec![ParseIssue {
                    checkpoint_id: checkpoint_id.to_owned(),
                    artifact: "metadata.json",
                    locator: Locator::WholeFile,
                    kind: IssueKind::Corrupt,
                    detail: "compact transcript artifact name is not a safe leaf".to_owned(),
                }],
            ));
        }
        None => "transcript.compact.jsonl",
    };
    if let Ok(Some(bytes)) = substrate.read_artifact(checkpoint_id, compact_name) {
        let parsed = parse::parse_compact_transcript(&bytes, checkpoint_id);
        return Some((
            parsed.value,
            metadata.compact_transcript_start,
            jsonl_line_count(&bytes),
            parsed.issues,
        ));
    }
    for fallback in ["transcript.jsonl", "full.jsonl"] {
        let Ok(Some(bytes)) = substrate.read_artifact(checkpoint_id, fallback) else {
            continue;
        };
        if fallback == "full.jsonl" {
            let parsed = parse::parse_full_events(&bytes, checkpoint_id);
            let records = parsed
                .value
                .into_iter()
                .enumerate()
                .map(|(artifact_line, event)| CompactTranscriptRecord {
                    artifact_line,
                    record_type: Some(event.kind),
                    content: event.payload,
                    extra: BTreeMap::from([(
                        "ts".to_owned(),
                        serde_json::Value::from(event.occurred_at_ms),
                    )]),
                    ..CompactTranscriptRecord::default()
                })
                .collect();
            return Some((records, None, jsonl_line_count(&bytes), parsed.issues));
        }
        let parsed = parse::parse_compact_transcript(&bytes, checkpoint_id);
        return Some((parsed.value, None, jsonl_line_count(&bytes), parsed.issues));
    }
    None
}

fn jsonl_line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        0
    } else {
        bytes
            .split(|byte| *byte == b'\n')
            .count()
            .saturating_sub(usize::from(bytes.ends_with(b"\n")))
    }
}

fn safe_artifact_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
}

fn record_text(record: &CompactTranscriptRecord) -> Option<String> {
    let value = if record.content.is_null() {
        record.message.get("content").unwrap_or(&record.message)
    } else {
        &record.content
    };
    let mut text = Vec::new();
    collect_explicit_text(value, &mut text);
    let joined = text
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then(|| bounded(&joined))
}

fn collect_explicit_text(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_explicit_text(value, out);
            }
        }
        serde_json::Value::Object(object) => {
            match object.get("type").and_then(serde_json::Value::as_str) {
                Some("tool_use") => {
                    let name = object
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("tool");
                    let id = object
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    out.push(format!("tool use: {name} ({id})"));
                }
                Some("tool_result") => {
                    let id = object
                        .get("tool_use_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    out.push(format!("tool result: {id}"));
                }
                _ => {
                    if let Some(text) = object.get("text").and_then(serde_json::Value::as_str) {
                        out.push(text.to_owned());
                    }
                }
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn record_tool_ids(record: &CompactTranscriptRecord) -> Vec<String> {
    let mut ids = Vec::new();
    collect_tool_ids(&record.content, &mut ids);
    collect_tool_ids(&record.message, &mut ids);
    ids.sort();
    ids.dedup();
    ids
}

fn collect_tool_ids(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_tool_ids(value, out);
            }
        }
        serde_json::Value::Object(object) => {
            if object.get("type").and_then(serde_json::Value::as_str) == Some("tool_use") {
                if let Some(id) = object.get("id").and_then(serde_json::Value::as_str) {
                    out.push(id.to_owned());
                }
            }
            if let Some(content) = object.get("content") {
                collect_tool_ids(content, out);
            }
        }
        _ => {}
    }
}

fn build_detail(
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

fn checkpoint_summary_segment(
    entry: &CheckpointCatalogueEntry,
    session_time: Option<i64>,
    containing: &[String],
) -> ViewerTimelineSegment {
    let (timestamp, provenance) = entry.effective_timestamp(session_time);
    let mut lines = vec![
        fact_line("checkpoint", &entry.checkpoint_id),
        fact_line(
            "anchor commit",
            entry.anchor_commit.as_deref().unwrap_or("unavailable"),
        ),
        fact_line(
            "chain",
            &entry.chain_position.map_or_else(
                || "unchained".to_owned(),
                |position| format!("canonical position {position}"),
            ),
        ),
        fact_line(
            "timestamp",
            &timestamp.map_or_else(
                || "unknown".to_owned(),
                |ms| {
                    format!(
                        "{} ({})",
                        compact_utc(ms).unwrap_or_else(|| "unknown".to_owned()),
                        map_timestamp_provenance(provenance).label()
                    )
                },
            ),
        ),
        fact_line(
            "strategy",
            entry.metadata.strategy.as_deref().unwrap_or("unavailable"),
        ),
        fact_line(
            "sessions",
            if entry.metadata.sessions.is_empty() {
                "none recorded".to_owned()
            } else {
                entry.metadata.sessions.join(", ")
            }
            .as_str(),
        ),
        fact_line(
            "local branches",
            if containing.is_empty() {
                "none proven".to_owned()
            } else {
                containing.join(", ")
            }
            .as_str(),
        ),
    ];
    for origin in &entry.origins {
        let kind = match origin.kind {
            CheckpointStorageKind::Branch => "branch",
            CheckpointStorageKind::PerCheckpointRef => "per-checkpoint-ref",
        };
        lines.push(fact_line(
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
        ));
    }
    for issue in &entry.issues {
        lines.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            format!("{}: {}", issue.component, issue.detail),
        ));
    }
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
        }],
    }
}

#[allow(clippy::too_many_lines)] // one factual read-only summary over config and two stores
fn recall_summary_segment(
    project: &Path,
    projection_path: &Path,
    recall_path: &Path,
    entry: &CheckpointCatalogueEntry,
) -> ViewerTimelineSegment {
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
    lines.push(fact_line(
        "capture projection",
        if projection_path.is_file() {
            "present (opened read-only)"
        } else {
            "absent"
        },
    ));
    lines.push(fact_line(
        "Recall store",
        if recall_path.is_file() {
            "present (opened read-only); no in-memory-live claim"
        } else {
            "absent; no in-memory-live claim"
        },
    ));

    let (discovered_ids, discovered_origins) =
        discovered_checkpoint_counts(project).unwrap_or((0, 0));
    if let Some(store) = open_existing_read_only(recall_path) {
        let ingested = query_count(&store, "SELECT COUNT(*) FROM n_checkpoint", &[]).unwrap_or(0);
        let serve_calls =
            query_count(&store, "SELECT COUNT(*) FROM memd_audit_event", &[]).unwrap_or(0);
        let attributed_calls =
            query_count(&store, "SELECT COUNT(*) FROM serve_call_event", &[]).unwrap_or(0);
        let latest = query_count(&store, "SELECT MAX(recorded_at) FROM memd_audit_event", &[]);
        lines.push(fact_line(
            "durable ingest coverage",
            &format!(
                "{ingested}/{discovered_ids} unioned checkpoint IDs; {discovered_origins} physical storage origin(s) discovered"
            ),
        ));
        lines.push(fact_line(
            "repository-wide Recall serves/calls",
            &format!(
                "{serve_calls} serve decisions · {attributed_calls} attributed calls · latest {}",
                latest
                    .and_then(compact_utc)
                    .unwrap_or_else(|| "unknown".to_owned())
            ),
        ));
        lines.push(ViewerLine::plain(
            ViewerLineKind::Dim,
            "serve/call totals are repository-wide and are not attributed to this checkpoint",
        ));
    } else {
        lines.push(fact_line(
            "durable ingest coverage",
            &format!(
                "0/{discovered_ids} unioned checkpoint IDs; {discovered_origins} physical storage origin(s); Recall store unavailable"
            ),
        ));
    }
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
        }],
    }
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

fn transcript_segment(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
) -> ViewerTimelineSegment {
    let Some((records, boundary, artifact_lines, issues)) =
        transcript_records_for_entry(substrate, entry)
    else {
        return ViewerTimelineSegment {
            title: "Transcript".to_owned(),
            boundary: Some(
                "no stored redacted compact or fallback transcript is available".to_owned(),
            ),
            ordered_by_time: false,
            items: vec![warning_item(
                "transcript-unavailable",
                "transcript unavailable",
                "compact and redacted full transcript artifacts are absent",
            )],
        };
    };
    let mut items = Vec::new();
    if boundary.is_none() {
        items.push(warning_item(
            "transcript-boundary",
            "checkpoint-new boundary unavailable",
            "the stored transcript is shown without claiming which records are new",
        ));
    } else if boundary.is_some_and(|start| start > artifact_lines) {
        items.push(warning_item(
            "transcript-boundary-invalid",
            "checkpoint-new boundary is out of range",
            "no boundary was inferred; all valid records remain inspectable",
        ));
    }
    for issue in issues {
        items.push(warning_item(
            &format!("transcript-issue-{}", items.len()),
            "transcript record skipped",
            &format!("{}: {}", issue.artifact, issue.detail),
        ));
    }

    let task_ids = entry
        .metadata
        .tasks
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut attached_tasks = BTreeSet::new();
    for (index, record) in records.iter().enumerate() {
        let role = record.record_type.as_deref().unwrap_or("unknown record");
        let timestamp = record_timestamp(record);
        let text = record_text(record)
            .unwrap_or_else(|| "structured record; no displayable text".to_owned());
        let full_session_only = boundary
            .filter(|start| *start <= artifact_lines)
            .is_some_and(|start| record.artifact_line < start);
        items.push(ViewerTimelineItem {
            id: format!("transcript:{index}"),
            title: bounded(role),
            timestamp,
            summary: Vec::new(),
            body: text
                .lines()
                .map(|line| ViewerLine::plain(ViewerLineKind::Text, bounded(line)))
                .collect(),
            expanded_body: None,
            collapsed: false,
            full_session_only,
        });
        for tool_id in record_tool_ids(record) {
            if task_ids.contains(&tool_id) {
                items.push(task_item(substrate, entry, &tool_id));
                attached_tasks.insert(tool_id);
            }
        }
    }
    for task_id in task_ids.difference(&attached_tasks) {
        let mut item = task_item(substrate, entry, task_id);
        item.summary.insert(
            0,
            ViewerLine::plain(
                ViewerLineKind::Warning,
                "matching parent tool-use record was not found",
            ),
        );
        items.push(item);
    }
    ViewerTimelineSegment {
        title: "Transcript".to_owned(),
        boundary: Some(
            "artifact order only; Full session reveals records before compact_transcript_start"
                .to_owned(),
        ),
        ordered_by_time: false,
        items,
    }
}

#[allow(clippy::too_many_lines)] // one bounded artifact read and truthful fallback assembly
fn task_item(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
    tool_id: &str,
) -> ViewerTimelineItem {
    if !safe_artifact_component(tool_id) {
        return warning_item(
            &format!("task-invalid:{}", bounded(tool_id)),
            "nested task unavailable",
            "task identifier is not a safe artifact path component",
        );
    }
    let meta_path = format!("tasks/{tool_id}/metadata.json");
    let metadata = substrate
        .read_artifact(&entry.checkpoint_id, &meta_path)
        .ok()
        .flatten()
        .map_or_else(
            || parse::Outcome {
                value: TaskMetadata::default(),
                issues: Vec::new(),
            },
            |bytes| parse::parse_task_metadata(&bytes, &entry.checkpoint_id),
        );
    let transcript_name = metadata
        .value
        .compact_transcript
        .as_deref()
        .unwrap_or("transcript.compact.jsonl");
    let safe_transcript_name = safe_artifact_component(transcript_name);
    let transcript = safe_transcript_name
        .then(|| format!("tasks/{tool_id}/{transcript_name}"))
        .and_then(|transcript_path| {
            substrate
                .read_artifact(&entry.checkpoint_id, &transcript_path)
                .ok()
                .flatten()
        })
        .map(|bytes| parse::parse_task_compact_transcript(&bytes, &entry.checkpoint_id));
    let mut summary = vec![fact_line(
        "agent",
        if metadata.value.agent_id.is_empty() {
            "unavailable"
        } else {
            &metadata.value.agent_id
        },
    )];
    if let Some(tokens) = metadata
        .value
        .input_tokens
        .zip(metadata.value.output_tokens)
    {
        summary.push(fact_line(
            "tokens",
            &format!("{} input · {} output", tokens.0, tokens.1),
        ));
    }
    summary.push(fact_line(
        "compact boundary",
        &metadata
            .value
            .compact_transcript_start
            .map_or_else(|| "unavailable".to_owned(), |start| start.to_string()),
    ));
    for issue in metadata.issues {
        summary.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            format!("task metadata: {}", issue.detail),
        ));
    }
    if !safe_transcript_name {
        summary.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            "task compact transcript artifact name is not a safe leaf",
        ));
    }
    let mut body = Vec::new();
    if let Some(parsed) = transcript {
        for record in parsed.value {
            let role = record.record_type.as_deref().unwrap_or("record");
            body.push(ViewerLine::plain(ViewerLineKind::Heading, bounded(role)));
            if let Some(text) = record_text(&record) {
                body.extend(
                    text.lines()
                        .map(|line| ViewerLine::plain(ViewerLineKind::Text, bounded(line))),
                );
            }
        }
        for issue in parsed.issues {
            body.push(ViewerLine::plain(
                ViewerLineKind::Warning,
                format!("task transcript: {}", issue.detail),
            ));
        }
    } else {
        body.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            "redacted compact task transcript unavailable",
        ));
    }
    ViewerTimelineItem {
        id: format!("task:{tool_id}"),
        title: format!("nested task {tool_id}"),
        timestamp: metadata.value.started_at_ms.and_then(compact_utc),
        summary,
        body,
        expanded_body: None,
        collapsed: true,
        full_session_only: false,
    }
}

fn record_timestamp(record: &CompactTranscriptRecord) -> Option<String> {
    let value = record.extra.get("ts")?;
    match value {
        serde_json::Value::Number(number) => number.as_i64().and_then(compact_utc),
        serde_json::Value::String(text) => rift_recall_substrate::parse_rfc3339_ms(text)
            .and_then(compact_utc)
            .or_else(|| Some(bounded(text))),
        _ => None,
    }
}

fn warning_item(id: &str, title: &str, text: &str) -> ViewerTimelineItem {
    ViewerTimelineItem {
        id: id.to_owned(),
        title: title.to_owned(),
        timestamp: None,
        summary: vec![ViewerLine::plain(ViewerLineKind::Warning, bounded(text))],
        body: Vec::new(),
        expanded_body: None,
        collapsed: false,
        full_session_only: false,
    }
}

fn fact_line(label: &str, value: &str) -> ViewerLine {
    ViewerLine::plain(
        ViewerLineKind::Text,
        format!("{}: {}", bounded(label), bounded(value)),
    )
}

/// The most per-rule rows the assembly card lists before folding the rest
/// into one "… and N more" line — a reader wants the headline detectors,
/// not a 30-row table.
const MAX_PER_RULE_ROWS: usize = 8;

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

/// The redaction-report facts of the assembly card: validity, counts, packs,
/// input digest, the per-rule rows, and the report's
/// parse issues as warnings; one warning when the artifact is absent.
fn redaction_report_lines(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
) -> Vec<ViewerLine> {
    let mut facts = Vec::new();
    if let Ok(Some(bytes)) = substrate.read_artifact(&entry.checkpoint_id, "redaction_report.json")
    {
        let report = parse::parse_redaction_report(&bytes, &entry.checkpoint_id);
        facts.extend([
            fact_line(
                "redaction structurally valid",
                &report.value.is_valid().to_string(),
            ),
            fact_line("redacted spans", &report.value.total_spans.to_string()),
            fact_line("masked bytes", &report.value.bytes_masked.to_string()),
            fact_line(
                "redaction rule packs",
                if report.value.rule_pack_versions.is_empty() {
                    "none recorded".to_owned()
                } else {
                    report.value.rule_pack_versions.join(", ")
                }
                .as_str(),
            ),
            fact_line(
                "redaction input digest",
                report
                    .value
                    .input_sha256
                    .as_deref()
                    .unwrap_or("unavailable"),
            ),
        ]);
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
    } else {
        facts.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            "redaction report unavailable",
        ));
    }
    facts
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

#[derive(Clone)]
struct RecordedHunk {
    id: String,
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
}

#[derive(Default)]
struct RecordedFile {
    revision_id: String,
    path: String,
    new_blob_oid: String,
    hunks: Vec<RecordedHunk>,
}

#[allow(clippy::too_many_lines)] // one batched read of recorded revisions/hunks and blob seams
fn diff_items(
    project: &Path,
    projection_path: &Path,
    entry: &CheckpointCatalogueEntry,
) -> Result<Vec<ViewerTimelineItem>> {
    let Some(anchor) = entry.anchor_commit.as_deref() else {
        return Ok(vec![warning_item(
            "diff-anchor-unavailable",
            "diff metadata only",
            "anchor commit unavailable; recorded ranges cannot be paired with blobs",
        )]);
    };
    let Some(store) = open_existing_read_only(projection_path) else {
        return Ok(vec![warning_item(
            "diff-projection-unavailable",
            "diff metadata unavailable",
            "capture projection is absent or unreadable; no database was created",
        )]);
    };
    let rt = runtime()?;
    let mut files = BTreeMap::<String, RecordedFile>::new();
    if let Ok(rows) = rt.block_on(store.query(
        "SELECT DISTINCT fr.file_revision_id, fr.path, fr.blob_oid \
         FROM attribution a JOIN file_revision fr \
           ON fr.file_revision_id = a.file_revision_id \
         WHERE a.checkpoint_id = ?1 ORDER BY fr.path, fr.file_revision_id",
        &[Value::text(entry.checkpoint_id.clone())],
    )) {
        for row in rows {
            let (Ok(revision_id), Ok(path), Ok(blob_oid)) =
                (row.get_text(0), row.get_text(1), row.get_text(2))
            else {
                continue;
            };
            files.insert(
                revision_id.clone(),
                RecordedFile {
                    revision_id,
                    path,
                    new_blob_oid: blob_oid,
                    hunks: Vec::new(),
                },
            );
        }
    }
    let rows = rt.block_on(store.query(
        "SELECT h.hunk_id, h.file_revision_id, h.old_start, h.old_len, \
                h.new_start, h.new_len, fr.path, fr.blob_oid \
         FROM changed_hunk h JOIN file_revision fr \
           ON fr.file_revision_id = h.file_revision_id \
         WHERE h.commit_id = ?1 \
         ORDER BY fr.path, h.old_start, h.new_start, h.hunk_id",
        &[Value::text(anchor.to_owned())],
    ))?;
    for row in rows {
        let (
            Ok(id),
            Ok(revision_id),
            Ok(old_start),
            Ok(old_len),
            Ok(new_start),
            Ok(new_len),
            Ok(path),
            Ok(blob_oid),
        ) = (
            row.get_text(0),
            row.get_text(1),
            row.get_i64(2),
            row.get_i64(3),
            row.get_i64(4),
            row.get_i64(5),
            row.get_text(6),
            row.get_text(7),
        )
        else {
            continue;
        };
        let Some(hunk) = checked_hunk(id, old_start, old_len, new_start, new_len) else {
            continue;
        };
        let file = files
            .entry(revision_id.clone())
            .or_insert_with(|| RecordedFile {
                revision_id,
                path,
                new_blob_oid: blob_oid,
                hunks: Vec::new(),
            });
        file.hunks.push(hunk);
    }
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let blob_reader = GixBlobReader::open(project)?;
    let new_tree = blob_reader
        .tree_entries_at(anchor)?
        .into_iter()
        .map(|entry| (entry.path, entry.blob_oid))
        .collect::<BTreeMap<_, _>>();
    let old_tree = match blob_reader.parent_of(anchor)? {
        Some(parent) => blob_reader
            .tree_entries_at(&parent)?
            .into_iter()
            .map(|entry| (entry.path, entry.blob_oid))
            .collect::<BTreeMap<_, _>>(),
        None => BTreeMap::new(),
    };
    let policy = DiffPolicy::load(project);
    let mut items = Vec::new();
    for file in files.into_values() {
        items.push(render_file_diff(
            &blob_reader,
            &old_tree,
            &new_tree,
            &policy,
            file,
        ));
    }
    Ok(items)
}

fn checked_hunk(
    id: String,
    old_start: i64,
    old_len: i64,
    new_start: i64,
    new_len: i64,
) -> Option<RecordedHunk> {
    Some(RecordedHunk {
        id,
        old_start: usize::try_from(old_start).ok()?,
        old_len: usize::try_from(old_len).ok()?,
        new_start: usize::try_from(new_start).ok()?,
        new_len: usize::try_from(new_len).ok()?,
    })
}

struct DiffPolicy {
    config: Option<EffectiveConfig>,
    serve: rift_recall_policy::ServePolicy,
    fail_closed_reason: Option<&'static str>,
}

impl DiffPolicy {
    fn load(project: &Path) -> Self {
        let rift_dir = project.join(".rift");
        let config = EffectiveConfig::load(&rift_dir).ok();
        let serve = load_serve_policy(&rift_dir);
        let config_refused = config
            .as_ref()
            .is_some_and(|config| matches!(config.policy_source(), PolicySource::Refused(_)));
        let fail_closed_reason = if config.is_none() {
            Some("invalid configuration")
        } else if config_refused {
            Some("refused configuration")
        } else {
            match serve.source {
                ServePolicySource::Invalid => Some("invalid Recall serve policy"),
                ServePolicySource::Refused => Some("refused Recall serve policy"),
                ServePolicySource::Declared | ServePolicySource::Default => None,
            }
        };
        Self {
            config,
            serve: serve.policy,
            fail_closed_reason,
        }
    }

    fn permits(&self, path: &str) -> std::result::Result<(), String> {
        if let Some(reason) = self.fail_closed_reason {
            return Err(format!("{reason}; all source code is metadata-only"));
        }
        let Some(config) = self.config.as_ref() else {
            return Err("configuration unavailable; all source code is metadata-only".to_owned());
        };
        let engine = PolicyEngine::new(config).with_serve_policy(self.serve.clone());
        if engine.is_denied_path(path) {
            return Err("denied path; ranges and counts only".to_owned());
        }
        if engine.exact_source_required(path) {
            return Err("sensitive path; ranges and counts only".to_owned());
        }
        Ok(())
    }
}

fn render_file_diff(
    reader: &impl BlobReader,
    old_tree: &BTreeMap<String, String>,
    new_tree: &BTreeMap<String, String>,
    policy: &DiffPolicy,
    mut file: RecordedFile,
) -> ViewerTimelineItem {
    file.hunks
        .sort_by_key(|hunk| (hunk.old_start, hunk.new_start, hunk.id.clone()));
    let removed = file.hunks.iter().map(|hunk| hunk.old_len).sum::<usize>();
    let added = file.hunks.iter().map(|hunk| hunk.new_len).sum::<usize>();
    let summary = vec![ViewerLine::plain(
        ViewerLineKind::DiffFile,
        format!("{}  +{added} -{removed}", bounded(&file.path)),
    )];
    if let Err(reason) = policy.permits(&file.path) {
        return ViewerTimelineItem {
            id: format!("diff:{}", file.revision_id),
            title: file.path,
            timestamp: None,
            summary,
            body: vec![ViewerLine::plain(ViewerLineKind::Warning, reason)],
            expanded_body: None,
            collapsed: false,
            full_session_only: false,
        };
    }
    if file.hunks.is_empty() {
        return ViewerTimelineItem {
            id: format!("diff:{}", file.revision_id),
            title: file.path,
            timestamp: None,
            summary,
            body: vec![ViewerLine::plain(
                ViewerLineKind::Warning,
                "changed file has no recorded textual hunk ranges (binary, oversized, or unavailable)",
            )],
            expanded_body: None,
            collapsed: false,
            full_session_only: false,
        };
    }
    let old_oid = old_tree.get(&file.path);
    let new_oid = new_tree.get(&file.path).or(Some(&file.new_blob_oid));
    let old_bytes = match old_oid {
        Some(oid) => match reader.read_blob(oid) {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(_) => {
                return metadata_only_diff(file, summary, "old-side blob missing or unreadable");
            }
        },
        // A path absent from the first parent's tree is a factual added file
        // (including every path in a root commit), so its old side is empty.
        None => Vec::new(),
    };
    let Some(new_bytes) = new_oid.and_then(|oid| reader.read_blob(oid).ok().flatten()) else {
        return metadata_only_diff(file, summary, "new-side blob missing or unreadable");
    };
    if old_bytes.len() > MAX_DIFF_BYTES || new_bytes.len() > MAX_DIFF_BYTES {
        return metadata_only_diff(file, summary, "blob exceeds the existing 5 MiB diff limit");
    }
    if old_bytes.contains(&0) || new_bytes.contains(&0) {
        return metadata_only_diff(file, summary, "binary blob; ranges and counts only");
    }
    let (Ok(old_text), Ok(new_text)) = (
        std::str::from_utf8(&old_bytes),
        std::str::from_utf8(&new_bytes),
    ) else {
        return metadata_only_diff(file, summary, "non-text blob; ranges and counts only");
    };
    let old_lines = logical_lines(old_text);
    let new_lines = logical_lines(new_text);
    if !ranges_fit(&file.hunks, old_lines.len(), new_lines.len()) {
        return metadata_only_diff(
            file,
            summary,
            "recorded hunk range is outside the readable blobs",
        );
    }
    let old_highlights = highlight_file(&file.path, old_text);
    let new_highlights = highlight_file(&file.path, new_text);
    let body = render_recorded_hunks(
        &file.hunks,
        &old_lines,
        &new_lines,
        old_highlights.as_deref(),
        new_highlights.as_deref(),
        DEFAULT_DIFF_CONTEXT,
    );
    let expanded = render_recorded_hunks(
        &file.hunks,
        &old_lines,
        &new_lines,
        old_highlights.as_deref(),
        new_highlights.as_deref(),
        EXPANDED_DIFF_CONTEXT,
    );
    ViewerTimelineItem {
        id: format!("diff:{}", file.revision_id),
        title: file.path,
        timestamp: None,
        summary,
        body,
        expanded_body: Some(expanded),
        collapsed: false,
        full_session_only: false,
    }
}

fn metadata_only_diff(
    file: RecordedFile,
    summary: Vec<ViewerLine>,
    reason: &str,
) -> ViewerTimelineItem {
    ViewerTimelineItem {
        id: format!("diff:{}", file.revision_id),
        title: file.path,
        timestamp: None,
        summary,
        body: vec![ViewerLine::plain(ViewerLineKind::Warning, reason)],
        expanded_body: None,
        collapsed: false,
        full_session_only: false,
    }
}

fn logical_lines(text: &str) -> Vec<&str> {
    text.split_terminator('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

fn ranges_fit(hunks: &[RecordedHunk], old_len: usize, new_len: usize) -> bool {
    hunks.iter().all(|hunk| {
        hunk.old_start.saturating_add(hunk.old_len) <= old_len
            && hunk.new_start.saturating_add(hunk.new_len) <= new_len
    })
}

fn render_recorded_hunks(
    hunks: &[RecordedHunk],
    old: &[&str],
    new: &[&str],
    old_highlights: Option<&[Vec<ViewerSpan>]>,
    new_highlights: Option<&[Vec<ViewerSpan>]>,
    context: usize,
) -> Vec<ViewerLine> {
    let mut out = Vec::new();
    let mut previous_old_end = 0usize;
    let mut previous_new_end = 0usize;
    for (hunk_index, hunk) in hunks.iter().enumerate() {
        let old_before = hunk.old_start.saturating_sub(context);
        let new_before = hunk.new_start.saturating_sub(context);
        let omitted_old = old_before.saturating_sub(previous_old_end);
        let omitted_new = new_before.saturating_sub(previous_new_end);
        let omitted = omitted_old.min(omitted_new);
        if omitted > 0 {
            out.push(ViewerLine::plain(
                ViewerLineKind::Gap,
                format!("{omitted} unchanged lines"),
            ));
        }
        out.push(ViewerLine::plain(
            ViewerLineKind::DiffHunk,
            format!(
                "@@ -{},{} +{},{} @@",
                hunk.old_start.saturating_add(1),
                hunk.old_len,
                hunk.new_start.saturating_add(1),
                hunk.new_len
            ),
        ));
        let visible_old_before = old_before.max(previous_old_end);
        let visible_new_before = new_before.max(previous_new_end);
        let before_count = hunk
            .old_start
            .saturating_sub(visible_old_before)
            .min(hunk.new_start.saturating_sub(visible_new_before));
        for offset in 0..before_count {
            let old_index = hunk.old_start - before_count + offset;
            let new_index = hunk.new_start - before_count + offset;
            out.push(source_line(
                ViewerLineKind::Context,
                Some(old_index + 1),
                Some(new_index + 1),
                new.get(new_index).copied().unwrap_or(""),
                new_highlights.and_then(|lines| lines.get(new_index)),
            ));
        }
        for offset in 0..hunk.old_len {
            let index = hunk.old_start + offset;
            out.push(source_line(
                ViewerLineKind::Removal,
                Some(index + 1),
                None,
                old.get(index).copied().unwrap_or(""),
                old_highlights.and_then(|lines| lines.get(index)),
            ));
        }
        for offset in 0..hunk.new_len {
            let index = hunk.new_start + offset;
            out.push(source_line(
                ViewerLineKind::Addition,
                None,
                Some(index + 1),
                new.get(index).copied().unwrap_or(""),
                new_highlights.and_then(|lines| lines.get(index)),
            ));
        }
        let old_end = hunk.old_start + hunk.old_len;
        let new_end = hunk.new_start + hunk.new_len;
        let next = hunks.get(hunk_index.saturating_add(1));
        let next_old = next.map_or(old.len(), |candidate| candidate.old_start);
        let next_new = next.map_or(new.len(), |candidate| candidate.new_start);
        let after_count = context
            .min(next_old.saturating_sub(old_end))
            .min(next_new.saturating_sub(new_end));
        for offset in 0..after_count {
            let old_index = old_end + offset;
            let new_index = new_end + offset;
            out.push(source_line(
                ViewerLineKind::Context,
                Some(old_index + 1),
                Some(new_index + 1),
                new.get(new_index).copied().unwrap_or(""),
                new_highlights.and_then(|lines| lines.get(new_index)),
            ));
        }
        previous_old_end = old_end.saturating_add(after_count);
        previous_new_end = new_end.saturating_add(after_count);
    }
    out
}

fn source_line(
    kind: ViewerLineKind,
    old_line: Option<usize>,
    new_line: Option<usize>,
    text: &str,
    spans: Option<&Vec<ViewerSpan>>,
) -> ViewerLine {
    ViewerLine {
        old_line: old_line.and_then(|line| u32::try_from(line).ok()),
        new_line: new_line.and_then(|line| u32::try_from(line).ok()),
        kind,
        spans: spans.cloned().unwrap_or_else(|| {
            vec![ViewerSpan {
                text: bounded(text),
                class: SyntaxClass::Plain,
            }]
        }),
    }
}

fn highlight_file(path: &str, text: &str) -> Option<Vec<Vec<ViewerSpan>>> {
    use syntect::easy::ScopeRegionIterator;
    use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

    let syntaxes = SyntaxSet::load_defaults_newlines();
    // An unrecognised extension is a plain-text fallback, not a successful
    // highlight. Returning `None` keeps that distinction explicit and lets the
    // common source-line builder apply the same sanitized fallback used for a
    // parser error.
    let syntax = syntaxes.find_syntax_for_file(path).ok().flatten()?;
    let mut parse_state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut output = Vec::new();
    for raw_line in text.split_inclusive('\n') {
        let display = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let display = display.strip_suffix('\r').unwrap_or(display);
        if raw_line.len() > 32 * 1024 {
            output.push(vec![ViewerSpan {
                text: bounded(display),
                class: SyntaxClass::Plain,
            }]);
            continue;
        }
        let operations = parse_state.parse_line(raw_line, &syntaxes).ok()?;
        let mut spans: Vec<ViewerSpan> = Vec::new();
        for (region, operation) in ScopeRegionIterator::new(&operations, raw_line) {
            stack.apply(operation).ok()?;
            let region = region.strip_suffix('\n').unwrap_or(region);
            let region = region.strip_suffix('\r').unwrap_or(region);
            if region.is_empty() {
                continue;
            }
            let class = classify_scope(&stack);
            if let Some(previous) = spans.last_mut() {
                if previous.class == class {
                    previous.text.push_str(region);
                    continue;
                }
            }
            spans.push(ViewerSpan {
                text: bounded(region),
                class,
            });
        }
        if spans.is_empty() {
            spans.push(ViewerSpan {
                text: bounded(display),
                class: SyntaxClass::Plain,
            });
        }
        output.push(spans);
    }
    if text.is_empty() || text.ends_with('\n') {
        output.truncate(logical_lines(text).len());
    }
    Some(output)
}

fn classify_scope(stack: &syntect::parsing::ScopeStack) -> SyntaxClass {
    for scope in stack.scopes.iter().rev() {
        let scope = scope.build_string();
        if scope.contains("comment") {
            return SyntaxClass::Comment;
        }
        if scope.contains("string") {
            return SyntaxClass::String;
        }
        if scope.contains("numeric") || scope.contains("constant") {
            return SyntaxClass::Number;
        }
        if scope.contains("entity.name.function")
            || scope.contains("support.function")
            || scope.contains("meta.function-call")
        {
            return SyntaxClass::Function;
        }
        if scope.contains("entity.name.type")
            || scope.contains("support.type")
            || scope.contains("storage.type")
        {
            return SyntaxClass::Type;
        }
        if scope.contains("keyword.operator") || scope.contains("punctuation") {
            return SyntaxClass::Operator;
        }
        if scope.contains("keyword") || scope.contains("storage.modifier") {
            return SyntaxClass::Keyword;
        }
    }
    SyntaxClass::Plain
}

#[allow(clippy::too_many_lines)] // one factual lifecycle query with honest unavailable branches
fn lifecycle_segment(
    projection_path: &Path,
    entry: &CheckpointCatalogueEntry,
) -> ViewerTimelineSegment {
    // Only a recorded checkpoint timestamp can truthfully timestamp the
    // condensation/archive decision. Session-derived and anchor-commit times
    // remain useful catalogue fallbacks, but are not decision chronology.
    let decision_time = entry.recorded_at_ms.filter(|value| *value >= 0);
    let provenance = if decision_time.is_some() {
        TimestampProvenance::Recorded
    } else {
        TimestampProvenance::Unknown
    };
    let decision = ViewerTimelineItem {
        id: "condensation-decision".to_owned(),
        title: "condensation / archive decision".to_owned(),
        timestamp: decision_time.and_then(compact_utc),
        summary: vec![
            fact_line(
                "strategy",
                entry.metadata.strategy.as_deref().unwrap_or("unavailable"),
            ),
            fact_line(
                "timestamp provenance",
                map_timestamp_provenance(provenance).label(),
            ),
            fact_line(
                "decision evidence",
                "checkpoint metadata; no separate decision row is inferred",
            ),
        ],
        body: Vec::new(),
        expanded_body: None,
        collapsed: false,
        full_session_only: false,
    };
    let Some(store) = open_existing_read_only(projection_path) else {
        return ViewerTimelineSegment {
            title: "Lifecycle / soak".to_owned(),
            boundary: Some("ordered only by each verification run's recorded timestamp".to_owned()),
            ordered_by_time: true,
            items: vec![
                decision,
                warning_item(
                    "lifecycle-unavailable",
                    "lifecycle unavailable",
                    "capture projection is absent or unreadable",
                ),
            ],
        };
    };
    let Ok(rt) = runtime() else {
        return ViewerTimelineSegment {
            title: "Lifecycle / soak".to_owned(),
            boundary: Some("ordered only by each verification run's recorded timestamp".to_owned()),
            ordered_by_time: true,
            items: vec![
                decision,
                warning_item(
                    "lifecycle-worker",
                    "lifecycle unavailable",
                    "data-worker runtime unavailable",
                ),
            ],
        };
    };
    let rows = rt.block_on(store.query(
        "SELECT vr.run_id, vr.branch, vr.exit_code, vr.checked_at, vr.outcome_sha256, \
                (SELECT COUNT(*) FROM verification_divergence vd WHERE vd.run_id = vr.run_id) \
         FROM verification_witness vw JOIN verification_run vr ON vr.run_id = vw.run_id \
         WHERE vw.checkpoint_id = ?1 \
         ORDER BY vr.checked_at DESC, vr.run_id DESC",
        &[Value::text(entry.checkpoint_id.clone())],
    ));
    let mut items = vec![decision];
    let mut witness_count = 0usize;
    if let Ok(rows) = rows {
        for row in rows {
            let (
                Ok(run_id),
                Ok(branch),
                Ok(exit_code),
                Ok(checked_at),
                Ok(outcome),
                Ok(divergences),
            ) = (
                row.get_text(0),
                row.get_text(1),
                row.get_i64(2),
                row.get_i64(3),
                row.get_text(4),
                row.get_i64(5),
            )
            else {
                continue;
            };
            let mut summary = vec![
                fact_line("branch", &branch),
                fact_line("exit code", &exit_code.to_string()),
                fact_line("outcome digest", &outcome),
                fact_line("divergences", &divergences.to_string()),
            ];
            if divergences > 0 {
                summary.push(ViewerLine::plain(
                    ViewerLineKind::Warning,
                    "verification recorded one or more divergences",
                ));
            }
            items.push(ViewerTimelineItem {
                id: format!("verify:{run_id}"),
                title: format!("verification run {}", short(&run_id)),
                timestamp: compact_utc(checked_at),
                summary,
                body: Vec::new(),
                expanded_body: None,
                collapsed: false,
                full_session_only: false,
            });
            witness_count = witness_count.saturating_add(1);
        }
    }
    if witness_count == 0 {
        items.push(warning_item(
            "lifecycle-no-witness",
            "no verification witness",
            "no durable verification run is linked to this condensation; absence is not a pass",
        ));
    }
    items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.id.cmp(&b.id)));
    ViewerTimelineSegment {
        title: "Lifecycle / soak".to_owned(),
        boundary: Some(
            "verification witnesses are sorted only by checked_at; no order is inferred across other segments"
                .to_owned(),
        ),
        ordered_by_time: true,
        items,
    }
}

fn open_existing_read_only(path: &Path) -> Option<SqliteStore> {
    path.is_file()
        .then(|| SqliteStore::open_read_only(path).ok())
        .flatten()
}

fn query_count(store: &SqliteStore, sql: &str, values: &[Value]) -> Option<i64> {
    let rt = runtime().ok()?;
    rt.block_on(store.query_opt_i64(sql, values)).ok().flatten()
}

#[cfg(test)]
mod tests {
    use rift_git::MemoryBlobReader;
    use rift_recall_substrate::{CommitMeta, MemorySubstrate};
    use rift_recall_types::checkpoint::CheckpointMetadata;

    use super::*;

    fn entry(id: &str, anchor: &str) -> CheckpointCatalogueEntry {
        CheckpointCatalogueEntry {
            checkpoint_id: id.to_owned(),
            origins: Vec::new(),
            chain_position: None,
            metadata: CheckpointMetadata::default(),
            metadata_present: true,
            recorded_at_ms: None,
            anchor_commit: Some(anchor.to_owned()),
            anchor_committed_ms: None,
            issues: Vec::new(),
        }
    }

    /// **The viewer opens the family's stores, and the recall one under the
    /// RECALL id.** `build_detail` derived the capture id once and handed it to
    /// both `projection_db_under` and `recall_db_under`, so every recall fact
    /// the Recall Viewer showed — "Recall store present", durable ingest
    /// coverage, repository-wide serves — was read from a file the writers
    /// never write. The panels had the identical defect and pin it in
    /// `feeds::tests::recall_reads_the_writers_recall_repo_id_not_the_capture_id`;
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
    fn recorded_hunks_keep_three_context_lines_and_explicit_gaps() {
        let old = (0..18).map(|n| format!("old-{n}")).collect::<Vec<_>>();
        let new = (0..18).map(|n| format!("new-{n}")).collect::<Vec<_>>();
        let old_refs = old.iter().map(String::as_str).collect::<Vec<_>>();
        let new_refs = new.iter().map(String::as_str).collect::<Vec<_>>();
        let hunks = [
            RecordedHunk {
                id: "h1".to_owned(),
                old_start: 5,
                old_len: 1,
                new_start: 5,
                new_len: 1,
            },
            RecordedHunk {
                id: "h2".to_owned(),
                old_start: 14,
                old_len: 1,
                new_start: 14,
                new_len: 1,
            },
        ];
        let lines = render_recorded_hunks(&hunks, &old_refs, &new_refs, None, None, 3);
        assert_eq!(lines[0].kind, ViewerLineKind::Gap);
        assert_eq!(lines[0].spans[0].text, "2 unchanged lines");
        assert!(lines
            .iter()
            .any(|line| { line.kind == ViewerLineKind::Removal && line.old_line == Some(6) }));
        assert!(lines
            .iter()
            .any(|line| { line.kind == ViewerLineKind::Addition && line.new_line == Some(15) }));
        let first_hunk = lines
            .iter()
            .position(|line| line.kind == ViewerLineKind::DiffHunk)
            .unwrap();
        assert_eq!(
            lines[first_hunk + 1..first_hunk + 4]
                .iter()
                .filter(|line| line.kind == ViewerLineKind::Context)
                .count(),
            3
        );
    }

    #[test]
    fn recorded_ranges_outside_blobs_fail_closed() {
        let hunk = RecordedHunk {
            id: "h".to_owned(),
            old_start: 8,
            old_len: 2,
            new_start: 0,
            new_len: 1,
        };
        assert!(!ranges_fit(&[hunk], 9, 1));
    }

    fn permissive_diff_policy() -> DiffPolicy {
        DiffPolicy {
            config: Some(EffectiveConfig::secure_default().unwrap()),
            serve: rift_recall_policy::ServePolicy::default(),
            fail_closed_reason: None,
        }
    }

    fn one_hunk_file(path: &str) -> RecordedFile {
        RecordedFile {
            revision_id: "revision".to_owned(),
            path: path.to_owned(),
            new_blob_oid: "new".to_owned(),
            hunks: vec![RecordedHunk {
                id: "hunk".to_owned(),
                old_start: 0,
                old_len: 1,
                new_start: 0,
                new_len: 1,
            }],
        }
    }

    fn item_text(item: &ViewerTimelineItem) -> String {
        item.summary
            .iter()
            .chain(&item.body)
            .flat_map(|line| &line.spans)
            .map(|span| span.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn unreadable_old_blob_is_metadata_only_not_invented_as_a_root_addition() {
        let reader = MemoryBlobReader::new().with_blob("new", b"new\n");
        let old_tree = BTreeMap::from([("src/lib.rs".to_owned(), "missing-old".to_owned())]);
        let new_tree = BTreeMap::from([("src/lib.rs".to_owned(), "new".to_owned())]);
        let item = render_file_diff(
            &reader,
            &old_tree,
            &new_tree,
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&item).contains("old-side blob missing or unreadable"));
        assert!(!item
            .body
            .iter()
            .any(|line| line.kind == ViewerLineKind::Addition));
    }

    #[test]
    fn binary_oversized_sensitive_and_fail_closed_diffs_keep_metadata() {
        let maps = || {
            (
                BTreeMap::from([("src/lib.rs".to_owned(), "old".to_owned())]),
                BTreeMap::from([("src/lib.rs".to_owned(), "new".to_owned())]),
            )
        };
        let (old_tree, new_tree) = maps();
        let binary = MemoryBlobReader::new()
            .with_blob("old", b"old\0")
            .with_blob("new", b"new\0");
        let item = render_file_diff(
            &binary,
            &old_tree,
            &new_tree,
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&item).contains("binary blob"));

        let oversized = MemoryBlobReader::new()
            .with_blob("old", b"old\n")
            .with_blob("new", &vec![b'x'; MAX_DIFF_BYTES + 1]);
        let item = render_file_diff(
            &oversized,
            &old_tree,
            &new_tree,
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&item).contains("exceeds the existing"));

        let item = render_file_diff(
            &MemoryBlobReader::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &permissive_diff_policy(),
            one_hunk_file("project/.env"),
        );
        assert!(item_text(&item).contains("sensitive path"));

        let fail_closed = DiffPolicy {
            config: Some(EffectiveConfig::secure_default().unwrap()),
            serve: rift_recall_policy::ServePolicy::default(),
            fail_closed_reason: Some("invalid configuration"),
        };
        let item = render_file_diff(
            &MemoryBlobReader::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &fail_closed,
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&item).contains("all source code is metadata-only"));
    }

    #[test]
    fn syntax_highlighting_is_semantic_and_has_plain_fallback() {
        let highlighted = highlight_file("main.rs", "fn main() { let n = 3; }\n").unwrap();
        assert!(highlighted.iter().flatten().any(|span| {
            matches!(
                span.class,
                SyntaxClass::Keyword | SyntaxClass::Function | SyntaxClass::Number
            )
        }));
        assert!(highlight_file("asset.unknown-rift-language", "plain\n").is_none());
    }

    #[test]
    fn compact_date_rejects_unknown_and_formats_utc() {
        assert_eq!(compact_utc(CREATED_AT_UNKNOWN), None);
        assert_eq!(compact_utc(0).as_deref(), Some("1970-01-01 00:00Z"));
    }

    #[test]
    fn display_helpers_cover_structured_and_hostile_factual_inputs() {
        assert_eq!(jsonl_line_count(b""), 0);
        assert_eq!(jsonl_line_count(b"one"), 1);
        assert_eq!(jsonl_line_count(b"one\n"), 1);
        assert_eq!(jsonl_line_count(b"one\ntwo"), 2);

        for safe in ["task", "task-id", "unicode-λ"] {
            assert!(safe_artifact_component(safe), "safe leaf: {safe}");
        }
        for unsafe_name in ["", ".", "..", "a/b", "a\\b", "line\nbreak"] {
            assert!(
                !safe_artifact_component(unsafe_name),
                "unsafe leaf: {unsafe_name:?}"
            );
        }

        let value = serde_json::json!([
            "plain",
            {"text": "object text"},
            {"type": "tool_use", "name": "read", "id": "call-1"},
            {"type": "tool_use"},
            {"type": "tool_result", "tool_use_id": "call-1"},
            {"type": "tool_result"},
            {"content": [{"type": "tool_use", "id": "call-2"}]},
            null,
            true,
            7
        ]);
        let mut text = Vec::new();
        collect_explicit_text(&value, &mut text);
        assert!(text.iter().any(|part| part == "plain"));
        assert!(text.iter().any(|part| part.contains("tool use: read")));
        let mut ids = Vec::new();
        collect_tool_ids(&value, &mut ids);
        assert_eq!(ids, ["call-1", "call-2"]);

        let mut record = CompactTranscriptRecord {
            content: value,
            ..CompactTranscriptRecord::default()
        };
        assert!(record_text(&record).is_some());
        assert_eq!(record_tool_ids(&record), ["call-1", "call-2"]);
        record.content = serde_json::Value::Null;
        record.message = serde_json::json!({"content": "message fallback"});
        assert_eq!(record_text(&record).as_deref(), Some("message fallback"));
        record.message = serde_json::json!({"ignored": 1});
        assert!(record_text(&record).is_none());

        for (value, expected) in [
            (serde_json::json!(0), Some("1970-01-01 00:00Z")),
            (serde_json::json!(-1), None),
            (
                serde_json::json!("1970-01-01T00:00:00Z"),
                Some("1970-01-01 00:00Z"),
            ),
            (
                serde_json::json!("recorded without parseable time"),
                Some("recorded without parseable time"),
            ),
            (serde_json::json!(true), None),
        ] {
            record.extra = BTreeMap::from([("ts".to_owned(), value)]);
            assert_eq!(record_timestamp(&record).as_deref(), expected);
        }
        record.extra.clear();
        assert!(record_timestamp(&record).is_none());

        assert!(civil_from_days(-719_468).is_some());
        assert!(civil_from_days(-719_469).is_some());
        assert!(civil_from_days(i64::MAX).is_none());
        assert!(compact_utc(i64::MAX).is_some());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn selection_and_worker_failure_matrix_stays_deterministic() {
        let anchor = "a".repeat(40);
        let other = "b".repeat(40);
        let ids = [
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

        let cache = tempfile::tempdir().unwrap();
        let missing = cache
            .path()
            .join("missing-repository")
            .to_string_lossy()
            .into_owned();
        let mut worker = ViewerWorker::new(cache.path().to_path_buf());
        for request in [
            ViewerRequest::VisibleExcerpts {
                project: missing.clone(),
                generation: 1,
                checkpoint_ids: vec![ids[0].clone()],
            },
            ViewerRequest::Detail {
                project: missing.clone(),
                generation: 2,
                checkpoint_id: ids[0].clone(),
            },
            ViewerRequest::FilterScan {
                project: missing.clone(),
                generation: 3,
                query: "query".to_owned(),
            },
            ViewerRequest::Catalogue {
                project: missing.clone(),
                generation: 4,
                target: None,
                refresh: true,
            },
        ] {
            assert!(matches!(
                worker.handle(request).as_slice(),
                [Event::Viewer(ViewerEvent::Error { .. })]
            ));
        }

        worker.catalogues.insert(
            missing.clone(),
            CachedCatalogue {
                entries: Vec::new(),
                rows: Vec::new(),
            },
        );
        assert!(matches!(
            worker
                .handle(ViewerRequest::FilterScan {
                    project: missing.clone(),
                    generation: 5,
                    query: "query".to_owned(),
                })
                .as_slice(),
            [Event::Viewer(ViewerEvent::FilterProgress {
                progress: ViewerFilterProgress { complete: true, .. },
                ..
            })]
        ));
        assert!(matches!(
            worker
                .handle(ViewerRequest::FilterScan {
                    project: missing.clone(),
                    generation: 6,
                    query: String::new(),
                })
                .as_slice(),
            [Event::Viewer(ViewerEvent::FilterProgress {
                progress: ViewerFilterProgress { scanned: 0, .. },
                ..
            })]
        ));
        assert!(matches!(
            worker
                .handle(ViewerRequest::Detail {
                    project: missing,
                    generation: 7,
                    checkpoint_id: ids[0].clone(),
                })
                .as_slice(),
            [Event::Viewer(ViewerEvent::Error { .. })]
        ));
    }

    #[test]
    // 101 lines: one over, since `checkpoint_row` gained its worktree
    // argument. The body is a single linear walk through the worker's cached
    // paths and splitting it would hide what it is checking.
    #[allow(clippy::too_many_lines)]
    fn cached_worker_paths_enrich_and_build_detail_without_writes() {
        let repo = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q", "--object-format=sha1"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        let project = repo.path().to_string_lossy().into_owned();
        let mut checkpoint = entry(&"7".repeat(64), &"a".repeat(40));
        checkpoint.anchor_commit = None;
        let row = checkpoint_row(
            &checkpoint,
            None,
            TimestampProvenance::Unknown,
            Vec::new(),
            "unprojected",
            None,
        );
        let root = tempfile::tempdir().unwrap();
        let mut worker = ViewerWorker::new(root.path().to_path_buf());
        worker.catalogues.insert(
            project.clone(),
            CachedCatalogue {
                entries: vec![checkpoint.clone()],
                rows: vec![row],
            },
        );
        worker.filter_jobs.insert(
            project.clone(),
            FilterJob {
                generation: 1,
                query: "absent".to_owned(),
                next: 1,
                matching_ids: vec![checkpoint.checkpoint_id.clone()],
            },
        );

        let catalogue = worker.handle(ViewerRequest::Catalogue {
            project: project.clone(),
            generation: 2,
            target: Some(checkpoint.checkpoint_id.clone()),
            refresh: false,
        });
        assert!(matches!(
            catalogue.as_slice(),
            [Event::Viewer(ViewerEvent::Catalogue {
                selected: Some(selected),
                ..
            })] if selected == &checkpoint.checkpoint_id
        ));
        let job = worker.filter_jobs.get(&project).unwrap();
        assert_eq!(job.next, 0);
        assert!(job.matching_ids.is_empty());

        let excerpts = worker.handle(ViewerRequest::VisibleExcerpts {
            project: project.clone(),
            generation: 3,
            checkpoint_ids: vec![checkpoint.checkpoint_id.clone()],
        });
        assert!(matches!(
            excerpts.as_slice(),
            [Event::Viewer(ViewerEvent::Catalogue {
                rows,
                complete: true,
                ..
            })] if !rows[0].excerpt_loading
        ));

        worker.filter_jobs.remove(&project);
        let scan = worker.handle(ViewerRequest::FilterScan {
            project: project.clone(),
            generation: 4,
            query: "not present".to_owned(),
        });
        assert!(matches!(
            scan.as_slice(),
            [Event::Viewer(ViewerEvent::FilterProgress {
                progress: ViewerFilterProgress {
                    scanned: 1,
                    total: 1,
                    complete: true,
                },
                ..
            })]
        ));
        assert!(worker.advance_filter_scan("unknown").is_none());

        let detail = worker.handle(ViewerRequest::Detail {
            project: project.clone(),
            generation: 5,
            checkpoint_id: checkpoint.checkpoint_id.clone(),
        });
        assert!(matches!(
            detail.as_slice(),
            [Event::Viewer(ViewerEvent::Detail { complete: true, .. })]
        ));
        let cached = worker.handle(ViewerRequest::Detail {
            project,
            generation: 6,
            checkpoint_id: checkpoint.checkpoint_id,
        });
        assert!(matches!(
            cached.as_slice(),
            [Event::Viewer(ViewerEvent::Detail { complete: true, .. })]
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn summary_transcript_diff_and_lifecycle_fallbacks_are_explicit() {
        let anchor = "a".repeat(40);
        let mut rich = entry(&"c".repeat(64), &anchor);
        rich.chain_position = Some(7);
        rich.recorded_at_ms = Some(0);
        rich.anchor_committed_ms = Some(1_000);
        rich.metadata.strategy = Some("manual".to_owned());
        rich.metadata.sessions = vec!["session-a".to_owned(), "session-b".to_owned()];
        rich.metadata.tasks = vec!["task-missing".to_owned(), "../unsafe".to_owned()];
        rich.metadata.merkle_root = Some("root".to_owned());
        rich.metadata.prev_hash = Some("previous".to_owned());
        rich.metadata.canon_version = Some(1);
        rich.metadata_present = false;
        rich.issues.push(CatalogueIssue {
            component: "metadata".to_owned(),
            detail: "partial".to_owned(),
        });
        rich.origins.extend([
            rift_recall_substrate::CheckpointOrigin {
                checkpoint_id: rich.checkpoint_id.clone(),
                dialect: Dialect::Rift,
                storage_ref: "refs/rift/checkpoints/main".to_owned(),
                kind: CheckpointStorageKind::Branch,
                tip_oid: Some(anchor.clone()),
            },
            rift_recall_substrate::CheckpointOrigin {
                checkpoint_id: rich.checkpoint_id.clone(),
                dialect: Dialect::Legacy,
                storage_ref: "refs/checkpoints/legacy".to_owned(),
                kind: CheckpointStorageKind::PerCheckpointRef,
                tip_oid: None,
            },
        ]);
        let summary = checkpoint_summary_segment(&rich, Some(2_000), &["main".to_owned()]);
        assert!(item_text(&summary.items[0]).contains("canonical position 7"));
        let sparse = checkpoint_summary_segment(&entry("sparse", &anchor), None, &[]);
        assert!(item_text(&sparse.items[0]).contains("none recorded"));

        let project = tempfile::tempdir().unwrap();
        let projection = project.path().join("missing-projection.db");
        let recall = project.path().join("missing-recall.db");
        let recall_summary = recall_summary_segment(project.path(), &projection, &recall, &rich);
        assert!(item_text(&recall_summary.items[0]).contains("Recall store unavailable"));

        let empty_substrate = MemorySubstrate::local();
        let missing_transcript = transcript_segment(&empty_substrate, &rich);
        assert!(item_text(&missing_transcript.items[0]).contains("compact and redacted"));
        assert!(
            item_text(&task_item(&empty_substrate, &rich, "../unsafe")).contains("safe artifact")
        );
        assert!(
            item_text(&task_item(&empty_substrate, &rich, "task-missing")).contains("unavailable")
        );

        let assembly = assembly_segment(project.path(), &empty_substrate, &projection, &rich);
        assert!(assembly
            .items
            .iter()
            .any(|item| item.id == "assembly-facts"));
        assert!(assembly
            .items
            .iter()
            .any(|item| item.id == "diff-projection-unavailable"));
        let lifecycle = lifecycle_segment(&projection, &rich);
        assert!(lifecycle
            .items
            .iter()
            .any(|item| item.id == "lifecycle-unavailable"));

        assert!(checked_hunk("ok".to_owned(), 0, 0, 0, 0).is_some());
        for values in [(-1, 0, 0, 0), (0, -1, 0, 0), (0, 0, -1, 0), (0, 0, 0, -1)] {
            assert!(
                checked_hunk("bad".to_owned(), values.0, values.1, values.2, values.3).is_none()
            );
        }

        let reader = MemoryBlobReader::new()
            .with_blob("old", b"old\n")
            .with_blob("new", b"new\n")
            .with_blob("invalid", &[0xff]);
        let empty_hunks = RecordedFile {
            revision_id: "empty".to_owned(),
            path: "src/lib.rs".to_owned(),
            new_blob_oid: "new".to_owned(),
            hunks: Vec::new(),
        };
        assert!(item_text(&render_file_diff(
            &reader,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &permissive_diff_policy(),
            empty_hunks,
        ))
        .contains("no recorded textual hunk"));

        let valid = render_file_diff(
            &reader,
            &BTreeMap::from([("src/lib.rs".to_owned(), "old".to_owned())]),
            &BTreeMap::from([("src/lib.rs".to_owned(), "new".to_owned())]),
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(valid.expanded_body.is_some());

        let invalid = render_file_diff(
            &reader,
            &BTreeMap::new(),
            &BTreeMap::from([("src/lib.rs".to_owned(), "invalid".to_owned())]),
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&invalid).contains("non-text blob"));

        let missing_new = render_file_diff(
            &MemoryBlobReader::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&missing_new).contains("new-side blob missing"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn remaining_read_only_fallbacks_keep_their_factual_labels() {
        let anchor = "d".repeat(40);
        let mut rich = entry(&"e".repeat(64), &anchor);
        rich.metadata_present = true;

        let repo = tempfile::tempdir().unwrap();
        let projection = repo.path().join("projection.db");
        let recall = repo.path().join("recall.db");
        std::fs::write(&projection, []).unwrap();
        std::fs::write(&recall, []).unwrap();
        let facts = projection_facts(&projection);
        assert!(facts.ids.is_empty());
        let summary = recall_summary_segment(repo.path(), &projection, &recall, &rich);
        let summary_text = item_text(&summary.items[0]);
        assert!(summary_text.contains("present (opened read-only)"));
        assert!(!summary_text.contains("metadata is unavailable"));
        let lifecycle = lifecycle_segment(&projection, &rich);
        assert!(lifecycle
            .items
            .iter()
            .any(|item| item.id == "lifecycle-no-witness" || item.id == "lifecycle-unavailable"));

        let missing_tip = "f".repeat(40);
        let substrate = MemorySubstrate::local().with_branch("missing", &missing_tip);
        let no_anchor = CheckpointCatalogueEntry {
            anchor_commit: None,
            ..rich.clone()
        };
        assert!(branch_containment(&substrate, &[no_anchor])
            .unwrap()
            .is_empty());
        assert!(branch_containment(&substrate, &[rich.clone()])
            .unwrap()
            .is_empty());

        let artifacts = MemorySubstrate::local()
            .with_artifact(
                &rich.checkpoint_id,
                "tasks/task-rich/metadata.json",
                br#"{"tool_use_id":"task-rich","agent_id":"codex","started_at_ms":0,"input_tokens":4,"output_tokens":2,"compact_transcript_start":0}"#,
            )
            .with_artifact(
                &rich.checkpoint_id,
                "tasks/task-rich/transcript.compact.jsonl",
                br#"{"type":"assistant","content":"nested result"}
{"type":"system","content":{"ignored":true}}
"#,
            )
            .with_artifact(
                &rich.checkpoint_id,
                "redaction_report.json",
                format!(
                    r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}","total_spans":2,"bytes_masked":8}}"#,
                    "a".repeat(64)
                )
                .as_bytes(),
            );
        let task = task_item(&artifacts, &rich, "task-rich");
        assert!(item_text(&task).contains("4 input"));
        assert!(item_text(&task).contains("nested result"));
        let assembly = assembly_segment(repo.path(), &artifacts, &projection, &rich);
        assert!(item_text(&assembly.items[0]).contains("builtin@1"));

        let no_config = DiffPolicy {
            config: None,
            serve: rift_recall_policy::ServePolicy::default(),
            fail_closed_reason: None,
        };
        assert!(no_config.permits("src/lib.rs").is_err());

        let reader = MemoryBlobReader::new().with_blob("added", b"added\n");
        let added = RecordedFile {
            revision_id: "added".to_owned(),
            path: "src/added.rs".to_owned(),
            new_blob_oid: "added".to_owned(),
            hunks: vec![RecordedHunk {
                id: "addition".to_owned(),
                old_start: 0,
                old_len: 0,
                new_start: 0,
                new_len: 1,
            }],
        };
        assert!(render_file_diff(
            &reader,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &permissive_diff_policy(),
            added,
        )
        .expanded_body
        .is_some());

        let highlighted = highlight_file(
            "matrix.rs",
            "// comment\nstruct Kind(u32);\nfn call() { if true { return; } let text = \"value\"; let n = 3 + 4; }\n",
        )
        .unwrap();
        let classes = highlighted
            .iter()
            .flatten()
            .map(|span| span.class)
            .collect::<Vec<_>>();
        for class in [
            SyntaxClass::Comment,
            SyntaxClass::String,
            SyntaxClass::Number,
            SyntaxClass::Type,
            SyntaxClass::Keyword,
            SyntaxClass::Operator,
        ] {
            assert!(
                classes.contains(&class),
                "missing class {class:?}: {classes:?}"
            );
        }

        let supplied = vec![ViewerSpan {
            text: "styled".to_owned(),
            class: SyntaxClass::Function,
        }];
        assert_eq!(
            source_line(
                ViewerLineKind::Text,
                Some(1),
                Some(2),
                "plain",
                Some(&supplied)
            )
            .spans,
            supplied
        );
        assert_eq!(logical_lines("a\r\nb\n"), ["a", "b"]);
    }

    #[test]
    fn compact_boundary_uses_physical_lines_even_when_a_prior_record_is_corrupt() {
        let mut checkpoint = entry("checkpoint", &"a".repeat(40));
        checkpoint.metadata.compact_transcript_start = Some(2);
        let substrate = MemorySubstrate::local()
            .with_artifact(
                "checkpoint",
                "metadata.json",
                b"{\"compact_transcript_start\":2}",
            )
            .with_artifact(
                "checkpoint",
                "transcript.compact.jsonl",
                b"{\"type\":\"user\",\"content\":\"before\"}\ncorrupt\n{\"type\":\"assistant\",\"content\":\"after\"}\n",
            );
        assert_eq!(
            transcript_excerpt(&substrate, "checkpoint").as_deref(),
            Some("after")
        );
        let segment = transcript_segment(&substrate, &checkpoint);
        let before = segment
            .items
            .iter()
            .find(|item| item.id == "transcript:0")
            .unwrap();
        let after = segment
            .items
            .iter()
            .find(|item| item.id == "transcript:1")
            .unwrap();
        assert!(before.full_session_only);
        assert!(!after.full_session_only);
        assert!(segment
            .items
            .iter()
            .any(|item| item.title == "transcript record skipped"));
    }

    #[test]
    fn configured_artifact_names_cannot_escape_the_checkpoint_subtree() {
        let mut checkpoint = entry("checkpoint", &"a".repeat(40));
        checkpoint.metadata.compact_transcript = Some("../raw-shadow.jsonl".to_owned());
        checkpoint.metadata.compact_transcript_start = Some(0);
        let substrate = MemorySubstrate::local()
            .with_artifact(
                "checkpoint",
                "metadata.json",
                b"{\"compact_transcript\":\"../raw-shadow.jsonl\",\"compact_transcript_start\":0}",
            )
            .with_artifact(
                "checkpoint",
                "../raw-shadow.jsonl",
                b"{\"type\":\"user\",\"content\":\"must-not-render\"}\n",
            );
        assert!(transcript_excerpt(&substrate, "checkpoint").is_none());
        let segment = transcript_segment(&substrate, &checkpoint);
        assert!(!segment.items.iter().any(|item| {
            item.body.iter().any(|line| {
                line.spans
                    .iter()
                    .any(|span| span.text.contains("must-not-render"))
            })
        }));
        assert!(segment.items.iter().any(|item| {
            item.summary.iter().any(|line| {
                line.spans
                    .iter()
                    .any(|span| span.text.contains("not a safe leaf"))
            })
        }));
    }

    #[test]
    fn thousand_checkpoint_filter_is_bounded_and_replacement_cancels_old_generation() {
        let repo = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q", "--object-format=sha1"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        let project = repo.path().to_string_lossy().into_owned();
        let entries = (0..1_000)
            .map(|index| entry(&format!("{index:064x}"), &"a".repeat(40)))
            .collect::<Vec<_>>();
        let rows = entries
            .iter()
            .map(|entry| {
                checkpoint_row(
                    entry,
                    None,
                    TimestampProvenance::Unknown,
                    Vec::new(),
                    "unprojected",
                    None,
                )
            })
            .collect();
        let cache = tempfile::tempdir().unwrap();
        let mut worker = ViewerWorker::new(cache.path().to_path_buf());
        worker
            .catalogues
            .insert(project.clone(), CachedCatalogue { entries, rows });

        let first = worker.handle(ViewerRequest::FilterScan {
            project: project.clone(),
            generation: 1,
            query: "old query".to_owned(),
        });
        assert!(matches!(
            first.as_slice(),
            [Event::Viewer(ViewerEvent::FilterProgress {
                generation: 1,
                progress: ViewerFilterProgress {
                    scanned: FILTER_BATCH,
                    complete: false,
                    ..
                },
                ..
            })]
        ));

        let replacement = worker.handle(ViewerRequest::FilterScan {
            project: project.clone(),
            generation: 2,
            query: "replacement".to_owned(),
        });
        assert!(matches!(
            replacement.as_slice(),
            [Event::Viewer(ViewerEvent::FilterProgress {
                generation: 2,
                progress: ViewerFilterProgress {
                    scanned: FILTER_BATCH,
                    ..
                },
                ..
            })]
        ));

        let mut previous = FILTER_BATCH;
        let mut batches = 1usize;
        while worker.has_filter_work() {
            let events = worker.advance_filter_scans();
            assert_eq!(events.len(), 1);
            let Event::Viewer(ViewerEvent::FilterProgress {
                generation,
                progress,
                ..
            }) = events.first().unwrap()
            else {
                panic!("filter scan emitted the wrong event")
            };
            assert_eq!(*generation, 2, "generation 1 was cancelled");
            assert!(progress.scanned.saturating_sub(previous) <= FILTER_BATCH);
            previous = progress.scanned;
            batches = batches.saturating_add(1);
        }
        assert_eq!(previous, 1_000);
        assert_eq!(batches, 1_000usize.div_ceil(FILTER_BATCH));
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
