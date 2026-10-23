//! Repository status and capture facts, with adoption on an unadopted worktree status row.

use crate::col2::{sanitize_for_terminal, DetailView};
use crate::layout::Rect;
use crate::render::frame::Frame;
use crate::theme::{Icon, Section, Theme};
use crate::{EntryKind, ProjectStatus};

use super::{card_frame, line, CardCtx};

/// The Repository card's **adopt act** (the family view): the status row,
/// clickable exactly when the card is drawing that row for an unadopted
/// member.
///
/// ONE solver feeds the draw and the hit-test, so the cells the operator sees
/// and the cells that answer a click are the same cells by construction — the
/// blind spot this closes, and the reason this is a function rather than a rule
/// spelled in two files. `None` means there is no act here: the card is
/// collapsed, too short for the status row, or the member is already adopted.
#[must_use]
pub fn repository_adopt_rect(rect: Rect, collapsed: bool, status: ProjectStatus) -> Option<Rect> {
    if collapsed || status != ProjectStatus::NotAdopted {
        return None;
    }
    if rect.w < 2 || rect.h < 2 {
        return None;
    }
    let inner = card_inner(rect);
    if inner.h < 3 {
        return None;
    }
    Some(Rect {
        x: inner.x,
        y: inner.y + 2,
        w: inner.w,
        h: 1,
    })
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
    let inner = card_frame(
        theme,
        Section::Repository,
        "repository",
        rect,
        ctx.focused,
        frame,
    );
    if collapsed {
        draw_repository_headline(ctx, theme, inner, frame);
        return;
    }
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
    if inner.h >= 3 {
        draw_repository_status(ctx, theme, rect, inner, frame);
    }
    draw_repository_facts(ctx, theme, inner, frame);
}

fn draw_repository_facts(ctx: &CardCtx<'_>, theme: Theme, inner: Rect, frame: &mut Frame) {
    let d = ctx.detail;
    let bottom = inner.h;
    if bottom > 3 {
        let git = d.git_summary.as_ref().map_or_else(
            || "Git status unavailable".to_owned(),
            |git| {
                let state = match git.tracked_changes {
                    Some(true) => "tracked files modified",
                    Some(false) => "tracked files clean",
                    None => "tracked status unavailable",
                };
                format!("{} · {state}", sanitize_for_terminal(&git.branch))
            },
        );
        line(frame, inner, 3, &git, theme.body());
    }
    if bottom > 4 {
        let capture =
            if d.capture.events == 0 && d.capture.sessions == 0 && d.capture.checkpoints == 0 {
                "No attributed capture facts available".to_owned()
            } else {
                format!(
                    "Capture: {} sessions · {} checkpoints · {} events",
                    d.capture.sessions, d.capture.checkpoints, d.capture.events
                )
            };
        line(frame, inner, 4, &capture, theme.dim());
    }
    if bottom > 5 {
        let id = d.capture.last_checkpoint_id.as_deref().map_or_else(
            || "unavailable".into(),
            |id| id.chars().take(12).collect::<String>(),
        );
        line(
            frame,
            inner,
            5,
            &sanitize_for_terminal(&format!("Latest checkpoint: {id}")),
            theme.dim(),
        );
    }
    if bottom > 6 {
        let agents = d
            .project_settings
            .integrations
            .iter()
            .filter(|row| row.enabled())
            .map(|row| row.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        line(
            frame,
            inner,
            6,
            &sanitize_for_terminal(&format!("Harnesses: {agents}")),
            theme.dim(),
        );
    }
}

/// The collapsed card retains a plain repository/status headline.
fn draw_repository_headline(ctx: &CardCtx<'_>, theme: Theme, inner: Rect, frame: &mut Frame) {
    let summary = format!(
        "{} · {}",
        sanitize_for_terminal(&ctx.detail.label),
        status_word(ctx.detail.status)
    );
    line(frame, inner, 0, &summary, theme.body());
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

/// The status row's text: the status word, and — for a family row whose
/// `Running` count is the sum over its members — which members it came from
/// (the family view), else the act the status asks for.
///
/// One row keeps the activity count beside the members it belongs to. Members are
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
    fn repository_card_uses_its_rows_for_facts_without_navigation_buttons() {
        use crate::cards::fixtures::{fixture_ctx, fixture_detail};
        for collapsed in [false, true] {
            let detail = fixture_detail(ProjectStatus::Idle);
            let activity = crate::col3::ActivityView::default();
            let ctx = fixture_ctx(&detail, &activity);
            let mut frame = Frame::new(crate::render::Size::new(80, 12));
            draw_repository(
                &ctx,
                Theme::default(),
                fixture_rect(0, 0, 80, 12),
                collapsed,
                &mut frame,
            );
            let text: String = frame.cells().iter().map(|cell| cell.ch).collect();
            assert!(text.contains("proj"));
            assert!(!text.contains("Recall Viewer") && !text.contains("Project Settings"));
            if !collapsed {
                assert!(text.contains("Latest checkpoint:"));
            }
        }
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
