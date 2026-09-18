//! The dispatch overlay family: Results, Confirm, Running, and the Alert /
//! Notice message — each a centered modal over the shared geometry, with the
//! focus ring's bracket cue and the severity frame colour every surface
//! borrows.

use crate::col2::sanitize_for_terminal;
use crate::layout::Rect;
use crate::render::frame::{Frame, Style};
use crate::state::ViewModel;

use super::grid::draw_overflow_bar;
use super::modal::{
    draw_modal_backdrop, modal_close_rect, modal_primary_rect, modal_rect, primary_chip,
    CONFIRM_BUTTONS,
};
use super::text::clip_line;
use super::ACCENT;

/// Draw the dispatch overlay as a centered modal: a themed scrim over
/// the dashboard, then a bordered, scrollable pane. Geometry comes from the one
/// shared [`modal_rect`], so pixels and hit-test targets can never disagree.
pub(super) fn draw_overlay(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    use crate::dispatch::Overlay;
    let Some(overlay) = &vm.overlay else { return };
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 3 {
        return; // clamped away on tiny terminals; the bar still works
    }
    draw_modal_backdrop(vm, frame, rect);
    match overlay {
        Overlay::Results {
            title,
            lines,
            scroll,
            footer,
            ..
        } => draw_results(
            frame,
            vm.theme,
            unicode,
            rect,
            title,
            lines,
            *scroll,
            footer.as_deref(),
        ),
        Overlay::Confirm { request, repo } => draw_confirm(vm, frame, rect, request, repo),
        Overlay::Running { title, lines } => draw_running(frame, rect, title, lines),
        // A message: an Alert (blocking) or a Notice (auto-dismissing)
        // renders from `{ title, body, severity, action }` through one path.
        Overlay::Alert(msg) | Overlay::Notice(msg) => draw_message(vm, frame, rect, msg),
    }
    // The universal close X on the top-right border, for modals that
    // carry a Close control (Results / Confirm; Running has none). The focus
    // wash marks it when the ring rests on Close; the flash when it is pressed.
    let control = crate::dispatch::OverlayControl::Close;
    if !overlay.controls().contains(&control) {
        return;
    }
    if let Some(cr) = modal_close_rect(rect) {
        let style = overlay_button_style(vm, &crate::hit::Target::OverlayClose, control);
        let focused = vm.focused_overlay_control() == Some(control);
        frame.put_str(cr.x, cr.y, &focus_brackets("[x]", focused), style);
    }
}

/// A message severity's frame colour: red error, amber warning, green
/// success, accent info. Colour-only, so the mono/ASCII tiers drop it and the
/// border glyphs stand — the one styling axis decided at draw, not per call site.
pub(super) fn severity_style(severity: crate::dispatch::Severity) -> Style {
    use crate::dispatch::Severity;
    Style::fg(match severity {
        Severity::Error => crate::theme::STATUS_ERROR,
        Severity::Warning => crate::theme::HEALTH_WARN,
        Severity::Success | Severity::Info => crate::theme::ACCENT,
    })
}

/// Draw the Confirm modal: the verb line and the target repository, then
/// Confirm / Cancel as two focus-ring buttons — the focused one carries the
/// focus wash, a pressed one the flash. The gap (3) and columns mirror the
/// hit-test exactly.
fn draw_confirm(
    vm: &ViewModel,
    frame: &mut Frame,
    rect: Rect,
    request: &crate::dispatch::DispatchRequest,
    repo: &str,
) {
    use crate::dispatch::OverlayControl;
    let inner = frame.border(rect, "confirm", ACCENT);
    let verb_line = sanitize_for_terminal(&format!("will run: {}", request.verb_line()));
    clip_line(frame, inner, 0, &verb_line, Style::DEFAULT);
    let repo_line = sanitize_for_terminal(&format!("target repository: {repo}"));
    clip_line(frame, inner, 1, &repo_line, Style::DEFAULT);
    let brow = inner.y + 3;
    let confirm_w = u16::try_from(CONFIRM_BUTTONS.0.chars().count()).unwrap_or(0);
    let focused = vm.focused_overlay_control();
    frame.put_str(
        inner.x,
        brow,
        &focus_brackets(CONFIRM_BUTTONS.0, focused == Some(OverlayControl::Confirm)),
        overlay_button_style(
            vm,
            &crate::hit::Target::OverlayConfirm,
            OverlayControl::Confirm,
        ),
    );
    let cancel_x = inner.x.saturating_add(confirm_w).saturating_add(3);
    frame.put_str(
        cancel_x,
        brow,
        &focus_brackets(CONFIRM_BUTTONS.1, focused == Some(OverlayControl::Cancel)),
        overlay_button_style(
            vm,
            &crate::hit::Target::OverlayCancel,
            OverlayControl::Cancel,
        ),
    );
}

