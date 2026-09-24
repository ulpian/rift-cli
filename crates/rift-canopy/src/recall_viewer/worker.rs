//! The request and response vocabulary of the read-only viewer worker.

use super::model::{ViewerCheckpointRow, ViewerDetail, ViewerFilterProgress};
use super::ViewerGeneration;

/// Requests that only the read-only viewer worker may execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerRequest {
    /// Load metadata-first catalogue and resolve an optional deep link.
    Catalogue {
        /// Project/repository path.
        project: String,
        /// Generation for stale-response rejection.
        generation: ViewerGeneration,
        /// Optional checkpoint/Git target.
        target: Option<String>,
        /// True for refresh; worker may compare/cache identities.
        refresh: bool,
    },
    /// Enrich visible rows with transcript excerpts.
    VisibleExcerpts {
        /// Project/repository path.
        project: String,
        /// Catalogue generation.
        generation: ViewerGeneration,
        /// Checkpoint ids in/near the viewport.
        checkpoint_ids: Vec<String>,
    },
    /// Load one selected checkpoint's timeline detail.
    Detail {
        /// Project/repository path.
        project: String,
        /// Detail generation.
        generation: ViewerGeneration,
        /// Checkpoint id.
        checkpoint_id: String,
    },
    /// Progressively search compact transcript text.
    FilterScan {
        /// Project/repository path.
        project: String,
        /// Filter generation (changing text cancels older scans).
        generation: ViewerGeneration,
        /// Case-insensitive query.
        query: String,
    },
}

/// Worker responses folded by the pure reducer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerEvent {
    /// Catalogue metadata or visible-row enrichment.
    Catalogue {
        /// Project/repository path.
        project: String,
        /// Matching catalogue generation.
        generation: ViewerGeneration,
        /// Newest-first rows.
        rows: Vec<ViewerCheckpointRow>,
        /// Deep-link selection resolved off-thread.
        selected: Option<String>,
        /// Precise non-blocking target/read notice.
        notice: Option<String>,
        /// `false` while visible excerpt enrichment remains.
        complete: bool,
    },
    /// Selected-checkpoint detail.
    Detail {
        /// Project/repository path.
        project: String,
        /// Matching detail generation.
        generation: ViewerGeneration,
        /// Immutable detail.
        detail: ViewerDetail,
        /// `false` for a partial independently-valid snapshot.
        complete: bool,
    },
    /// One progressive transcript-filter scan batch.
    FilterProgress {
        /// Project/repository path.
        project: String,
        /// Matching filter generation.
        generation: ViewerGeneration,
        /// Newly matching checkpoint ids.
        matching_ids: Vec<String>,
        /// Progress counters.
        progress: ViewerFilterProgress,
    },
    /// Safe worker error for one request class.
    Error {
        /// Project/repository path.
        project: String,
        /// Request generation.
        generation: ViewerGeneration,
        /// Area affected.
        area: ViewerErrorArea,
        /// Safe error category/detail.
        message: String,
    },
}

/// Viewer request class for error routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerErrorArea {
    /// Catalogue/refresh.
    Catalogue,
    /// Selected detail.
    Detail,
    /// Progressive filter scan.
    Filter,
}
