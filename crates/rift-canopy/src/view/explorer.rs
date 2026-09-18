//! The destination explorer: the `⬑ ..` back row, the scrolling entry list,
//! the status line and the two action chips, over the shared modal frame.
//!
//! One solver per region feeds BOTH the draw and the hit-test (modal-surface
//! parity), and the row solver is the tree's first to honour a scroll offset
//! — the profile menu simply stops drawing past the frame, which a directory
//! listing cannot afford. Layout inside the modal's inner box: the `⬑ ..`
//! back row on top, the scrolling list beneath it, then the status line and
//! the button row on the two bottom rows (7 entry rows at the 40×12 modal
//! floor).

use crate::col2::sanitize_for_terminal;
use crate::layout::Rect;
use crate::render::frame::{Frame, Style};
use crate::state::ViewModel;

use super::grid::draw_overflow_bar;
use super::modal::{draw_modal_backdrop, draw_mode_close, modal_rect, mode_chip_style, ModalFrame};
use super::overlay::severity_style;
use super::text::{clip_line, elide_left};
use super::ACCENT;

/// The explorer's action chips, in display order — `(wide label, narrow
/// fallback)`. One source for the draw, the measurement, and the target
/// mapping (the `MEMBERS_CHIPS` contract). The narrow form exists so a chip
/// **degrades** rather than vanishing: an act the surface teaches must stay
/// clickable at every width it draws at, down to the 24-column tier where the
/// modal is narrower than `MIN_MODAL_COLS` (modal-surface parity).
pub const EXPLORER_BUTTONS: [(&str, &str); 2] = [
    ("[ select destination ]", "[ select ]"),
    ("[ input as path ]", "[ path ]"),
];

/// The dot-entry toggle's chip — the click twin of `.`, right-aligned inside
/// the back row rather than sitting with the action chips, so it keeps its own
/// width budget and survives the narrow tiers (the `profile_menu_settings_rect`
/// precedent).
pub const EXPLORER_DOTFILES_CHIP: &str = "[.dotfiles]";

/// The `⬑ ..` back row — the explorer's top inner row, full width.
#[must_use]
pub fn explorer_up_rect(modal: Rect) -> Rect {
    explorer_inner(modal).map_or_else(
        || explorer_empty(modal),
        |inner| Rect {
            x: inner.x,
            y: inner.y,
            w: inner.w,
            h: 1,
        },
    )
}

/// The scrolling entry-list viewport: everything between the back row and the
/// status + button rows. Zero-height on a frame with no room to spare.
#[must_use]
pub fn explorer_list_area(modal: Rect) -> Rect {
    explorer_inner(modal).map_or_else(
        || explorer_empty(modal),
        |inner| Rect {
            x: inner.x,
            y: inner.y + 1,
            w: inner.w,
            h: inner.h.saturating_sub(3),
        },
    )
}

/// The row rect for **visible** entry `i` at viewport offset `scroll` — the
/// scroll-aware solver draw and hit share. Zero-sized when the row is scrolled
/// out of the viewport in either direction (never clamped into a wrong row).
#[must_use]
pub fn explorer_row_rect(modal: Rect, i: usize, scroll: usize) -> Rect {
    let list = explorer_list_area(modal);
    if list.h == 0 {
        return explorer_empty(modal);
    }
    let Some(dy) = i.checked_sub(scroll).and_then(|d| u16::try_from(d).ok()) else {
        return explorer_empty(modal);
    };
    if dy >= list.h {
        return explorer_empty(modal);
    }
    Rect {
        x: list.x,
        y: list.y + dy,
        w: list.w,
        h: 1,
    }
}

/// The dot-toggle chip's rect. Zero-sized only when the back row cannot host a
/// chip beside the `..` label at all.
#[must_use]
pub fn explorer_dotfiles_rect(modal: Rect) -> Rect {
    let up = explorer_up_rect(modal);
    let chip_w = u16::try_from(EXPLORER_DOTFILES_CHIP.chars().count()).unwrap_or(0);
    if up.h == 0 || up.w < chip_w.saturating_add(8) {
        return explorer_empty(modal);
    }
    Rect {
        x: up.x + up.w - chip_w - 1,
        y: up.y,
        w: chip_w,
        h: 1,
    }
}