/// Draw a message modal into `rect`: the severity-coloured frame, the
/// sanitized body lines, and — for a message with a primary action — the
/// bottom-left action chip (the wizard's *add project*). The universal close X
/// is drawn by the shared tail of [`draw_overlay`].
fn draw_message(vm: &ViewModel, frame: &mut Frame, rect: Rect, msg: &crate::dispatch::Message) {
    let style = severity_style(msg.severity);
    let inner = frame.border(rect, &sanitize_for_terminal(&msg.title), style);
    for (i, line) in msg.body.iter().enumerate() {
        let Ok(dy) = u16::try_from(i) else { break };
        if dy >= inner.h {
            break;
        }
        clip_line(
            frame,
            inner,
            dy,
            &sanitize_for_terminal(line),
            Style::DEFAULT,
        );
    }
    if let (Some(chip), Some(pr)) = (
        primary_chip(msg.action),
        modal_primary_rect(inner, msg.action),
    ) {
        let control = crate::dispatch::OverlayControl::Primary;
        let bstyle = overlay_button_style(vm, &crate::hit::Target::WizardAdd, control);
        let focused = vm.focused_overlay_control() == Some(control);
        frame.put_str(pr.x, pr.y, &focus_brackets(&chip, focused), bstyle);
    }
}

/// Draw a Results modal (the injected dispatcher): the bordered title, the scrolled body lines,
/// the nonzero-class footer, and the modal-body overflow scrollbar
/// when the listing exceeds the box.
fn draw_results(
    frame: &mut Frame,
    theme: crate::theme::Theme,
    unicode: bool,
    rect: Rect,
    title: &str,
    lines: &[String],
    scroll: usize,
    footer: Option<&str>,
) {
    let inner = frame.border(rect, &sanitize_for_terminal(title), ACCENT);
    let footer_rows = usize::from(footer.is_some());
    let visible = usize::from(inner.h).saturating_sub(footer_rows);
    for (i, line) in lines.iter().skip(scroll).take(visible).enumerate() {
        let Ok(dy) = u16::try_from(i) else { break };
        clip_line(frame, inner, dy, line, Style::DEFAULT);
    }
    if let Some(f) = footer {
        let dy = inner.h.saturating_sub(1);
        clip_line(frame, inner, dy, &sanitize_for_terminal(f), ACCENT);
    }
    // The modal-body scrollbar over the body's rows (a long listing overflows the
    // 80/80 box — the shared modal geometry); its presence means "there is more".
    let body = Rect {
        h: u16::try_from(visible).unwrap_or(inner.h),
        ..inner
    };
    draw_overflow_bar(frame, body, scroll, lines.len(), theme, unicode);
}

/// Draw the Running frame: the verb line, then the body under it — a
/// setup's log, one step per row, clipped to the box like a message body.
fn draw_running(frame: &mut Frame, rect: Rect, title: &str, lines: &[String]) {
    let inner = frame.border(rect, "running", ACCENT);
    clip_line(
        frame,
        inner,
        0,
        &sanitize_for_terminal(&format!("running {title} …")),
        Style::DEFAULT,
    );
    for (i, line) in lines.iter().enumerate() {
        let Some(dy) = u16::try_from(i.saturating_add(1))
            .ok()
            .filter(|dy| *dy < inner.h)
        else {
            break;
        };
        clip_line(
            frame,
            inner,
            dy,
            &sanitize_for_terminal(line),
            Style::DEFAULT,
        );
    }
}

/// The style for a modal button: the accent, washed with the
/// focus background when the focus ring rests on it, and the press-flash
/// background (which wins) while it is pressed. Colour-only, so the mono/ASCII
/// tiers keep the label glyphs and drop the wash.
fn overlay_button_style(
    vm: &ViewModel,
    target: &crate::hit::Target,
    control: crate::dispatch::OverlayControl,
) -> Style {
    if let Some(bg) = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(target)) {
        ACCENT.with_bg(bg)
    } else if vm.focused_overlay_control() == Some(control) {
        ACCENT
            .with_bg(crate::theme::SELECT_BG)
            .with_mods(crate::render::Mods::BOLD)
    } else {
        ACCENT
    }
}

