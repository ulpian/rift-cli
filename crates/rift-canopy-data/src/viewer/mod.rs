//! Strictly read-only Recall Viewer data builder.
//!
//! Every Git, projection, policy, transcript, diff, and syntax operation in
//! this module runs on Canopy's data worker. Stores are opened with SQLite's
//! read-only flag and only after an existence check; Git is accessed through
//! the object/ref reader seams. In particular, shadow refs are enumerated for
//! metadata only: no shadow artifact or payload is ever read.
//!
//! One module per concern, because each reads a different substrate: the
//! catalogue reads refs and the projection's time facts, the transcript
//! readers parse archived artifacts, the diff renderer pairs recorded hunk
//! ranges with blobs under the serve policy, the lifecycle card reads
//! verification witnesses, and the detail assembler only composes their
//! segments. This file holds the worker that owns the caches.

mod catalogue;
mod detail;
mod diff;
mod lifecycle;
mod seal;
mod text;
mod transcript;

#[cfg(test)]
mod fixtures;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_canopy::{
    Event, ViewerCheckpointRow, ViewerDetail, ViewerErrorArea, ViewerEvent, ViewerFilterProgress,
    ViewerGeneration, ViewerRequest,
};
use rift_core::RiftError;
use rift_recall_substrate::{CheckpointCatalogueEntry, GixSubstrate};
use rift_store::{CanonicalStore, SqliteStore, Value};

use self::catalogue::{load_catalogue, resolve_selection};
use self::detail::build_detail;
use self::text::bounded;
use self::transcript::{transcript_excerpt, transcript_search_text};

use crate::store::runtime;

const FILTER_BATCH: usize = 64;
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
    use super::fixtures::{entry, initialized_repo, rows_for};
    use super::*;

    /// A worker over a repository with no commits, holding one cached
    /// checkpoint whose anchor is unknown, and a filter job mid-scan. The
    /// directories are owned here so the project path outlives the worker.
    struct OneCheckpointWorker {
        worker: ViewerWorker,
        project: String,
        checkpoint: CheckpointCatalogueEntry,
        _repo: tempfile::TempDir,
        _root: tempfile::TempDir,
    }

    fn worker_with_one_checkpoint() -> OneCheckpointWorker {
        let repo = initialized_repo();
        let project = repo.path().to_string_lossy().into_owned();
        let mut checkpoint = entry(&"7".repeat(64), &"a".repeat(40));
        checkpoint.anchor_commit = None;
        let rows = rows_for(std::slice::from_ref(&checkpoint));
        let root = tempfile::tempdir().unwrap();
        let mut worker = ViewerWorker::new(root.path().to_path_buf());
        worker.catalogues.insert(
            project.clone(),
            CachedCatalogue {
                entries: vec![checkpoint.clone()],
                rows,
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
        OneCheckpointWorker {
            worker,
            project,
            checkpoint,
            _repo: repo,
            _root: root,
        }
    }

    /// A worker over an empty repository whose catalogue holds a thousand
    /// checkpoints, so a filter needs many bounded batches.
    fn worker_with_thousand_checkpoints() -> (ViewerWorker, String, [tempfile::TempDir; 2]) {
        let repo = initialized_repo();
        let project = repo.path().to_string_lossy().into_owned();
        let entries = (0..1_000)
            .map(|index| entry(&format!("{index:064x}"), &"a".repeat(40)))
            .collect::<Vec<_>>();
        let rows = rows_for(&entries);
        let cache = tempfile::tempdir().unwrap();
        let mut worker = ViewerWorker::new(cache.path().to_path_buf());
        worker
            .catalogues
            .insert(project.clone(), CachedCatalogue { entries, rows });
        (worker, project, [repo, cache])
    }

    #[test]
    fn every_request_against_a_missing_repository_is_one_bounded_error() {
        let ids = [format!("{}1", "0".repeat(63))];
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
    }

    #[test]
    fn an_empty_catalogue_completes_every_filter_and_refuses_detail() {
        let ids = [format!("{}1", "0".repeat(63))];
        let cache = tempfile::tempdir().unwrap();
        let missing = cache
            .path()
            .join("missing-repository")
            .to_string_lossy()
            .into_owned();
        let mut worker = ViewerWorker::new(cache.path().to_path_buf());
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
    fn a_cached_catalogue_selects_the_target_and_restarts_the_live_filter() {
        let mut fixture = worker_with_one_checkpoint();
        let (project, checkpoint) = (fixture.project.clone(), fixture.checkpoint.clone());
        let worker = &mut fixture.worker;
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
    }

    #[test]
    fn visible_excerpts_enrich_the_requested_rows_without_a_store_write() {
        let mut fixture = worker_with_one_checkpoint();
        let (project, checkpoint) = (fixture.project.clone(), fixture.checkpoint.clone());
        let worker = &mut fixture.worker;
        let excerpts = worker.handle(ViewerRequest::VisibleExcerpts {
            project,
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
    }

    #[test]
    fn a_filter_over_one_checkpoint_completes_in_one_batch() {
        let mut fixture = worker_with_one_checkpoint();
        let project = fixture.project.clone();
        let worker = &mut fixture.worker;
        worker.filter_jobs.remove(&project);
        let scan = worker.handle(ViewerRequest::FilterScan {
            project,
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
    }

    #[test]
    fn a_detail_is_built_once_and_then_served_from_the_cache() {
        let mut fixture = worker_with_one_checkpoint();
        let (project, checkpoint) = (fixture.project.clone(), fixture.checkpoint.clone());
        let worker = &mut fixture.worker;
        let detail = worker.handle(ViewerRequest::Detail {
            project: project.clone(),
            generation: 5,
            checkpoint_id: checkpoint.checkpoint_id.clone(),
        });
        assert!(matches!(
            detail.as_slice(),
            [Event::Viewer(ViewerEvent::Detail { complete: true, .. })]
        ));
        assert_eq!(worker.details.len(), 1);
        let cached = worker.handle(ViewerRequest::Detail {
            project,
            generation: 6,
            checkpoint_id: checkpoint.checkpoint_id,
        });
        assert!(matches!(
            cached.as_slice(),
            [Event::Viewer(ViewerEvent::Detail { complete: true, .. })]
        ));
        assert_eq!(worker.details.len(), 1);
    }

    #[test]
    fn the_first_filter_batch_over_a_thousand_checkpoints_is_bounded() {
        let (mut worker, project, _dirs) = worker_with_thousand_checkpoints();
        let first = worker.handle(ViewerRequest::FilterScan {
            project,
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
    }

    #[test]
    fn a_replacement_query_cancels_the_old_generation_and_finishes_in_bounded_batches() {
        let (mut worker, project, _dirs) = worker_with_thousand_checkpoints();
        let _first = worker.handle(ViewerRequest::FilterScan {
            project: project.clone(),
            generation: 1,
            query: "old query".to_owned(),
        });
        let replacement = worker.handle(ViewerRequest::FilterScan {
            project,
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
            let Some(Event::Viewer(ViewerEvent::FilterProgress {
                generation,
                progress,
                ..
            })) = events.first()
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
}