/// The explorer's chip rects on the inner bottom row, left-aligned with 1-cell
/// gaps — one solver for the draw and the hit-test (modal-surface parity), returning the
/// label it measured with so the two sides can never disagree about the text.
/// Each chip takes its wide label when it fits and its narrow fallback
/// otherwise; a chip that fits in neither form clips away **whole** (never
/// half-drawn). Both acts stay clickable down to the 24-column tier.
#[must_use]
pub fn explorer_button_rects(modal: Rect) -> Vec<(Rect, crate::hit::Target, &'static str)> {
    let Some(inner) = explorer_inner(modal) else {
        return Vec::new();
    };
    let targets = [
        crate::hit::Target::ExplorerSelect,
        crate::hit::Target::ExplorerTypePath,
    ];
    let y = inner.y + inner.h - 1;
    let right = inner.x + inner.w;
    let mut x = inner.x + 1;
    let mut out = Vec::with_capacity(EXPLORER_BUTTONS.len());
    for ((wide, narrow), target) in EXPLORER_BUTTONS.iter().zip(targets) {
        let fits = |text: &str| {
            let w = u16::try_from(text.chars().count()).unwrap_or(0);
            (x.saturating_add(w) <= right).then_some(w)
        };
        let Some((w, text)) = fits(wide)
            .map(|w| (w, *wide))
            .or_else(|| fits(narrow).map(|w| (w, *narrow)))
        else {
            break; // too narrow in either form: the chip clips away whole
        };
        out.push((Rect { x, y, w, h: 1 }, target, text));
        x = x.saturating_add(w).saturating_add(1);
    }
    out
}

/// Draw the destination explorer: the shared centered modal frame +
/// scrim, the open folder in the title (elided from the LEFT so the basename
/// survives), the `⬑ ..` back row, the scrolling entry list (directories
/// enterable, files dimmed and inert), an overflow scrollbar, the status line,
/// and the two action chips. Every stage carries the close `[x]`.
/// Pure drawing over the snapshot; every name passes the shared sanitizer.
pub(super) fn draw_explorer(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 5 {
        return;
    }
    draw_modal_backdrop(vm, frame, rect);
    let inner = frame.border(rect, &explorer_title(vm, rect.w), ACCENT);
    draw_mode_close(vm, frame, rect, &crate::hit::Target::ExplorerClose);
    let modal = ModalFrame {
        rect,
        inner,
        unicode,
    };
    draw_back_row(vm, frame, modal);
    draw_dotfiles_chip(vm, frame, rect);
    draw_entries(vm, frame, modal);
    draw_status_line(vm, frame, inner);
    for (chip, target, text) in explorer_button_rects(rect) {
        frame.put_str(chip.x, chip.y, text, mode_chip_style(vm, &target));
    }
}

/// The title names what `[ select destination ]` will register — so it is
/// elided from the left, keeping the basename that identifies the folder.
/// The budget leaves room for the corner glyphs and the close `[x]`.
fn explorer_title(vm: &ViewModel, modal_w: u16) -> String {
    const TITLE_PREFIX: &str = "add project · ";
    let budget = usize::from(modal_w.saturating_sub(8));
    let cwd = sanitize_for_terminal(&vm.explorer.display_cwd());
    if budget <= TITLE_PREFIX.chars().count() {
        return "add project".to_owned();
    }
    format!(
        "{TITLE_PREFIX}{}",
        elide_left(&cwd, budget - TITLE_PREFIX.chars().count())
    )
}

/// The `⬑ ..` back row — dimmed and inert at the filesystem root (a calm
/// edge), press-flash aware elsewhere.
fn draw_back_row(vm: &ViewModel, frame: &mut Frame, modal: ModalFrame) {
    let up = explorer_up_rect(modal.rect);
    if up.h == 0 {
        return;
    }
    let glyph = if modal.unicode { "⬑" } else { "^" };
    let style = if vm.explorer.cwd.parent().is_none() {
        vm.theme.dim()
    } else {
        mode_chip_style(vm, &crate::hit::Target::ExplorerUp)
    };
    let dy = up.y - modal.inner.y;
    clip_line(frame, modal.inner, dy, &format!(" {glyph} .."), style);
}

