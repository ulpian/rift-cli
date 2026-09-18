//! The modal geometry every centered surface shares: where the box sits,
//! the scrim behind it, the close `[x]` and the primary chip.
//!
//! One solver feeds both the draw and the pure hit-test in `crate::hit`, so a
//! resize can never desync a modal's pixels from its click targets; the
//! functions keep their names because `hit.rs` and the parity tests call
//! them.

use crate::layout::{bottom_chrome_rows, choose_mode, LayoutMode, Rect};
use crate::render::frame::{Frame, Style};
use crate::state::ViewModel;

/// The smallest readable modal box: a modal clamps *up* to this even
/// when 80% of a small terminal would be fewer cells.
pub const MIN_MODAL_COLS: u16 = 40;

/// The smallest readable modal box height — see [`MIN_MODAL_COLS`].
pub const MIN_MODAL_ROWS: u16 = 12;

/// The confirm overlay's two button labels (shared with hit-testing).
pub const CONFIRM_BUTTONS: (&str, &str) = ("[ Enter: confirm ]", "[ Esc: cancel ]");

/// An open modal's frame, handed to the helpers a surface draws its regions
/// through: the modal box the row solvers take, the inner box the rows clip
/// to, and the glyph tier (Unicode or the ASCII floor).
#[derive(Clone, Copy)]
pub(super) struct ModalFrame {
    pub(super) rect: Rect,
    pub(super) inner: Rect,
    pub(super) unicode: bool,
}

/// Where a modal sits: a box at 80% × 80% of the terminal, clamped
/// *up* to the readable minimum and *down* to the region between the 1-row
/// header and the bottom chrome (hint + command bars) — so those bars always
/// stay visible while modals come and go — then centered in that region. On the
/// narrow `SinglePanel`/`TooSmall` tiers it degrades to near-full-screen so its
/// content stays usable. This is the single source of truth for both
/// `draw_overlay` and the pure hit-test, so a resize can never desync a
/// modal's pixels from its click targets.
#[must_use]
pub fn modal_rect(size: crate::render::frame::Size) -> Rect {
    // The region a modal may occupy: below the header, above the bottom chrome.
    let body_h = size.h.saturating_sub(1);
    let avail_y = 1u16.min(size.h);
    let avail_h = body_h.saturating_sub(bottom_chrome_rows(body_h));
    let avail_w = size.w;

    // Target = 80% of the terminal (95% near-full on the narrow tiers), clamped
    // up to the readable minimum and down to the region so chrome stays put.
    let pct: u32 = if matches!(
        choose_mode(size),
        LayoutMode::SinglePanel | LayoutMode::TooSmall
    ) {
        95
    } else {
        80
    };
    let want_w = u16::try_from(u32::from(size.w) * pct / 100).unwrap_or(size.w);
    let want_h = u16::try_from(u32::from(size.h) * pct / 100).unwrap_or(size.h);
    let w = want_w.max(MIN_MODAL_COLS).min(avail_w);
    let h = want_h.max(MIN_MODAL_ROWS).min(avail_h);

    // Center within the available region.
    let x = avail_w.saturating_sub(w) / 2;
    let y = avail_y.saturating_add(avail_h.saturating_sub(h) / 2);
    Rect { x, y, w, h }
}

/// The clickable close `[x]` on a modal's top-right border — one
/// shared source for the draw and the hit-test, so the glyph and its click
/// target are the same cells. `None` when the modal is too narrow to host it.
#[must_use]
pub fn modal_close_rect(modal: Rect) -> Option<Rect> {
    (modal.w >= 8).then(|| Rect {
        x: modal.x + modal.w - 4,
        y: modal.y,
        w: 3,
        h: 1,
    })
}

/// The primary-action button rect on a message modal — bottom-left of
/// the modal's inner content, shared by `draw_overlay` and the hit-test so the
/// chip cells and the click target are the same cells. `None` for a dismiss-only
/// message or a modal too small to host it.
#[must_use]
pub fn modal_primary_rect(inner: Rect, action: crate::dispatch::MessageAction) -> Option<Rect> {
    let chip = primary_chip(action)?;
    if inner.w == 0 || inner.h == 0 {
        return None;
    }
    let w = u16::try_from(chip.chars().count())
        .unwrap_or(0)
        .min(inner.w);
    Some(Rect {
        x: inner.x,
        y: inner.y + inner.h - 1,
        w,
        h: 1,
    })
}

/// Open a modal's box: the scrim over the dashboard, then the box blanked to
/// the surface background so the panels underneath never bleed through. Every
/// modal surface starts here, after its own size floor, so the four mode
/// modals and the overlay family cannot drift on how a modal sits on the body.
pub(super) fn draw_modal_backdrop(vm: &ViewModel, frame: &mut Frame, modal: Rect) {
    draw_scrim(frame, modal);
    crate::chrome::fill(frame, modal, Style::DEFAULT.with_bg(vm.theme.surface_bg()));
}

