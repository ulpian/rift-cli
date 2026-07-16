//! The keymap tables the hint bar derives from (M55-04, ADR-0024 §1).
//!
//! One table per interaction context (focused panel × mode × overlay kind),
//! each row naming a key the reducer **actually handles** there. The hint bar
//! renders these tables verbatim, so a hint can never advertise a dead key —
//! and the derivation test in this module proves it by feeding every hinted
//! key through [`crate::input::on_key`] in its context and asserting the
//! reducer responds (a state change or a returned action).

use crate::dispatch::Overlay;
use crate::input::{CanopyKey, Mode};
use crate::Panel;

/// One hint-bar entry: the key's display glyph and the verb it performs. The
/// `probe` key is what the derivation test feeds the reducer (display glyphs
/// like `j/k` compress two probe-equivalent keys into one hint).
#[derive(Clone, Copy, Debug)]
pub struct Hint {
    /// The display form of the key(s) — `j/k`, `⏎`, `esc`, …
    pub keys: &'static str,
    /// The verb the key performs, lowercase and short.
    pub verb: &'static str,
    /// A representative concrete key the reducer must handle in this context.
    pub probe: CanopyKey,
}

const fn hint(keys: &'static str, verb: &'static str, probe: CanopyKey) -> Hint {
    Hint { keys, verb, probe }
}

/// The interaction context a hint table belongs to. Derived from the same
/// state the reducer dispatches on (overlay kind first, then mode, then the
/// focused panel), so the shown table always matches the live key routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintContext {
    /// Normal mode, projects rail focused.
    Rail,
    /// Normal mode, detail column focused.
    Detail,
    /// Normal mode, graphs column focused.
    Graphs,
    /// Normal mode, command row focused (not yet editing).
    CommandIdle,
    /// Editing a slash command (the suggest panel is up).
    CommandEditing,
    /// Typing an add-project path.
    AddProject,
    /// Rearranging the layout in panel-editor mode.
    PanelEditor,
    /// Browsing profiles in the profile menu's hub (ADR-0037/ADR-0044).
    ProfileMenu,
    /// A profile's settings page (ADR-0044 §3).
    ProfileSettings,
    /// Typing a profile name (create / rename / duplicate).
    ProfileNaming,
    /// The menu's inline delete confirm.
    ProfileConfirm,
    /// Picking a profile's theme colours (ADR-0039).
    ColourPicker,
    /// A results overlay is open.
    OverlayResults,
    /// A confirm overlay is open.
    OverlayConfirm,
    /// A message overlay (alert / notice / the first-run wizard) is open.
    OverlayMessage,
    /// A dispatch is running (keys are inert).
    OverlayRunning,
}

/// Derive the context from the reducer's own dispatch order: overlay kind
/// first (an open overlay owns the keys), then mode — the profile menu
/// per stage (ADR-0044 §5, so hints never advertise a dead key mid-stage) —
/// then focus.
#[must_use]
pub fn context(
    mode: Mode,
    focus: Panel,
    overlay: Option<&Overlay>,
    menu: &crate::profile_menu::MenuStage,
) -> HintContext {
    use crate::profile_menu::MenuStage;
    match overlay {
        Some(Overlay::Results { .. }) => HintContext::OverlayResults,
        Some(Overlay::Confirm { .. }) => HintContext::OverlayConfirm,
        Some(Overlay::Alert(_) | Overlay::Notice(_)) => HintContext::OverlayMessage,
        Some(Overlay::Running { .. }) => HintContext::OverlayRunning,
        None => match mode {
            Mode::Command => HintContext::CommandEditing,
            Mode::AddProject => HintContext::AddProject,
            Mode::PanelEditor => HintContext::PanelEditor,
            Mode::ProfileMenu => match menu {
                MenuStage::Browse => HintContext::ProfileMenu,
                MenuStage::Settings { .. } => HintContext::ProfileSettings,
                MenuStage::NamingCreate
                | MenuStage::NamingRename { .. }
                | MenuStage::NamingDuplicate { .. } => HintContext::ProfileNaming,
                MenuStage::ConfirmDelete { .. } => HintContext::ProfileConfirm,
            },
            Mode::ColourPicker => HintContext::ColourPicker,
            Mode::Normal => match focus {
                Panel::Projects => HintContext::Rail,
                Panel::Detail => HintContext::Detail,
                Panel::Graphs => HintContext::Graphs,
                Panel::Command => HintContext::CommandIdle,
            },
        },
    }
}