/// The dot-toggle's click twin (modal-surface parity), right-aligned in the
/// back row: the status line teaches `.`, so a pointer must be able to press
/// it too. Lit in the accent while dot-entries are shown, muted while hidden —
/// the state reads off the chip itself.
fn draw_dotfiles_chip(vm: &ViewModel, frame: &mut Frame, modal: Rect) {
    let dots = explorer_dotfiles_rect(modal);
    if dots.w == 0 {
        return;
    }
    let fg = if vm.explorer.show_hidden {
        crate::theme::ACCENT
    } else {
        crate::theme::MUTED
    };
    let bg = mode_chip_style(vm, &crate::hit::Target::ExplorerDotfiles).bg;
    frame.put_str(
        dots.x,
        dots.y,
        EXPLORER_DOTFILES_CHIP,
        Style::fg(fg).with_bg(bg),
    );
}

/// The entries through the scroll-aware solver, then the overflow indicator
/// only (the scrollbar rule) — a fitting listing draws none. An empty
/// directory says so in the first list row.
fn draw_entries(vm: &ViewModel, frame: &mut Frame, modal: ModalFrame) {
    let visible = vm.explorer.visible();
    let list = explorer_list_area(modal.rect);
    if visible.is_empty() && list.h > 0 {
        clip_line(
            frame,
            modal.inner,
            list.y - modal.inner.y,
            " (empty directory)",
            Style::fg(crate::theme::MUTED),
        );
    }
    for (i, entry) in visible.iter().enumerate() {
        let rr = explorer_row_rect(modal.rect, i, vm.explorer.scroll);
        if rr.h == 0 {
            continue; // scrolled out of the viewport
        }
        draw_entry_row(vm, frame, modal.inner, rr, entry, i == vm.explorer.cursor);
    }
    draw_overflow_bar(
        frame,
        list,
        vm.explorer.scroll,
        vm.explorer.visible_len(),
        vm.theme,
        modal.unicode,
    );
}

/// One entry row. The cursor cue is the one every list surface shares (the
/// rail's rule): a cursor-only marker glyph in the row's first cell, so the
/// highlighted row survives the colourless tiers where the selection wash
/// resolves away, plus the bold selection pill on the tiers that carry it. A
/// directory is told apart by its `/` suffix, so the marker column belongs to
/// the cursor. A junction says so — and an unlistable one is a dim, inert row
/// like a file, so the operator is never walked into an access-denied listing.
fn draw_entry_row(
    vm: &ViewModel,
    frame: &mut Frame,
    inner: Rect,
    rr: Rect,
    entry: &crate::explorer::ExplorerEntry,
    on_cursor: bool,
) {
    let style = match (on_cursor, entry.is_dir) {
        (true, _) => vm.theme.selection(),
        (false, true) => Style::fg(vm.theme.body_fg()),
        (false, false) => vm.theme.dim(),
    };
    if on_cursor {
        crate::chrome::cursor_row(frame, rr, style);
    }
    let name = sanitize_for_terminal(&entry.name);
    let suffix = if entry.is_dir { "/" } else { "" };
    let tag = match entry.tag {
        Some(crate::explorer::EntryTag::Junction) => " (junction)",
        None => "",
    };
    let line = format!(
        "{}{name}{suffix}{tag}",
        crate::chrome::row_prefix(on_cursor)
    );
    clip_line(frame, inner, rr.y - inner.y, &line, style);
}

/// The status line on the second-from-bottom inner row: an error in the
/// warning colour, else the truncation count or the key hints, muted.
fn draw_status_line(vm: &ViewModel, frame: &mut Frame, inner: Rect) {
    if inner.h < 2 {
        return;
    }
    let (line, is_error) = explorer_status_line(&vm.explorer, usize::from(inner.w));
    let style = if is_error {
        severity_style(crate::dispatch::Severity::Warning)
    } else {
        Style::fg(crate::theme::MUTED)
    };
    clip_line(frame, inner, inner.h - 2, &line, style);
}

