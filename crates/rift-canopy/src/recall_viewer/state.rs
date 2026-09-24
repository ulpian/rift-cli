//! The viewer's session-local navigation, filter and expansion state.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::model::{ViewerCheckpointRow, ViewerDetail, ViewerFilterProgress};
use super::{ViewerGeneration, ViewerLoadState, ViewerPane};

/// Session-local viewer state. No field is persisted.
#[derive(Clone, Debug)]
pub struct RecallViewerState {
    /// Dashboard focus restored when the viewer closes.
    pub dashboard_focus: crate::Panel,
    /// Focused pane.
    pub pane: ViewerPane,
    /// Content pane restored when command editing closes.
    pub pane_before_command: ViewerPane,
    /// Active repository/project key.
    pub project: Option<String>,
    /// Current catalogue request generation.
    pub catalogue_generation: ViewerGeneration,
    /// Current detail request generation.
    pub detail_generation: ViewerGeneration,
    /// Checkpoint id the current detail generation targets.
    pub detail_target: Option<String>,
    /// Current filter request generation.
    pub filter_generation: ViewerGeneration,
    /// Catalogue loading posture.
    pub catalogue_state: ViewerLoadState,
    /// Detail loading posture.
    pub detail_state: ViewerLoadState,
    /// Full newest-first catalogue.
    pub catalogue: Arc<Vec<ViewerCheckpointRow>>,
    /// Visible indices into [`Self::catalogue`], kept in catalogue order.
    pub visible: Vec<usize>,
    /// Selected visible row index.
    pub selected: usize,
    /// Catalogue viewport top, in rows (not terminal lines).
    pub checkpoint_scroll: usize,
    /// Timeline viewport top, in flattened terminal lines.
    pub timeline_scroll: usize,
    /// Timeline keyboard cursor, in flattened terminal lines.
    pub timeline_cursor: usize,
    /// Selected detail snapshot.
    pub detail: Arc<ViewerDetail>,
    /// Filter text.
    pub filter: String,
    /// `f` placed the filter chip into editing mode.
    pub filter_editing: bool,
    /// Progressive transcript scan status.
    pub filter_progress: Option<ViewerFilterProgress>,
    /// Transcript matches delivered progressively by the worker.
    pub(super) transcript_matches: BTreeSet<String>,
    /// Checkpoint selected when this filter-editing session began. It remains
    /// provisionally visible while unloaded transcript excerpts are searched,
    /// then either stays selected on a match or yields to the newest match.
    pub(super) filter_selection_anchor: Option<String>,
    /// Show the whole self-contained compact transcript.
    pub full_session: bool,
    /// Expanded task/shadow/hunk item ids.
    pub expanded: BTreeSet<String>,
    /// New rows seen on refresh without moving the reader.
    pub new_count: usize,
    /// Non-blocking target/refresh/read notice.
    pub notice: Option<String>,
}

impl Default for RecallViewerState {
    fn default() -> Self {
        Self {
            dashboard_focus: crate::Panel::Projects,
            pane: ViewerPane::Projects,
            pane_before_command: ViewerPane::Checkpoints,
            project: None,
            catalogue_generation: 0,
            detail_generation: 0,
            detail_target: None,
            filter_generation: 0,
            catalogue_state: ViewerLoadState::Idle,
            detail_state: ViewerLoadState::Idle,
            catalogue: Arc::new(Vec::new()),
            visible: Vec::new(),
            selected: 0,
            checkpoint_scroll: 0,
            timeline_scroll: 0,
            timeline_cursor: 0,
            detail: Arc::new(ViewerDetail::default()),
            filter: String::new(),
            filter_editing: false,
            filter_progress: None,
            transcript_matches: BTreeSet::new(),
            filter_selection_anchor: None,
            full_session: false,
            expanded: BTreeSet::new(),
            new_count: 0,
            notice: None,
        }
    }
}

impl RecallViewerState {
    /// Selected row in the filtered catalogue.
    #[must_use]
    pub fn selected_row(&self) -> Option<&ViewerCheckpointRow> {
        self.visible
            .get(self.selected)
            .and_then(|index| self.catalogue.get(*index))
    }