/// The per-context tables (named consts so the slices are `'static`).
const RAIL: &[Hint] = &[
    hint("j/k", "navigate", CanopyKey::Down),
    hint("⏎", "open", CanopyKey::Enter),
    hint("a", "add", CanopyKey::Char('a')),
    hint("d", "remove", CanopyKey::Char('d')),
    hint("⇧j/k", "reorder", CanopyKey::ShiftDown),
    hint("/", "palette", CanopyKey::Char('/')),
    hint("q", "quit", CanopyKey::Char('q')),
];
const DETAIL: &[Hint] = &[
    hint("j/k", "cursor", CanopyKey::Down),
    hint("⏎", "toggle", CanopyKey::Enter),
    hint("r", "retry", CanopyKey::Char('r')),
    hint("/", "palette", CanopyKey::Char('/')),
    hint("q", "quit", CanopyKey::Char('q')),
];
const GRAPHS: &[Hint] = &[
    hint("j/k", "scroll", CanopyKey::Down),
    hint("tab", "panes", CanopyKey::Tab),
    hint("/", "palette", CanopyKey::Char('/')),
    hint("q", "quit", CanopyKey::Char('q')),
];
const COMMAND_IDLE: &[Hint] = &[
    hint("/", "palette", CanopyKey::Char('/')),
    hint("tab", "panes", CanopyKey::Tab),
    hint("q", "quit", CanopyKey::Char('q')),
];
const COMMAND_EDITING: &[Hint] = &[
    hint("↑/↓", "highlight", CanopyKey::Down),
    hint("tab", "insert", CanopyKey::Tab),
    hint("⏎", "run", CanopyKey::Enter),
    hint("esc", "close", CanopyKey::Esc),
];
const ADD_PROJECT: &[Hint] = &[
    hint("⏎", "register", CanopyKey::Enter),
    hint("esc", "cancel", CanopyKey::Esc),
];
const PANEL_EDITOR: &[Hint] = &[
    hint("hjkl", "move", CanopyKey::Char('l')),
    hint("⏎", "pick/drop", CanopyKey::Enter),
    hint("<>", "resize", CanopyKey::Char('>')),
    hint("[]", "column", CanopyKey::Char(']')),
    hint("r", "reset", CanopyKey::Char('r')),
    hint("esc", "done", CanopyKey::Esc),
];
const PROFILE_MENU: &[Hint] = &[
    hint("j/k", "move", CanopyKey::Down),
    hint("⏎", "switch", CanopyKey::Enter),
    hint("s", "settings", CanopyKey::Char('s')),
    hint("n", "new", CanopyKey::Char('n')),
    hint("d", "delete", CanopyKey::Char('d')),
    hint("esc", "close", CanopyKey::Esc),
];
const PROFILE_SETTINGS: &[Hint] = &[
    hint("j/k", "move", CanopyKey::Down),
    hint("⏎", "open", CanopyKey::Enter),
    hint("esc", "back", CanopyKey::Esc),
];
const PROFILE_NAMING: &[Hint] = &[
    hint("⏎", "save", CanopyKey::Enter),
    hint("esc", "back", CanopyKey::Esc),
];
const PROFILE_CONFIRM: &[Hint] = &[
    hint("⏎", "confirm", CanopyKey::Enter),
    hint("esc", "cancel", CanopyKey::Esc),
];
const COLOUR_PICKER: &[Hint] = &[
    hint("j/k", "move", CanopyKey::Down),
    hint("⏎", "select", CanopyKey::Enter),
    hint("esc", "back", CanopyKey::Esc),
];
const OVERLAY_RESULTS: &[Hint] = &[
    hint("j/k", "scroll", CanopyKey::Down),
    hint("esc", "dismiss", CanopyKey::Esc),
];
const OVERLAY_CONFIRM: &[Hint] = &[
    hint("⏎", "confirm", CanopyKey::Enter),
    hint("esc", "cancel", CanopyKey::Esc),
];
const OVERLAY_MESSAGE: &[Hint] = &[
    hint("⏎", "action", CanopyKey::Enter),
    hint("esc", "dismiss", CanopyKey::Esc),
];