/// The explorer's inner content box, or `None` when the frame is too small to
/// host the surface at all (the one size guard all four solvers share).
fn explorer_inner(modal: Rect) -> Option<Rect> {
    (modal.w >= 4 && modal.h >= 5).then(|| Rect {
        x: modal.x + 1,
        y: modal.y + 1,
        w: modal.w - 2,
        h: modal.h - 2,
    })
}

/// A zero-sized rect anchored at `modal`'s origin (the "no such region" reply
/// every explorer solver returns, matching `profile_menu_row_rect`'s shape).
fn explorer_empty(modal: Rect) -> Rect {
    Rect {
        x: modal.x,
        y: modal.y,
        w: 0,
        h: 0,
    }
}

/// The explorer's status line: the inline error, else the truncation
/// disclosure, else the muted key hints (the destination explorer precedence — a problem
/// always outranks a count, and a count outranks a reminder).
///
/// An error names the directory that failed — elided from the LEFT so its
/// basename survives, in whatever room the error text leaves across `width`
/// cells — because the operator is still in `cwd`, and `Access is denied`
/// alone reads as a problem with the dashboard rather than with one folder.
/// A row too narrow for any useful part of the path drops it rather than
/// showing a bare ellipsis.
#[must_use]
fn explorer_status_line(explorer: &crate::explorer::ExplorerState, width: usize) -> (String, bool) {
    if let Some(err) = &explorer.error {
        const HEAD: &str = " ! cannot list ";
        let err = sanitize_for_terminal(err);
        let line = match &explorer.error_path {
            Some(path) => {
                let budget = width.saturating_sub(HEAD.chars().count() + 2 + err.chars().count());
                if budget >= 4 {
                    let shown = elide_left(&sanitize_for_terminal(&path.to_string_lossy()), budget);
                    format!("{HEAD}{shown}: {err}")
                } else {
                    format!(" ! cannot list: {err}")
                }
            }
            None => format!(" ! {err}"),
        };
        return (line, true);
    }
    if explorer.omitted > 0 {
        let kept = explorer.entries.len();
        return (
            format!(
                " showing {kept} of {}",
                kept.saturating_add(explorer.omitted)
            ),
            false,
        );
    }
    (
        " ⏎ open · ⌫ up · s select · i type path · . dotfiles · esc close".to_owned(),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use crate::view::draw;
    use crate::view::fixtures::{
        dir, explorer_row_text, explorer_state, render_to_text, CapsBackend,
    };

    #[test]
    fn explorer_draws_the_open_folder_its_rows_and_both_affordances() {
        let mut s = explorer_state();
        let text = render_to_text(&s.snapshot());
        assert!(
            text.contains("add project"),
            "the frame names the act: {text}"
        );
        assert!(
            text.contains("codebase"),
            "the title carries the open folder's basename: {text}"
        );
        assert!(text.contains(".."), "the back row renders: {text}");
        assert!(text.contains("alpha/"), "directories wear the `/`: {text}");
        assert!(text.contains("beta/"), "{text}");
        assert!(
            text.contains("notes.md") && !text.contains("notes.md/"),
            "files list without the directory suffix: {text}"
        );
        // Both acts have a drawn chip, and the mode carries the shared close X.
        assert!(text.contains(EXPLORER_BUTTONS[0].0), "{text}");
        assert!(text.contains(EXPLORER_BUTTONS[1].0), "{text}");
        assert!(text.contains("[x]"), "the close X renders: {text}");
    }

    #[test]
    fn explorer_dims_files_and_fills_the_cursor_row() {
        let mut s = explorer_state();
        s.app.explorer.cursor = 1; // → beta/
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&vm, &mut b, &mut f);
        let modal = modal_rect(vm.size);
        let cursor_row = explorer_row_rect(modal, 1, 0);
        assert!(cursor_row.h > 0);
        let cell = f.get(cursor_row.x, cursor_row.y).expect("a drawn cell");
        assert_eq!(
            cell.ch,
            crate::chrome::CURSOR_MARKER,
            "the cursor row carries the marker glyph"
        );
        assert_eq!(
            cell.style.bg,
            crate::theme::SELECT_BG,
            "the cursor row is a filled pill"
        );
        assert!(
            cell.style.mods.contains(crate::render::Mods::BOLD),
            "bold survives the mono tier"
        );
        // The file row (index 2) reads as context, not a choice.
        let file_row = explorer_row_rect(modal, 2, 0);
        let file_cell = f
            .get(file_row.x + 2, file_row.y)
            .expect("the file row's first glyph");
        assert_eq!(
            file_cell.style.fg,
            vm.theme.dim().fg,
            "files are dimmed and inert"
        );
    }

    #[test]
    fn explorer_title_elides_a_long_path_from_the_left() {
        let mut s = explorer_state();
        let deep = format!("/home/dev{}/final", "/very-long-directory-name".repeat(8));
        s.app.explorer.cwd = std::path::PathBuf::from(&deep);
        let text = render_to_text(&s.snapshot());
        assert!(
            text.contains("final"),
            "the basename survives the elision — it is what select registers: {text}"
        );
        assert!(
            !text.contains("/home/dev/very-long"),
            "the head is what gets cut: {text}"
        );
        assert!(text.contains('…'), "the cut is marked: {text}");
    }

    #[test]
    fn explorer_status_line_prefers_the_error_then_the_truncation_then_hints() {
        // The status line is the modal's second-from-bottom inner row (the
        // chips take the last), read directly so the dashboard's own hint bar
        // beneath cannot answer for it.
        let mut s = explorer_state();
        let status = explorer_row_text(&mut s, 2);
        assert!(status.contains("s select"), "hints at rest: {status}");
        // …truncation outranks them…
        s.app.explorer.omitted = 7;
        let status = explorer_row_text(&mut s, 2);
        assert!(status.contains("showing 3 of 10"), "{status}");
        assert!(
            !status.contains("s select"),
            "the count replaces the hints: {status}"
        );
        // …and an error outranks everything.
        s.app.explorer.error = Some("permission denied".into());
        let status = explorer_row_text(&mut s, 2);
        assert!(status.contains("permission denied"), "{status}");
        assert!(
            !status.contains("showing 3 of 10"),
            "a problem outranks a count: {status}"
        );
    }

    /// The status line names the folder that could not be listed — elided
    /// from the left so its basename survives — beside the OS error, because
    /// the operator is still in the folder they came from and a bare
    /// `Access is denied` read as a problem with the dashboard itself.
    #[test]
    fn explorer_status_line_names_the_unlistable_folder_elided_from_the_left() {
        let mut st = crate::explorer::ExplorerState {
            error: Some("Access is denied. (os error 5)".into()),
            error_path: Some(std::path::PathBuf::from(
                r"C:\Users\Shadow\a-very-long-profile-directory-name\My Documents",
            )),
            ..Default::default()
        };
        let (line, is_error) = explorer_status_line(&st, 60);
        assert!(is_error);
        assert!(
            line.starts_with(" ! cannot list …"),
            "elided from the left: {line}"
        );
        assert!(
            line.ends_with("My Documents: Access is denied. (os error 5)"),
            "{line}"
        );
        assert!(line.chars().count() <= 60, "fits the row: {line}");
        // Room to spare: the whole path shows.
        let (full, _) = explorer_status_line(&st, 200);
        assert!(full.contains("C:\\Users\\Shadow\\"), "{full}");
        // No path recorded (a listing that failed before the path was known):
        // the error alone, as before.
        st.error_path = None;
        assert_eq!(
            explorer_status_line(&st, 60).0,
            " ! Access is denied. (os error 5)"
        );
    }

    /// A row too narrow for any useful part of the path drops the path rather
    /// than showing a lone ellipsis — the error still names the act.
    #[test]
    fn explorer_status_line_drops_the_path_when_too_narrow() {
        let st = crate::explorer::ExplorerState {
            error: Some("Access is denied. (os error 5)".into()),
            error_path: Some(std::path::PathBuf::from("/home/dev/My Documents")),
            ..Default::default()
        };
        let (line, _) = explorer_status_line(&st, 48);
        assert_eq!(line, " ! cannot list: Access is denied. (os error 5)");
        assert!(!line.contains('…'), "{line}");
    }

    /// A junction row says so beside its name; one Windows refuses to list is
    /// inert — drawn dim like a file, without the directory suffix — while a
    /// listable one keeps the suffix and the directory's style.
    #[test]
    fn a_junction_row_carries_its_suffix_and_an_inert_one_is_dim() {
        let mut s = explorer_state();
        s.app.explorer.entries = vec![
            crate::explorer::ExplorerEntry {
                name: "Documents".into(),
                is_dir: true,
                tag: None,
            },
            crate::explorer::ExplorerEntry {
                name: "My Documents".into(),
                is_dir: false,
                tag: Some(crate::explorer::EntryTag::Junction),
            },
            crate::explorer::ExplorerEntry {
                name: "Linked".into(),
                is_dir: true,
                tag: Some(crate::explorer::EntryTag::Junction),
            },
        ];
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&vm, &mut b, &mut f);
        let text = render_to_text(&vm);
        assert!(text.contains("  My Documents (junction)"), "{text}");
        assert!(
            !text.contains("My Documents/"),
            "an inert junction wears no suffix: {text}"
        );
        assert!(
            text.contains("  Linked/ (junction)"),
            "a listable one stays a directory: {text}"
        );
        let modal = modal_rect(vm.size);
        let inert = explorer_row_rect(modal, 1, 0);
        let cell = f
            .get(inert.x + 2, inert.y)
            .expect("the inert row's first glyph");
        assert_eq!(
            cell.style.fg,
            vm.theme.dim().fg,
            "inert reads as context, like a file"
        );
        let live = explorer_row_rect(modal, 2, 0);
        let cell = f
            .get(live.x + 2, live.y)
            .expect("the listable row's first glyph");
        assert_eq!(
            cell.style.fg,
            vm.theme.body_fg(),
            "listable reads as a directory"
        );
    }

    #[test]
    fn explorer_shows_an_empty_directory_and_a_bar_only_on_overflow() {
        let mut s = explorer_state();
        s.app.explorer.entries.clear();
        assert!(render_to_text(&s.snapshot()).contains("(empty directory)"));

        // The bar is asserted on the DRAWN frame — the track's own cells in the
        // list viewport's right column — not on `chrome::scrollbar` in the
        // abstract, so this pins what `draw_explorer` actually paints.
        let track_glyphs = |s: &mut AppState| {
            let vm = s.snapshot();
            let mut f = Frame::new(vm.size);
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&vm, &mut b, &mut f);
            let list = explorer_list_area(modal_rect(vm.size));
            let x = list.x + list.w - 1;
            (0..list.h)
                .filter(|dy| {
                    f.get(x, list.y + dy)
                        .is_some_and(|c| c.ch == '|' || c.ch == '#')
                })
                .count()
        };
        let mut fits = explorer_state();
        assert_eq!(track_glyphs(&mut fits), 0, "three rows fit — no bar drawn");
        let mut overflows = explorer_state();
        overflows.app.explorer.entries = (0..200).map(|i| dir(&format!("d{i:03}"))).collect();
        let drawn = track_glyphs(&mut overflows);
        assert_eq!(
            drawn,
            usize::from(explorer_list_area(modal_rect(overflows.size)).h),
            "an overflowing listing paints a full-height track"
        );
    }

    #[test]
    fn the_dotfiles_toggle_has_a_drawn_chip_at_every_width_the_surface_draws() {
        // modal-surface parity: the status line teaches `.`, so a pointer
        // must be able to press it — at every width, not just the wide one.
        let mut s = explorer_state();
        let text = render_to_text(&s.snapshot());
        assert!(text.contains(EXPLORER_DOTFILES_CHIP), "{text}");
        for w in [24u16, 40, 80, 120] {
            let modal = modal_rect(crate::render::frame::Size::new(w, 24));
            assert!(
                explorer_dotfiles_rect(modal).w > 0,
                "the dot-toggle chip vanished at {w} columns"
            );
        }
        // It reads its own state: accent while showing, muted while hiding.
        let style_at = |s: &mut AppState| {
            let vm = s.snapshot();
            let mut f = Frame::new(vm.size);
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&vm, &mut b, &mut f);
            let chip = explorer_dotfiles_rect(modal_rect(vm.size));
            f.get(chip.x, chip.y).map(|c| c.style.fg)
        };
        assert_eq!(style_at(&mut s), Some(crate::theme::MUTED));
        s.app.explorer.show_hidden = true;
        assert_eq!(style_at(&mut s), Some(crate::theme::ACCENT));
    }

    #[test]
    fn explorer_chips_degrade_rather_than_vanish_on_narrow_tiers() {
        // modal-surface parity: a chip never renders half a label — but an act the surface
        // teaches must stay clickable, so a chip that cannot fit its wide label
        // takes the narrow one instead. Both acts survive down to the 24-column
        // tier (where the modal is narrower than MIN_MODAL_COLS).
        for w in [24u16, 40, 80, 120] {
            let chips = explorer_button_rects(modal_rect(crate::render::frame::Size::new(w, 24)));
            assert_eq!(
                chips.iter().map(|c| c.1.clone()).collect::<Vec<_>>(),
                vec![
                    crate::hit::Target::ExplorerSelect,
                    crate::hit::Target::ExplorerTypePath,
                ],
                "both acts must be clickable at {w} columns"
            );
            for (rect, _, text) in &chips {
                assert_eq!(
                    usize::from(rect.w),
                    text.chars().count(),
                    "the solver measured the label it reports at {w} columns"
                );
                assert!(
                    EXPLORER_BUTTONS
                        .iter()
                        .any(|(a, b)| *a == *text || *b == *text),
                    "chips only ever draw a declared label"
                );
            }
        }
        // The wide labels win when there is room; the narrow ones stand in when
        // there is not.
        let wide = explorer_button_rects(modal_rect(crate::render::frame::Size::new(120, 40)));
        assert_eq!(wide[0].2, EXPLORER_BUTTONS[0].0);
        assert_eq!(wide[1].2, EXPLORER_BUTTONS[1].0);
        let narrow = explorer_button_rects(modal_rect(crate::render::frame::Size::new(24, 24)));
        assert_eq!(narrow[0].2, EXPLORER_BUTTONS[0].1);
        assert_eq!(narrow[1].2, EXPLORER_BUTTONS[1].1);
    }

    #[test]
    fn explorer_glyphs_swap_between_unicode_and_ascii() {
        let mut s = explorer_state();
        let vm = s.snapshot();
        let render = |caps| {
            let mut f = Frame::new(vm.size);
            let mut b = CapsBackend { caps };
            draw(&vm, &mut b, &mut f);
            let mut out = String::new();
            for y in 0..f.height() {
                for x in 0..f.width() {
                    out.push(f.get(x, y).map_or(' ', |c| c.ch));
                }
                out.push('\n');
            }
            out
        };
        let ascii = render(crate::render::Capabilities::empty());
        let unicode = render(crate::render::Capabilities::UNICODE);
        // The cursor marker is tier-invariant; a non-cursor row carries none.
        assert!(ascii.contains("> alpha/"), "the cursor marker: {ascii}");
        assert!(
            ascii.contains("  beta/"),
            "no marker off the cursor: {ascii}"
        );
        assert!(ascii.contains("^ .."), "the ASCII back row: {ascii}");
        assert!(unicode.contains("> alpha/"), "the same marker on Unicode");
        assert!(unicode.contains("⬑ .."), "the Unicode back row");
    }

    #[test]
    fn explorer_rows_scroll_with_the_offset_and_clip_out_of_view() {
        let modal = modal_rect(crate::render::frame::Size::new(120, 40));
        let list = explorer_list_area(modal);
        assert!(list.h > 0);
        // At rest row 0 is the viewport's top…
        assert_eq!(explorer_row_rect(modal, 0, 0).y, list.y);
        // …scrolled by two, row 2 takes the top and rows 0/1 are gone whole.
        assert_eq!(explorer_row_rect(modal, 2, 2).y, list.y);
        assert_eq!(explorer_row_rect(modal, 1, 2).h, 0, "scrolled off the top");
        assert_eq!(
            explorer_row_rect(modal, usize::from(list.h) + 2, 2).h,
            0,
            "past the viewport's bottom"
        );
    }
}