    /// Start a new project-scoped catalogue request and clear only data that
    /// cannot belong to the new project.
    pub fn start_project(&mut self, project: String) -> ViewerGeneration {
        self.project = Some(project);
        self.catalogue_generation = self.catalogue_generation.wrapping_add(1).max(1);
        self.detail_generation = self.detail_generation.wrapping_add(1).max(1);
        self.detail_target = None;
        self.filter_generation = self.filter_generation.wrapping_add(1).max(1);
        self.catalogue_state = ViewerLoadState::Loading;
        self.detail_state = ViewerLoadState::Idle;
        self.catalogue = Arc::new(Vec::new());
        self.visible.clear();
        self.selected = 0;
        self.checkpoint_scroll = 0;
        self.timeline_scroll = 0;
        self.timeline_cursor = 0;
        self.detail = Arc::new(ViewerDetail::default());
        self.filter.clear();
        self.filter_editing = false;
        self.filter_progress = None;
        self.transcript_matches.clear();
        self.filter_selection_anchor = None;
        self.full_session = false;
        self.expanded.clear();
        self.new_count = 0;
        self.notice = None;
        self.catalogue_generation
    }

    /// Replace catalogue metadata for the current generation. Selection is
    /// kept by checkpoint id; a refresh that added newer rows records `N new`
    /// but never moves a reader away from their checkpoint.
    pub fn apply_catalogue(
        &mut self,
        rows: Vec<ViewerCheckpointRow>,
        selected_hint: Option<&str>,
        notice: Option<String>,
        complete: bool,
    ) {
        let previous_id = self.selected_row().map(|row| row.checkpoint_id.clone());
        let previous_ids: BTreeSet<&str> = self
            .catalogue
            .iter()
            .map(|row| row.checkpoint_id.as_str())
            .collect();
        let newly_discovered = if previous_ids.is_empty() {
            0
        } else {
            rows.iter()
                .filter(|row| !previous_ids.contains(row.checkpoint_id.as_str()))
                .count()
        };
        self.new_count = self.new_count.saturating_add(newly_discovered);
        self.catalogue = Arc::new(rows);
        self.recompute_visible();
        let desired = previous_id.as_deref().or(selected_hint);
        self.selected = desired
            .and_then(|id| {
                self.visible.iter().position(|index| {
                    self.catalogue
                        .get(*index)
                        .is_some_and(|row| row.checkpoint_id == id)
                })
            })
            .unwrap_or(0)
            .min(self.visible.len().saturating_sub(1));
        self.catalogue_state = if complete {
            ViewerLoadState::Ready
        } else {
            ViewerLoadState::Partial
        };
        if notice.is_some() {
            self.notice = notice;
        }
    }

    /// Move selection and return whether it changed.
    pub fn move_checkpoint(&mut self, down: bool, page: bool) -> bool {
        if self.visible.is_empty() {
            return false;
        }
        let old = self.selected;
        let step = if page { 8 } else { 1 };
        self.selected = if down {
            self.selected
                .saturating_add(step)
                .min(self.visible.len().saturating_sub(1))
        } else {
            self.selected.saturating_sub(step)
        };
        old != self.selected
    }

    /// Jump to first/last visible checkpoint.
    pub fn checkpoint_edge(&mut self, last: bool) -> bool {
        let old = self.selected;
        self.selected = if last {
            self.visible.len().saturating_sub(1)
        } else {
            0
        };
        old != self.selected
    }

    /// Select the latest visible checkpoint and clear the refresh notice.
    pub fn select_latest(&mut self) -> bool {
        let changed = self.selected != 0 && !self.visible.is_empty();
        self.selected = 0;
        self.checkpoint_scroll = 0;
        self.new_count = 0;
        changed
    }

    /// Recompute immediate metadata matches. Transcript matches are unioned as
    /// progressive worker responses arrive.
    pub fn recompute_visible(&mut self) {
        let query = self.filter.to_lowercase();
        let scanning = self
            .filter_progress
            .as_ref()
            .is_some_and(|progress| !progress.complete);
        self.visible = self
            .catalogue
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                query.is_empty()
                    || row.metadata_search.contains(&query)
                    || self.transcript_matches.contains(&row.checkpoint_id)
                    || scanning
                        && self.filter_selection_anchor.as_deref()
                            == Some(row.checkpoint_id.as_str())
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }

    /// Enter filter editing and remember the reader's checkpoint before any
    /// immediate metadata filtering can move the visible cursor.
    pub fn begin_filter_editing(&mut self) {
        self.filter_selection_anchor = self.selected_row().map(|row| row.checkpoint_id.clone());
        self.filter_editing = true;
    }

