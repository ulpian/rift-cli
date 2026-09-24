//! Overlay keys: an open modal owns every key and its focus ring.

use super::{enter_explorer, Action, CanopyKey};
use crate::App;

/// Keys while an overlay is open — the modal owns them and its focus ring.
/// Tab / Shift-Tab move focus across the modal's controls;
/// Enter / Space activate the focused control (a mutating Confirm needs Enter —
/// Space never confirms it, so a buffered keystroke can't); Esc always
/// dismisses a Results/Confirm; `j`/`k`/arrows and the wheel scroll a Results
/// body regardless of focus. Running swallows everything.
pub(super) fn on_key_overlay(app: &mut App, key: CanopyKey) -> Action {
    use crate::dispatch::{Overlay, OverlayControl};
    // Focus-ring traversal is available in every modal that has controls.
    match key {
        CanopyKey::Tab => {
            app.cycle_overlay_focus(true);
            return Action::None;
        }
        CanopyKey::BackTab => {
            app.cycle_overlay_focus(false);
            return Action::None;
        }
        _ => {}
    }
    // Results keeps its j/k (and arrow) scroll whichever control is focused.
    if let Some(Overlay::Results { scroll, lines, .. }) = app.overlay.as_mut() {
        match key {
            CanopyKey::Char('j') | CanopyKey::Down => {
                *scroll = scroll.saturating_add(1).min(lines.len().saturating_sub(1));
                return Action::None;
            }
            CanopyKey::Char('k') | CanopyKey::Up => {
                *scroll = scroll.saturating_sub(1);
                return Action::None;
            }
            _ => {}
        }
    }
    // A normalized hard quit (Ctrl-C) exits the app even from a modal — an
    // open message must never trap the escape hatch.
    if matches!(key, CanopyKey::Quit) {
        app.quit = true;
        return Action::Quit;
    }
    // Esc closes/cancels a dismissable modal — results, confirm,
    // alert, notice, the wizard; only Running swallows it.
    if matches!(key, CanopyKey::Esc) {
        if !matches!(app.overlay, Some(Overlay::Running { .. })) {
            app.overlay = None;
            app.overlay_focus = 0;
        }
        return Action::None;
    }
    // Enter / Space activate the focused control. Space never activates the
    // mutating Confirm — only Enter does — so a stray Space can't run it.
    if matches!(key, CanopyKey::Enter | CanopyKey::Char(' ')) {
        let is_space = matches!(key, CanopyKey::Char(' '));
        return match app.focused_overlay_control() {
            Some(OverlayControl::Confirm) if !is_space => Action::ConfirmDispatch,
            Some(OverlayControl::Cancel | OverlayControl::Close) => {
                app.overlay = None;
                app.overlay_focus = 0;
                Action::None
            }
            // The wizard's *add a project* action: dismiss the modal
            // and raise the destination explorer (the `a` key's effect, from
            // the wizard — the destination explorer: one door, three affordances).
            Some(OverlayControl::Primary) => enter_explorer(app),
            _ => Action::None,
        };
    }
    Action::None
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{project, ready};
    use super::super::{on_key, Mode};
    use super::*;

    #[test]
    fn wizard_primary_enter_raises_the_explorer() {
        // the destination explorer: the newest operator's first act is browsing, not typing
        // an absolute path blind — the wizard primary arrives through the same
        // `enter_explorer` door as `a` and the rail `+`.
        let mut app = ready(vec![project("a")]);
        app.home_dir = Some("/home/dev".into());
        app.overlay = Some(crate::dispatch::Overlay::wizard()); // focus = Primary
        assert_eq!(
            on_key(&mut app, CanopyKey::Enter),
            Action::ExplorerList(std::path::PathBuf::from("/home/dev")),
            "Enter fired the wizard action: list the start directory"
        );
        assert_eq!(app.mode, Mode::Explorer);
        assert!(app.overlay.is_none(), "the wizard dismissed");
    }

    #[test]
    fn esc_dismisses_an_alert_and_a_notice() {
        for ov in [
            crate::dispatch::Overlay::alert(
                "e",
                vec!["x".into()],
                crate::dispatch::Severity::Error,
            ),
            crate::dispatch::Overlay::notice("ok", vec!["done".into()]),
        ] {
            let mut app = ready(vec![project("a")]);
            app.overlay = Some(ov);
            assert_eq!(on_key(&mut app, CanopyKey::Esc), Action::None);
            assert!(app.overlay.is_none(), "Esc closes every message modal");
        }
    }

    #[test]
    fn ctrl_c_quits_even_from_a_modal() {
        // A hard quit must never be trapped by an open message (the escape hatch).
        let mut app = ready(vec![project("a")]);
        app.overlay = Some(crate::dispatch::Overlay::wizard());
        assert_eq!(on_key(&mut app, CanopyKey::Quit), Action::Quit);
        assert!(app.quit, "Ctrl-C sets quit even with a modal open");
    }
}
