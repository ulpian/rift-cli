//! The repository card: name, kind, path and status, with its two labelled
//! actions (Recall Viewer, Project Settings) and the adopt act on the status row.
//!
//! The chip geometry lives here beside the drawing because the hit-test reads
//! it too: `repository_button_layout` is the one solver both call, so the
//! cells that look clickable are the cells that answer a click.

use crate::col2::{sanitize_for_terminal, DetailView};
use crate::hit::Target;
use crate::layout::Rect;
use crate::render::frame::Frame;
use crate::theme::{Icon, Section, Theme};
use crate::{EntryKind, ProjectStatus};

use super::{card_frame, line, CardCtx};

/// Repository-card Project Settings button text in each collapse state.
pub const REPOSITORY_SETTINGS: &str = "[ Project Settings ]";
/// Compact button used by a collapsed repository card.
pub const REPOSITORY_SETTINGS_COLLAPSED: &str = "[settings]";
/// Repository-card entry point for the read-only Recall Viewer.
pub const REPOSITORY_RECALL_VIEWER: &str = "[ Recall Viewer ]";
/// Compact Recall Viewer chip.
pub const REPOSITORY_RECALL_COLLAPSED: &str = "[recall]";

/// One solved repository-card button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryButton {
    /// Exact clickable rectangle.
    pub rect: Rect,
    /// Exact rendered text.
    pub text: &'static str,
}

/// Both repository-card actions, solved together so they cannot overlap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryButtonLayout {
    /// Recall Viewer, left/first.
    pub recall: RepositoryButton,
    /// Project Settings, right/second.
    pub settings: RepositoryButton,
}

/// The Repository card's **adopt act** (the family view): the status row,
/// clickable exactly when the card is drawing that row for an unadopted
/// member.
///
/// ONE solver feeds the draw and the hit-test, so the cells the operator sees
/// and the cells that answer a click are the same cells by construction — the
/// blind spot this closes, and the reason this is a function rather than a rule
/// spelled in two files. `None` means there is no act here: the card is
/// collapsed (its single row belongs to the two labelled actions), the
/// geometry pushed the buttons onto the status row, or the member is not
/// `NotAdopted` and has nothing to adopt.
#[must_use]
pub fn repository_adopt_rect(rect: Rect, collapsed: bool, status: ProjectStatus) -> Option<Rect> {
    if collapsed || status != ProjectStatus::NotAdopted {
        return None;
    }
    if rect.w < 2 || rect.h < 2 {
        return None;
    }
    let inner = card_inner(rect);
    // The status row is drawn only when the two actions share one row; when
    // they stack they OWN the two bottom rows and there is no status line to
    // click. Read off the same layout the renderer uses, never re-derived.
    let buttons = repository_button_layout(rect, false);
    if buttons.recall.rect.y != buttons.settings.rect.y || inner.h < 3 {
        return None;
    }
    Some(Rect {
        x: inner.x,
        y: inner.y + 2,
        w: inner.w,
        h: 1,
    })
}

/// Solve both Repository actions. Full-width cards place them side by side;
/// narrow cards stack them on the two bottom inner rows. Collapsed cards use
/// compact labels and, at the absolute floor, one-letter chips rather than
/// overlapping.
#[must_use]
pub fn repository_button_layout(rect: Rect, collapsed: bool) -> RepositoryButtonLayout {
    let inner = card_inner(rect);
    let (recall_text, settings_text) = button_labels(inner.w, collapsed);
    if inner.h == 0 || inner.w == 0 {
        return unplaced_buttons(inner, recall_text, settings_text);
    }
    let recall_w = label_width(recall_text);
    let settings_w = label_width(settings_text);
    if recall_w.saturating_add(settings_w).saturating_add(1) <= inner.w {
        let y = if collapsed {
            inner.y
        } else {
            inner.y + inner.h - 1
        };
        return RepositoryButtonLayout {
            recall: chip(inner.x, y, recall_w, recall_text),
            settings: chip(inner.x + inner.w - settings_w, y, settings_w, settings_text),
        };
    }
    // A normal card has at least four inner rows. If a pathological custom
    // layout gives fewer, compact both actions onto the last row.
    if inner.h < 2 {
        return floor_buttons(inner);
    }
    RepositoryButtonLayout {
        recall: chip(
            inner.x,
            inner.y + inner.h - 2,
            recall_w.min(inner.w),
            recall_text,
        ),
        settings: chip(
            inner.x,
            inner.y + inner.h - 1,
            settings_w.min(inner.w),
            settings_text,
        ),
    }
}

/// Exact Project Settings chip rect, shared by draw and hit-test.
#[must_use]
pub fn repository_settings_rect(rect: Rect, collapsed: bool) -> Rect {
    repository_button_layout(rect, collapsed).settings.rect
}

/// Exact Recall Viewer chip rect, shared by draw and hit-test.
#[must_use]
pub fn repository_recall_rect(rect: Rect, collapsed: bool) -> Rect {
    repository_button_layout(rect, collapsed).recall.rect
}

