//! The checkpoint catalogue pane: filter chip, scan status and the five-line rows.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Style};
use crate::state::ViewModel;

use super::draw::{put_clipped, short_id, warning_style};
use super::geometry::CHECKPOINT_ROW_HEIGHT;
use super::model::{ViewerCheckpointRow, ViewerFilterProgress};
use super::{ViewerLoadState, ViewerPane};

/// A conditional background, so a row's dim detail line keeps the selection
/// band only when that row is the selected one.
trait StyleExt {
    fn with_bg_if(self, condition: bool, color: Color) -> Self;
}

impl StyleExt for Style {
    fn with_bg_if(self, condition: bool, color: Color) -> Self {
        if condition {
            self.with_bg(color)
        } else {
            self
        }
    }
}

/// Draw the catalogue pane: its frame, the header rows, then either the
/// visible rows or the reason there are none.
pub(super) fn draw_checkpoints(vm: &ViewModel, frame: &mut Frame, rect: Rect) {
    let focused = vm.viewer.pane == ViewerPane::Checkpoints;
    let title = format!("checkpoints · {}", vm.viewer.visible.len());
    let inner = crate::chrome::panel(
        frame,
        rect,
        &title,
        vm.theme.border(crate::theme::Section::Repository, focused),
        vm.theme.title(crate::theme::Section::Repository),
    );
    if inner.h == 0 {
        return;
    }
    let header_rows = draw_catalogue_header(vm, frame, inner);
    let list = Rect {
        x: inner.x,
        y: inner.y.saturating_add(header_rows),
        w: inner.w,
        h: inner.h.saturating_sub(header_rows),
    };
    if vm.viewer.visible.is_empty() {
        draw_empty_catalogue(vm, frame, list);
        return;
    }
    draw_checkpoint_rows(vm, frame, list);
}

/// Draw the filter chip and, below it, at most one status row: the transcript
/// scan's progress while a filter scan exists, else the `N new` notice a
/// refresh left. Returns the rows used, so the list starts below them.
fn draw_catalogue_header(vm: &ViewModel, frame: &mut Frame, inner: Rect) -> u16 {
    let filter = if vm.viewer.filter_editing {
        format!("[ Filter: {}▏ ]", vm.viewer.filter)
    } else if vm.viewer.filter.is_empty() {
        "[ Filter ]".to_owned()
    } else {
        format!("[ Filter: {} ]", vm.viewer.filter)
    };
    put_clipped(
        frame,
        inner.x,
        inner.y,
        inner.w,
        &filter,
        if vm.viewer.filter_editing {
            vm.theme.selection()
        } else {
            vm.theme.button(false)
        },
    );
    let status = match &vm.viewer.filter_progress {
        Some(progress) => Some((scan_status(progress), vm.theme.dim())),
        None if vm.viewer.new_count > 0 => Some((
            format!("{} new · [ Latest ]", vm.viewer.new_count),
            vm.theme.key_hint(),
        )),
        None => None,
    };
    let Some((text, style)) = status else {
        return 1;
    };
    if inner.h <= 1 {
        return 1;
    }
    put_clipped(frame, inner.x, inner.y + 1, inner.w, &text, style);
    2
}

/// The transcript scan's progress line.
fn scan_status(progress: &ViewerFilterProgress) -> String {
    if progress.complete {
        format!("search complete · {}/{}", progress.scanned, progress.total)
    } else {
        format!(
            "searching transcripts · {}/{}",
            progress.scanned, progress.total
        )
    }
}

/// Say why the list is empty — still loading, unreadable, nothing captured, or
/// nothing matching the filter — and, for an unreadable catalogue, the safe
/// error detail on the line below.
fn draw_empty_catalogue(vm: &ViewModel, frame: &mut Frame, list: Rect) {
    let text = match &vm.viewer.catalogue_state {
        ViewerLoadState::Loading => "loading checkpoint catalogue …",
        ViewerLoadState::Error(_) => "catalogue unavailable",
        _ if vm.viewer.filter.is_empty() => "no durable checkpoints",
        _ => "no checkpoint matches yet",
    };
    put_clipped(frame, list.x, list.y, list.w, text, vm.theme.dim());
    if let ViewerLoadState::Error(error) = &vm.viewer.catalogue_state {
        if list.h > 1 {
            put_clipped(frame, list.x, list.y + 1, list.w, error, warning_style(vm));
        }
    }
}