/// The close `[x]` on a mode modal's top-right border, the pointer twin of
/// `Esc` — `target` is the MODE's own close, never an overlay's. Absent when
/// the modal is too narrow to host it (the hit-test agrees, by construction).
pub(super) fn draw_mode_close(
    vm: &ViewModel,
    frame: &mut Frame,
    modal: Rect,
    target: &crate::hit::Target,
) {
    let Some(close) = modal_close_rect(modal) else {
        return;
    };
    frame.put_str(close.x, close.y, "[x]", mode_chip_style(vm, target));
}

/// The style of a muted chip on a mode modal — the close `[x]`, a row's
/// `[settings]`, the explorer's buttons: muted at rest, the press-flash
/// background while `target` is pressed, so a click is acknowledged on every
/// chip the same way (modal-surface parity).
pub(super) fn mode_chip_style(vm: &ViewModel, target: &crate::hit::Target) -> Style {
    let bg = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(target))
        .unwrap_or(vm.theme.surface_bg());
    Style::fg(crate::theme::MUTED).with_bg(bg)
}

/// The primary-action chip string for a message modal (`[ Add a project ]`), or
/// `None` for a dismiss-only message. One source for draw + measurement, so the
/// drawn cells and the hit-test width agree.
pub(super) fn primary_chip(action: crate::dispatch::MessageAction) -> Option<String> {
    action.primary_label().map(|label| format!("[ {label} ]"))
}

/// Dim the dashboard behind an open modal: a themed background wash
/// over every cell outside `modal`, glyphs kept so the body reads as *behind*
/// the modal rather than gone. Colour-only, so the capability tiers drop it to
/// no-dim on mono / ASCII / `NO_COLOR` — there the modal's border alone
/// separates it. The modal owns input while open, so the dimmed body is inert.
fn draw_scrim(frame: &mut Frame, modal: Rect) {
    let area = frame.rect();
    let modal_bottom = modal.y.saturating_add(modal.h);
    let modal_right = modal.x.saturating_add(modal.w);
    for y in area.y..area.y.saturating_add(area.h) {
        let in_modal_rows = y >= modal.y && y < modal_bottom;
        for x in area.x..area.x.saturating_add(area.w) {
            if in_modal_rows && x >= modal.x && x < modal_right {
                continue; // the modal paints its own rect below
            }
            if let Some(cell) = frame.get(x, y) {
                frame.set(x, y, cell.ch, cell.style.with_bg(crate::theme::SCRIM_BG));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;

    #[test]
    fn primary_rect_is_bottom_left_only_for_an_action() {
        use crate::dispatch::MessageAction;
        let inner = Rect {
            x: 4,
            y: 2,
            w: 30,
            h: 8,
        };
        let pr = modal_primary_rect(inner, MessageAction::AddProject).expect("has a primary");
        assert_eq!(
            pr.y,
            inner.y + inner.h - 1,
            "the chip sits on the bottom row"
        );
        assert_eq!(pr.x, inner.x, "left-aligned");
        assert!(modal_primary_rect(inner, MessageAction::Dismiss).is_none());
    }

    #[test]
    fn modal_is_centered_and_insets_the_dashboard() {
        // At a full-grid size the modal is 80% × 80%, centered, and clear of the
        // 1-row header and the bottom chrome (the shared modal geometry) — an inset box, not the
        // whole screen.
        let r = modal_rect(Size::new(120, 40));
        assert_eq!((r.x, r.y, r.w, r.h), (12, 2, 96, 32));
    }

    #[test]
    fn modal_degrades_to_near_full_on_narrow_tiers() {
        // On the narrow SinglePanel tier the modal widens past the 80% grid
        // fraction toward near-full-screen so its content stays usable.
        let size = Size::new(44, 30); // SinglePanel: MIN_COL1 <= w < MIN_COL1+MIN_COL2
        assert_eq!(
            crate::layout::choose_mode(size),
            crate::layout::LayoutMode::SinglePanel
        );
        let r = modal_rect(size);
        assert!(r.w > size.w * 80 / 100, "wider than the 80% grid modal");
    }

    proptest::proptest! {
        /// The modal rect is always within the frame, never covers the
        /// header row, and clears the bottom chrome — at every size above the
        /// too-small floor — so the header and command bars always stay put and
        /// the draw/hit-test share one rect that can never leave the screen.
        #[test]
        fn modal_rect_fits_and_clears_chrome(w in 24u16..400, h in 6u16..400) {
            let size = Size::new(w, h);
            proptest::prop_assume!(
                crate::layout::choose_mode(size) != crate::layout::LayoutMode::TooSmall
            );
            let r = modal_rect(size);
            proptest::prop_assert!(r.x + r.w <= size.w, "within the frame width");
            proptest::prop_assert!(r.y >= 1, "never covers the header row");
            let chrome = crate::layout::bottom_chrome_rows(size.h - 1);
            proptest::prop_assert!(r.y + r.h <= size.h - chrome, "clears the bottom chrome");
        }
    }
}