pub(super) fn draw_repository(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    collapsed: bool,
    frame: &mut Frame,
) {
    let d = ctx.detail;
    let kind_word = match d.kind {
        EntryKind::Repo => "repo",
        EntryKind::Worktree => "worktree",
    };
    let inner = card_frame(theme, Section::Repository, "repository", rect, frame);
    if collapsed {
        draw_repository_headline(ctx, theme, rect, inner, frame);
        return;
    }
    let buttons = repository_button_layout(rect, false);
    line(
        frame,
        inner,
        0,
        &format!("{} ({kind_word})", sanitize_for_terminal(&d.label)),
        theme.body(),
    );
    line(
        frame,
        inner,
        1,
        &format!("path: {}", sanitize_for_terminal(&d.path)),
        theme.dim(),
    );
    // Status with its coloured dot. At a width that needs the buttons stacked,
    // the two bottom rows belong to the two labelled actions; path + heading
    // remain and no action clips.
    if buttons.recall.rect.y == buttons.settings.rect.y {
        draw_repository_status(ctx, theme, rect, inner, frame);
    }
    draw_repository_buttons(ctx, theme, &buttons, frame);
}

/// The collapsed card's single row: both actions, and the headline between
/// them.
fn draw_repository_headline(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    inner: Rect,
    frame: &mut Frame,
) {
    let d = ctx.detail;
    let buttons = repository_button_layout(rect, true);
    let summary = format!(
        "{} · {}",
        sanitize_for_terminal(&d.label),
        status_word(d.status)
    );
    // The collapsed card has a single row. Keep Recall anchored left and
    // Settings right, and put the repository headline in the honest gap
    // between them; drawing it from `inner.x` would be overwritten by the
    // left action and silently lose the selected-project state.
    let summary_x = buttons
        .recall
        .rect
        .x
        .saturating_add(buttons.recall.rect.w)
        .saturating_add(u16::from(buttons.recall.rect.w > 0));
    let summary_right = buttons
        .settings
        .rect
        .x
        .saturating_sub(u16::from(buttons.settings.rect.w > 0));
    let summary_w = usize::from(summary_right.saturating_sub(summary_x));
    let summary: String = summary.chars().take(summary_w).collect();
    draw_repository_buttons(ctx, theme, &buttons, frame);
    frame.put_str(summary_x, inner.y, &summary, theme.body());
}

/// The status row: its coloured dot, then the status line — in the key-hint
/// style when the row is the adopt act.
fn draw_repository_status(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    inner: Rect,
    frame: &mut Frame,
) {
    let d = ctx.detail;
    let dot = Icon::for_status(d.status).glyph(ctx.unicode);
    frame.set(inner.x, inner.y + 2, dot, theme.status_dot(d.status));
    // An unadopted member's status row IS the adopt act (the family view),
    // so it is drawn in the key-hint style: the cells that answer a click
    // are the cells that look clickable. `repository_adopt_rect` is the one
    // solver behind both, so the two cannot drift.
    let act = repository_adopt_rect(rect, false, d.status).is_some();
    line(
        frame,
        Rect {
            x: inner.x + 2,
            ..inner
        },
        2,
        &status_line(d),
        if act { theme.key_hint() } else { theme.body() },
    );
}

/// Both actions, Recall first, each answering to the target its click
/// resolves to so the press-flash lands on the chip that was pressed.
fn draw_repository_buttons(
    ctx: &CardCtx<'_>,
    theme: Theme,
    buttons: &RepositoryButtonLayout,
    frame: &mut Frame,
) {
    draw_repository_button(
        ctx,
        theme,
        &buttons.recall,
        Target::RepositoryRecallViewer,
        frame,
    );
    draw_repository_button(
        ctx,
        theme,
        &buttons.settings,
        Target::RepositorySettings,
        frame,
    );
}

fn draw_repository_button(
    ctx: &CardCtx<'_>,
    theme: Theme,
    button: &RepositoryButton,
    target: Target,
    frame: &mut Frame,
) {
    if button.rect.w == 0 {
        return;
    }
    let bg = crate::chrome::press_flash_bg(theme, ctx.flash(&target));
    let style = bg.map_or(theme.button(false), |bg| theme.button(false).with_bg(bg));
    let clipped: String = button
        .text
        .chars()
        .take(usize::from(button.rect.w))
        .collect();
    frame.put_str(button.rect.x, button.rect.y, &clipped, style);
}

/// The card's content rect inside its one-cell border — or the rect itself
/// when it is too small to carry a border, so the solver still answers.
fn card_inner(rect: Rect) -> Rect {
    if rect.w < 2 || rect.h < 2 {
        return rect;
    }
    Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        w: rect.w - 2,
        h: rect.h - 2,
    }
}

/// The widest label tier where both chips fit `inner_w`: the full labels
/// (never on a collapsed card), then the compact ones, then letters — a chip
/// shrinks rather than overlapping its neighbour.
fn button_labels(inner_w: u16, collapsed: bool) -> (&'static str, &'static str) {
    let fits = |(recall, settings): (&str, &str)| {
        label_width(recall) <= inner_w && label_width(settings) <= inner_w
    };
    let full = (REPOSITORY_RECALL_VIEWER, REPOSITORY_SETTINGS);
    let compact = (REPOSITORY_RECALL_COLLAPSED, REPOSITORY_SETTINGS_COLLAPSED);
    if !collapsed && fits(full) {
        return full;
    }
    if fits(compact) {
        return compact;
    }
    if inner_w >= 3 {
        ("[R]", "[S]")
    } else {
        ("R", "S")
    }
}