/// Draw the visible rows that fit below the header, starting at the
/// catalogue's scroll offset; the last row is clipped to the list's bottom.
fn draw_checkpoint_rows(vm: &ViewModel, frame: &mut Frame, list: Rect) {
    let capacity = usize::from(list.h / CHECKPOINT_ROW_HEIGHT).max(1);
    let selected = vm
        .viewer
        .selected
        .min(vm.viewer.visible.len().saturating_sub(1));
    let start = vm
        .viewer
        .checkpoint_scroll
        .min(vm.viewer.visible.len().saturating_sub(capacity));
    let end = (start + capacity).min(vm.viewer.visible.len());
    for visible_index in start..end {
        let Some(catalogue_index) = vm.viewer.visible.get(visible_index) else {
            continue;
        };
        let Some(row) = vm.viewer.catalogue.get(*catalogue_index) else {
            continue;
        };
        let Ok(offset) = u16::try_from(visible_index - start) else {
            break;
        };
        let y = list.y + offset.saturating_mul(CHECKPOINT_ROW_HEIGHT);
        draw_checkpoint_row(
            vm,
            frame,
            Rect {
                x: list.x,
                y,
                w: list.w,
                h: CHECKPOINT_ROW_HEIGHT.min(list.y.saturating_add(list.h).saturating_sub(y)),
            },
            row,
            visible_index == selected,
        );
    }
}

/// Draw one five-line row: date and its provenance, excerpt, ids, storage
/// origin and chain position, containing branch and badges. A row clipped
/// short keeps its top lines, so the date is the last thing it loses.
fn draw_checkpoint_row(
    vm: &ViewModel,
    frame: &mut Frame,
    rect: Rect,
    row: &ViewerCheckpointRow,
    selected: bool,
) {
    let style = if selected {
        vm.theme.selection()
    } else {
        vm.theme.body()
    };
    if selected {
        crate::chrome::fill(frame, rect, style);
    }
    let marker = if selected { '>' } else { ' ' };
    let date = row.compact_utc.as_deref().unwrap_or("date unknown");
    put_clipped(
        frame,
        rect.x,
        rect.y,
        rect.w,
        &format!("{marker} {date} · {}", row.timestamp_provenance.label()),
        style,
    );
    let excerpt = row.excerpt.as_deref().unwrap_or(if row.excerpt_loading {
        "… loading excerpt"
    } else {
        "(no user/assistant text)"
    });
    let anchor = row.anchor_commit.as_deref().map_or("—", short_id);
    let details = [
        (excerpt.to_owned(), style),
        (
            format!("cp {} · commit {anchor}", short_id(&row.checkpoint_id)),
            vm.theme.dim().with_bg_if(selected, crate::theme::SELECT_BG),
        ),
        (
            format!("{} · {}", origin_summary(row), chain_summary(row)),
            vm.theme.dim(),
        ),
        (
            format!("{} · {}", branch_summary(row), checkpoint_badges(row)),
            if row.issues.is_empty() {
                vm.theme.dim()
            } else {
                warning_style(vm)
            },
        ),
    ];
    for (dy, (text, detail_style)) in (1..rect.h).zip(details) {
        put_clipped(
            frame,
            rect.x + 2,
            rect.y + dy,
            rect.w.saturating_sub(2),
            &text,
            detail_style,
        );
    }
}