/// A modal control's text as drawn: its square brackets turned into angle
/// brackets while the focus ring rests on it (`[ Enter: confirm ]` →
/// `< Enter: confirm >`, `[x]` → `<x>`). A glyph cue inside the chip's own
/// cells — the same width, so the hit-test is untouched — because the focus
/// wash is colour-only and the mono / ASCII floors would otherwise show no
/// ring at all: a keyboard user could not tell which control `Enter` fires.
fn focus_brackets(text: &str, focused: bool) -> String {
    if !focused {
        return text.to_owned();
    }
    text.chars()
        .map(|c| match c {
            '[' => '<',
            ']' => '>',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;
    use crate::state::AppState;
    use crate::view::draw;
    use crate::view::fixtures::render_to_text;

    /// The focus ring's floor cue: the focused modal control swaps its square
    /// brackets for angle brackets, in the same cells, so a keyboard user on a
    /// colourless tier can still tell which control `Enter` fires.
    #[test]
    fn the_focused_modal_control_wears_angle_brackets() {
        assert_eq!(
            focus_brackets("[ Enter: confirm ]", true),
            "< Enter: confirm >"
        );
        assert_eq!(focus_brackets("[x]", true), "<x>");
        assert_eq!(focus_brackets("[x]", false), "[x]");
        assert_eq!(
            focus_brackets("[ Enter: confirm ]", true).chars().count(),
            "[ Enter: confirm ]".chars().count(),
            "the cue never changes the chip's width"
        );
        // Live: with the ring on the wizard's primary chip, the floor render
        // shows the angle-bracket form and not the square one.
        let mut s = AppState::new(Vec::new(), Size::new(100, 30));
        s.app.splash.skip();
        s.app.set_projects(Vec::new());
        let Some(crate::dispatch::Overlay::Alert(msg)) = s.app.overlay.clone() else {
            panic!("an empty registry raises the wizard");
        };
        let controls = s
            .app
            .overlay
            .as_ref()
            .map(crate::dispatch::Overlay::controls)
            .unwrap_or_default();
        s.app.overlay_focus = controls
            .iter()
            .position(|c| *c == crate::dispatch::OverlayControl::Primary)
            .expect("the wizard carries a primary control");
        let chip = primary_chip(msg.action).expect("the wizard's primary chip");
        let text = render_to_text(&s.snapshot());
        assert!(
            text.contains(&focus_brackets(&chip, true)),
            "the focused primary wears angle brackets: {text}"
        );
        assert!(
            !text.contains(&chip),
            "the square form is gone while focused: {text}"
        );
    }

    #[test]
    fn wizard_when_empty() {
        // The first-run wizard renders as a modal, with the message,
        // the clickable *add project* chip, and the universal close X.
        let mut s = AppState::new(vec![], Size::new(100, 30));
        s.app.splash.skip();
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("No projects registered"), "the message shows");
        assert!(
            text.contains("Add a project"),
            "the primary action chip shows"
        );
        assert!(text.contains("[x]"), "the universal close X shows");
    }

    #[test]
    fn severity_frames_each_message_class() {
        use crate::dispatch::Severity;
        assert_eq!(
            severity_style(Severity::Error).fg,
            crate::theme::STATUS_ERROR
        );
        assert_eq!(
            severity_style(Severity::Warning).fg,
            crate::theme::HEALTH_WARN
        );
        assert_eq!(severity_style(Severity::Success).fg, crate::theme::ACCENT);
        assert_eq!(severity_style(Severity::Info).fg, crate::theme::ACCENT);
    }

    #[test]
    fn an_error_alert_frames_the_message_in_red() {
        let mut s = AppState::new(vec![], Size::new(100, 30));
        s.app.splash.skip();
        s.open_alert(crate::dispatch::Message::error(
            "disk full",
            vec!["no space left on device".into()],
        ));
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&vm, &mut b, &mut f);
        let rect = modal_rect(vm.size);
        // The top-left border cell carries the error severity colour.
        assert_eq!(
            f.get(rect.x, rect.y).map(|c| c.style.fg),
            Some(crate::theme::STATUS_ERROR),
            "an error Alert frames red"
        );
        // A dismiss-only alert has no primary chip.
        let text = render_to_text(&vm);
        assert!(text.contains("no space left on device"));
        assert!(
            !text.contains("Add a project"),
            "no primary on a plain alert"
        );
    }
}