    /// Begin/cancel a progressive transcript scan after filter text changes.
    pub fn changed_filter(&mut self) -> ViewerGeneration {
        let selected_id = self.selected_row().map(|row| row.checkpoint_id.clone());
        if self.filter_selection_anchor.is_none() {
            self.filter_selection_anchor.clone_from(&selected_id);
        }
        self.filter_generation = self.filter_generation.wrapping_add(1).max(1);
        self.transcript_matches.clear();
        self.filter_progress = (!self.filter.is_empty()).then_some(ViewerFilterProgress {
            scanned: 0,
            total: self.catalogue.len(),
            complete: false,
        });
        self.recompute_visible();
        self.selected = selected_id
            .as_deref()
            .and_then(|id| {
                self.visible.iter().position(|index| {
                    self.catalogue
                        .get(*index)
                        .is_some_and(|row| row.checkpoint_id == id)
                })
            })
            .unwrap_or(0);
        self.filter_generation
    }

    /// Merge one progressive filter-scan response.
    pub fn apply_filter_progress(
        &mut self,
        matching_ids: impl IntoIterator<Item = String>,
        progress: ViewerFilterProgress,
    ) {
        let selected_id = self.selected_row().map(|row| row.checkpoint_id.clone());
        self.transcript_matches.extend(matching_ids);
        self.filter_progress = Some(progress);
        self.recompute_visible();
        self.selected = selected_id
            .as_deref()
            .and_then(|id| {
                self.visible.iter().position(|index| {
                    self.catalogue
                        .get(*index)
                        .is_some_and(|row| row.checkpoint_id == id)
                })
            })
            .unwrap_or(0)
            .min(self.visible.len().saturating_sub(1));
    }

    /// Start a selected-detail request and return its generation.
    pub fn start_detail(&mut self, checkpoint_id: String) -> ViewerGeneration {
        self.detail_generation = self.detail_generation.wrapping_add(1).max(1);
        self.detail_target = Some(checkpoint_id);
        self.detail_state = ViewerLoadState::Loading;
        self.timeline_scroll = 0;
        self.timeline_cursor = 0;
        self.detail_generation
    }

    /// Apply selected detail for the current generation.
    pub fn apply_detail(&mut self, detail: ViewerDetail, complete: bool) {
        self.detail_target = Some(detail.checkpoint_id.clone());
        self.detail = Arc::new(detail);
        self.detail_state = if complete {
            ViewerLoadState::Ready
        } else {
            ViewerLoadState::Partial
        };
        self.timeline_scroll = 0;
        self.timeline_cursor = 0;
        self.expanded.clear();
        self.full_session = false;
    }

    /// Toggle one timeline item's expansion state.
    pub fn toggle_expanded(&mut self, id: &str) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall_viewer::fixtures::seeded_row;

    #[test]
    fn filter_preserves_selection_when_it_still_matches() {
        let mut viewer = RecallViewerState::default();
        viewer.apply_catalogue(
            vec![seeded_row("new", "alpha"), seeded_row("old", "beta")],
            Some("old"),
            None,
            true,
        );
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("old")
        );
        viewer.begin_filter_editing();
        viewer.filter = "beta".into();
        viewer.changed_filter();
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("old")
        );
        viewer.filter = "alpha".into();
        viewer.changed_filter();
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("old"),
            "the reader stays put while its unloaded transcript is scanned"
        );
        viewer.apply_filter_progress(
            ["new".to_owned()],
            ViewerFilterProgress {
                scanned: 2,
                total: 2,
                complete: true,
            },
        );
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("new")
        );
    }

    #[test]
    fn filter_restores_the_anchor_when_its_transcript_matches() {
        let mut viewer = RecallViewerState::default();
        viewer.apply_catalogue(
            vec![seeded_row("new", "alpha"), seeded_row("old", "beta")],
            Some("old"),
            None,
            true,
        );
        viewer.begin_filter_editing();
        viewer.filter = "needle only in transcript".into();
        viewer.changed_filter();
        viewer.apply_filter_progress(
            ["old".to_owned()],
            ViewerFilterProgress {
                scanned: 2,
                total: 2,
                complete: true,
            },
        );
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("old")
        );
    }

    #[test]
    fn refresh_preserves_reader_and_counts_new_prefix() {
        let mut viewer = RecallViewerState::default();
        viewer.apply_catalogue(
            vec![seeded_row("b", ""), seeded_row("a", "")],
            Some("a"),
            None,
            true,
        );
        viewer.apply_catalogue(
            vec![
                seeded_row("d", ""),
                seeded_row("c", ""),
                seeded_row("b", ""),
                seeded_row("a", ""),
            ],
            None,
            None,
            true,
        );
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("a")
        );
        assert_eq!(viewer.new_count, 2);
        viewer.select_latest();
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("d")
        );
    }
}