/// A catalogue row's badge strip: capture strategy, projection posture, the
/// parse-issue count when the row has any, and the worktree the shard was
/// **archived in** (worktree provenance).
///
/// `wt:unknown` is a fact, not a placeholder (worktree provenance): an absent
/// `checkpoint_worktree` row means the shard carries no `worktree` member, as
/// every shard written before provenance labelling does, and sealed shards are
/// never rewritten, so one can never be back-filled. Printing `main` there — or
/// nothing — would turn "we do
/// not know" into a claim about where the archive ran.
///
/// The worktree goes **last** because this strip is clipped to the pane's
/// width and the row that loses its tail should lose provenance before it
/// loses a warning: `!parse:n` says the row is corrupt, and a badge must
/// never be what pushes that off the screen.
fn checkpoint_badges(row: &ViewerCheckpointRow) -> String {
    let strategy = row.strategy.as_deref().unwrap_or("strategy ?");
    let worktree = row.worktree.as_deref().unwrap_or("unknown");
    let mut strip = format!("{strategy} · {}", row.projection);
    if !row.issues.is_empty() {
        strip.push_str(&format!(" · !parse:{}", row.issues.len()));
    }
    strip.push_str(&format!(" · wt:{worktree}"));
    strip
}

/// The first storage origin as `dialect/br|ref storage-ref`, with a `+n` for
/// the origins a row this narrow cannot list.
fn origin_summary(row: &ViewerCheckpointRow) -> String {
    let Some(origin) = row.origins.first() else {
        return "origin unavailable".to_owned();
    };
    let kind = if origin.kind == "branch" { "br" } else { "ref" };
    let more = more_suffix(row.origins.len().saturating_sub(1));
    format!("{}/{kind} {}{more}", origin.dialect, origin.storage_ref)
}

/// The canonical-chain position, or `unchained` for a shard outside it.
fn chain_summary(row: &ViewerCheckpointRow) -> String {
    row.chain_position
        .map_or("unchained".to_owned(), |position| {
            format!("chain #{position}")
        })
}

/// The first local branch containing the anchor, with a `+n` for the rest.
fn branch_summary(row: &ViewerCheckpointRow) -> String {
    let Some(name) = row.local_branches.first() else {
        return "no containing branch".to_owned();
    };
    let more = more_suffix(row.local_branches.len().saturating_sub(1));
    format!("{name}{more}")
}

/// ` +n` for `n` further entries, nothing for none.
fn more_suffix(more: usize) -> String {
    if more > 0 {
        format!(" +{more}")
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall_viewer::fixtures::seeded_row;

    /// ★ worktree provenance, the catalogue's half: a checkpoint with no
    /// `checkpoint_worktree` row badges `unknown` — never `main`, never a
    /// blank — and a checkpoint that has one badges that member by name. This
    /// is the first reader of the relation.
    #[test]
    fn a_checkpoint_without_a_worktree_row_badges_unknown_never_main() {
        let mut r = seeded_row("c1", "");
        r.strategy = Some("native".to_owned());
        r.projection = "projected".into();

        assert_eq!(
            checkpoint_badges(&r),
            "native · projected · wt:unknown",
            "no projected row ⇒ unknown, spelled out"
        );
        assert!(
            checkpoint_badges(&r).ends_with("wt:unknown"),
            "provenance trails the strip, so a clip sheds it first"
        );
        assert!(
            !checkpoint_badges(&r).contains("main"),
            "absence never borrows the main work tree's name"
        );

        r.worktree = Some("swt-d".to_owned());
        assert_eq!(checkpoint_badges(&r), "native · projected · wt:swt-d");
        // The main work tree is a member like any other: it is named only
        // because the shard said so.
        r.worktree = Some("main".to_owned());
        assert_eq!(checkpoint_badges(&r), "native · projected · wt:main");

        // The parse-issue count still trails the strip, so a corrupt row
        // keeps its warning AND its provenance.
        r.worktree = None;
        r.issues = vec!["metadata: bad".to_owned()];
        assert_eq!(
            checkpoint_badges(&r),
            "native · projected · !parse:1 · wt:unknown",
            "the corruption warning outranks the badge when the row is clipped"
        );
    }
}