/// The hint table for a context — the focused panel's real bindings, in the
/// order they should read left to right.
#[must_use]
pub fn hints(ctx: HintContext) -> &'static [Hint] {
    match ctx {
        HintContext::Rail => RAIL,
        HintContext::Detail => DETAIL,
        HintContext::Graphs => GRAPHS,
        HintContext::CommandIdle => COMMAND_IDLE,
        HintContext::CommandEditing => COMMAND_EDITING,
        HintContext::AddProject => ADD_PROJECT,
        HintContext::PanelEditor => PANEL_EDITOR,
        HintContext::ProfileMenu => PROFILE_MENU,
        HintContext::ProfileSettings => PROFILE_SETTINGS,
        HintContext::ProfileNaming => PROFILE_NAMING,
        HintContext::ProfileConfirm => PROFILE_CONFIRM,
        HintContext::ColourPicker => COLOUR_PICKER,
        HintContext::OverlayResults => OVERLAY_RESULTS,
        HintContext::OverlayConfirm => OVERLAY_CONFIRM,
        HintContext::OverlayMessage => OVERLAY_MESSAGE,
        // Everything is deliberately inert while a dispatch runs.
        HintContext::OverlayRunning => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{on_key, Action};
    use crate::{App, EntryKind, Project, ProjectStatus};

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            path: format!("/r/{name}"),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }
    }

    /// Three live profiles (active first) — the shared profile-plane fixture.
    fn profiles_fixture(app: &mut App) {
        app.profiles = ["default", "work", "lab"]
            .iter()
            .enumerate()
            .map(|(i, name)| crate::profile::ProfileEntry {
                profile_id: format!("pf{i}"),
                name: (*name).to_string(),
                created_at: i64::try_from(i).unwrap_or(0),
            })
            .collect();
        app.active_profile = "pf0".into();
    }

    /// A mid-state app for `ctx`: three projects with the middle one selected,
    /// cursors off their rails' edges — so every direction key has room to
    /// move and every action key has a live target.
    fn app_in(ctx: HintContext) -> App {
        let mut app = App::new(vec![project("a"), project("b"), project("c")]);
        app.splash.skip();
        app.selected = 1;
        match ctx {
            HintContext::Rail => app.set_focus(Panel::Projects),
            HintContext::Detail => {
                app.set_focus(Panel::Detail);
                app.toggle_cursor = 1;
            }
            HintContext::Graphs => {
                app.set_focus(Panel::Graphs);
                app.scroll.graphs = 2;
            }
            HintContext::CommandIdle => app.set_focus(Panel::Command),
            HintContext::CommandEditing => {
                app.set_focus(Panel::Command);
                app.mode = Mode::Command;
                app.suggest_highlight = 1;
            }
            HintContext::AddProject => {
                app.mode = Mode::AddProject;
                app.command_line = "/tmp/x".into();
            }
            HintContext::PanelEditor => {
                app.mode = Mode::PanelEditor;
                // A NON-default layout so `r` (reset) observably changes it, with
                // the cursor mid-grid (a right neighbor for resize / column move).
                app.layout.columns.swap(0, 1);
                app.editor.cursor = crate::editor::Cursor { col: 1, unit: 0 };
            }
            HintContext::ProfileMenu => {
                // Three live profiles, active first, cursor mid-list on a
                // NON-active row — so `d` can open the confirm, `s` its
                // settings, `Enter` can switch, and `j`/`k` have room.
                profiles_fixture(&mut app);
                app.mode = Mode::ProfileMenu;
                app.profile_menu.cursor = 1;
            }
            HintContext::ProfileSettings => {
                // The settings page for a NON-active profile, cursor mid-list
                // — `j`/`k` have room, `Enter` opens the cursor row, `Esc`
                // steps back to Browse.
                profiles_fixture(&mut app);
                app.mode = Mode::ProfileMenu;
                app.profile_menu.stage = crate::profile_menu::MenuStage::Settings {
                    profile: "pf1".into(),
                };
                app.profile_menu.settings_cursor = 1;
            }
            HintContext::ProfileNaming => {
                // A naming input holding a VALID name, so `Enter` emits the
                // typed action and `Esc` observably steps back.
                profiles_fixture(&mut app);
                app.mode = Mode::ProfileMenu;
                app.profile_menu.stage = crate::profile_menu::MenuStage::NamingCreate;
                app.command_line = "qa".into();
            }
            HintContext::ProfileConfirm => {
                // The inline confirm on a NON-active profile — `Enter` lands
                // the delete (a typed action), `Esc` cancels (a stage move).
                profiles_fixture(&mut app);
                app.mode = Mode::ProfileMenu;
                app.profile_menu.stage = crate::profile_menu::MenuStage::ConfirmDelete {
                    profile: "pf1".into(),
                    back_to_settings: false,
                };
            }
            HintContext::ColourPicker => {
                // The preset stage mid-list: `j`/`k` have room, `Enter`
                // applies (a typed action), `Esc` steps back to the slots.
                app.profiles = vec![crate::profile::ProfileEntry {
                    profile_id: "pf0".into(),
                    name: "default".into(),
                    created_at: 0,
                }];
                app.active_profile = "pf0".into();
                app.mode = Mode::ColourPicker;
                app.colour_picker.stage = crate::colour_picker::PickerStage::Presets;
                app.colour_picker.slot_cursor = 1;
                app.colour_picker.preset_cursor = 2;
            }
            HintContext::OverlayResults => {
                app.overlay = Some(Overlay::Results {
                    title: "t".into(),
                    lines: (0..10).map(|i| format!("line {i}")).collect(),
                    scroll: 3,
                    footer: None,
                    truncated: false,
                });
            }
            HintContext::OverlayConfirm => {
                app.overlay = Some(Overlay::Confirm {
                    request: crate::dispatch::DispatchRequest::Rebuild,
                    repo: "/r/b".into(),
                });
            }
            HintContext::OverlayMessage => {
                // The wizard message: a Primary + Close ring, so Enter has an
                // observable action (enter add-project mode) to prove.
                app.overlay = Some(Overlay::wizard());
            }
            HintContext::OverlayRunning => {
                app.overlay = Some(Overlay::Running { title: "x".into() });
            }
        }
        app
    }

    /// The observable fingerprint a "handled" key must move (or it must return
    /// a non-`None` action).
    fn fingerprint(app: &App) -> String {
        format!(
            "{:?}|{:?}|{}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}",
            app.focus,
            app.mode,
            app.selected,
            app.toggle_cursor,
            app.scroll.graphs,
            app.command_line,
            app.suggest_highlight,
            app.quit,
            app.overlay.as_ref().map(|o| match o {
                Overlay::Results { scroll, .. } => format!("results@{scroll}"),
                Overlay::Confirm { .. } => "confirm".into(),
                Overlay::Running { .. } => "running".into(),
                Overlay::Alert(_) => "alert".into(),
                Overlay::Notice(_) => "notice".into(),
            }),
            // The panel-editor cursor/pickup and the layout arrangement — so an
            // editor key (move/reorder/resize/reset) registers as handled.
            app.editor,
            app.layout,
        ) + &format!("|{:?}|{:?}", app.profile_menu, app.colour_picker)
    }

    /// The M55-04 derivation proof: every hinted key, fed to the reducer in
    /// its own context, is actually handled — the state observably changes or
    /// an action is returned. A hint can never advertise a dead key.
    #[test]
    fn every_hint_key_is_handled_by_the_reducer() {
        const ALL: [HintContext; 16] = [
            HintContext::Rail,
            HintContext::Detail,
            HintContext::Graphs,
            HintContext::CommandIdle,
            HintContext::CommandEditing,
            HintContext::AddProject,
            HintContext::PanelEditor,
            HintContext::ProfileMenu,
            HintContext::ProfileSettings,
            HintContext::ProfileNaming,
            HintContext::ProfileConfirm,
            HintContext::ColourPicker,
            HintContext::OverlayResults,
            HintContext::OverlayConfirm,
            HintContext::OverlayMessage,
            HintContext::OverlayRunning,
        ];
        for ctx in ALL {
            for h in hints(ctx) {
                let mut app = app_in(ctx);
                let before = fingerprint(&app);
                let action = on_key(&mut app, h.probe);
                let handled = action != Action::None || fingerprint(&app) != before;
                assert!(
                    handled,
                    "{ctx:?}: hinted key {:?} (`{}` {}) is dead in the reducer",
                    h.probe, h.keys, h.verb
                );
            }
        }
    }

    #[test]
    fn context_follows_the_reducer_dispatch_order() {
        // Overlay kind outranks mode and focus (an open overlay owns the keys).
        let results = Overlay::Results {
            title: "t".into(),
            lines: vec![],
            scroll: 0,
            footer: None,
            truncated: false,
        };
        assert_eq!(
            context(
                Mode::Command,
                Panel::Projects,
                Some(&results),
                &crate::profile_menu::MenuStage::Browse
            ),
            HintContext::OverlayResults
        );
        // Mode outranks focus.
        assert_eq!(
            context(
                Mode::Command,
                Panel::Projects,
                None,
                &crate::profile_menu::MenuStage::Browse
            ),
            HintContext::CommandEditing
        );
        assert_eq!(
            context(
                Mode::AddProject,
                Panel::Graphs,
                None,
                &crate::profile_menu::MenuStage::Browse
            ),
            HintContext::AddProject
        );
        // Focus routes Normal mode.
        assert_eq!(
            context(
                Mode::Normal,
                Panel::Projects,
                None,
                &crate::profile_menu::MenuStage::Browse
            ),
            HintContext::Rail
        );
        assert_eq!(
            context(
                Mode::Normal,
                Panel::Detail,
                None,
                &crate::profile_menu::MenuStage::Browse
            ),
            HintContext::Detail
        );
        assert_eq!(
            context(
                Mode::Normal,
                Panel::Graphs,
                None,
                &crate::profile_menu::MenuStage::Browse
            ),
            HintContext::Graphs
        );
        assert_eq!(
            context(
                Mode::Normal,
                Panel::Command,
                None,
                &crate::profile_menu::MenuStage::Browse
            ),
            HintContext::CommandIdle
        );
    }

    #[test]
    fn running_overlay_hints_nothing() {
        assert!(hints(HintContext::OverlayRunning).is_empty());
    }
}