fn label_width(text: &str) -> u16 {
    u16::try_from(text.chars().count()).unwrap_or(0)
}

/// A one-row chip at `(x, y)`, `w` cells wide.
fn chip(x: u16, y: u16, w: u16, text: &'static str) -> RepositoryButton {
    RepositoryButton {
        rect: Rect { x, y, w, h: 1 },
        text,
    }
}

/// A card with no inner area: both labels kept, neither placed, so a click
/// can never land on a chip that was not drawn.
fn unplaced_buttons(
    inner: Rect,
    recall_text: &'static str,
    settings_text: &'static str,
) -> RepositoryButtonLayout {
    let empty = Rect {
        x: inner.x,
        y: inner.y,
        w: 0,
        h: 0,
    };
    RepositoryButtonLayout {
        recall: RepositoryButton {
            rect: empty,
            text: recall_text,
        },
        settings: RepositoryButton {
            rect: empty,
            text: settings_text,
        },
    }
}

/// The one-row floor: a letter chip at each end of the only row, and a single
/// Recall cell when the row is one cell wide.
fn floor_buttons(inner: Rect) -> RepositoryButtonLayout {
    let recall_width = if inner.w >= 2 { 1 } else { inner.w };
    let settings_width = u16::from(inner.w >= 2);
    RepositoryButtonLayout {
        recall: chip(inner.x, inner.y, recall_width, "R"),
        settings: chip(
            inner.x + inner.w.saturating_sub(settings_width),
            inner.y,
            settings_width,
            "S",
        ),
    }
}

/// The status row's text: the status word, and — for a family row whose
/// `Running` count is the sum over its members — which members it came from
/// (the family view), else the act the status asks for.
///
/// One row rather than two: the card's third line is the only one guaranteed
/// to be drawn at the widths where the two actions sit side by side, and a
/// count a reader cannot attribute is the thing the serve accounting seam forbids. Members are
/// already sanitized by the reader that names them (they are administrative
/// directory names), and the row is clipped by `line`.
fn status_line(d: &DetailView) -> String {
    let word = status_word(d.status);
    if !d.running_members.is_empty() {
        return format!("{word} · {}", d.running_members.join(", "));
    }
    // The hint already names the state it is a hint about, so it replaces the
    // word rather than following it: "error · checkout gone…" would say the
    // same thing twice in the row that has the least space for it.
    d.status
        .hint(d.kind)
        .map_or_else(|| word.to_owned(), str::to_owned)
}

/// A short human label for a project's live status.
fn status_word(status: ProjectStatus) -> &'static str {
    match status {
        ProjectStatus::Running { .. } => "running",
        ProjectStatus::PickingUp => "picking up",
        ProjectStatus::Idle => "idle",
        ProjectStatus::NotInitialized => "not initialized",
        ProjectStatus::NotProvisioned => "not set up here",
        ProjectStatus::NotRepository => "not a repository",
        ProjectStatus::SetupIncomplete => "setup incomplete",
        ProjectStatus::NotAdopted => "not adopted",
        ProjectStatus::Error(_) => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::fixtures::fixture_rect;

    #[test]
    fn repository_recall_and_settings_actions_never_overlap() {
        for width in 3..60 {
            for collapsed in [false, true] {
                let layout = repository_button_layout(fixture_rect(0, 0, width, 7), collapsed);
                let a = layout.recall.rect;
                let b = layout.settings.rect;
                let overlap = a.w > 0
                    && b.w > 0
                    && a.x < b.x.saturating_add(b.w)
                    && b.x < a.x.saturating_add(a.w)
                    && a.y < b.y.saturating_add(b.h)
                    && b.y < a.y.saturating_add(a.h);
                assert!(!overlap, "width {width}, collapsed {collapsed}");
                for button in [&layout.recall, &layout.settings] {
                    if button.rect.w > 0 {
                        assert!(
                            u16::try_from(button.text.chars().count()).unwrap_or(u16::MAX)
                                <= button.rect.w,
                            "width {width}, collapsed {collapsed}: {:?}",
                            button.text
                        );
                    }
                }
            }
        }
        let wide = repository_button_layout(fixture_rect(0, 0, 48, 7), false);
        assert_eq!(wide.recall.text, "[ Recall Viewer ]");
        assert_eq!(wide.settings.text, "[ Project Settings ]");
        assert_eq!(wide.recall.rect.y, wide.settings.rect.y);
    }

    #[test]
    fn the_status_line_of_a_failing_integration_reads_its_act() {
        let detail = DetailView::thin(
            "/r/demo".into(),
            "demo".into(),
            ProjectStatus::Error(crate::StatusError::Integration),
        );
        assert_eq!(
            status_line(&detail),
            "capture failing \u{2014} s / /settings"
        );
    }
}
