//! The command palette's verbs: parsing one submitted line, routing it by
//! tier, and the dashboard-tier and `/profile` flows that run in-process.
//!
//! Separate so the palette can be read as a grammar over the panels' own
//! effects — a verb never reaches a capability a panel lacks — and so the
//! confirm-before-mutate rule for delegated verbs is decided in one place.

use crate::command::{PanelName, ProfileVerb, ProjectRef, Verb};
use crate::dispatch::{Message, Overlay};
use crate::profile::normalize_profile_name;
use crate::recall_viewer::{Surface, ViewerPane};
use crate::{App, EntryKind, Mode, Panel, Project, ProjectStatus};

use super::actions::{profile_persist_flow, remove_project_op};
use super::effects::persist_flow;
use super::openers::open_project_settings;
use super::reduce::selection_change_effects;
use super::viewer::viewer_command_allowed;
use super::{AppState, Effects, Flow, PersistOp};

/// The soft warning for a `/profile` name that fails validation.
const NAME_RULE: &str = "name must be 1–24 chars and unique";

/// The soft warning for a `/profile` name no live profile carries.
const NO_SUCH_PROFILE: &str = "no profile by that name";

/// Submit one palette buffer: parse; a span error renders inline
/// (buffer + mode kept so the underline points at the typed text); a verb
/// routes by tier — dashboard verbs to typed panel effects, read-only
/// delegation straight to the dispatcher behind a Running frame, mutating
/// delegation to the confirm overlay.
pub(super) fn run_line(s: &mut AppState, buf: &str) -> Flow {
    match crate::command::parse(buf) {
        Err(e) => {
            // An incomplete line accepts the highlighted suggestion first
            // (Enter runs, accepting the highlight when the line is
            // incomplete); only if THAT parses does it run.
            let suggestions = crate::current_suggestions(&s.app);
            if let Some(sug) = suggestions.get(
                s.app
                    .suggest_highlight
                    .min(suggestions.len().saturating_sub(1)),
            ) {
                if crate::command::parse(&sug.insert).is_ok() {
                    let line = sug.insert.clone();
                    s.app.command_line.clone_from(&line);
                    return run_line(s, &line);
                }
            }
            s.app.command_error = Some(e);
            Flow::Continue(Effects::default())
        }
        Ok(verb) => {
            s.app.command_error = None;
            s.app.command_line.clear();
            s.app.mode = Mode::Normal;
            if s.app.surface == Surface::RecallViewer && !viewer_command_allowed(&verb) {
                s.open_alert(Message::warning(
                    "Recall Viewer is read-only",
                    vec![
                        "allowed: viewer, select, focus, help, quit, status, checkpoint list/show, session list/show, explain, doctor, projects list, gc --dry-run".into(),
                    ],
                ));
                return Flow::Continue(Effects::default());
            }
            // Frecency: a dashboard verb executes right here (bump now); a
            // delegated verb bumps only when its dispatch succeeds.
            if crate::dispatch::request_for(&verb).is_none() {
                s.app.frecency.bump(verb_word_of(&verb));
            }
            match crate::dispatch::request_for(&verb) {
                Some(req) => {
                    let repo = s
                        .app
                        .selected_project()
                        .map(|p| p.path.clone())
                        .unwrap_or_default();
                    if req.is_mutating() {
                        s.app.open_overlay(Overlay::Confirm { request: req, repo });
                        Flow::Continue(Effects::default())
                    } else {
                        s.app.open_overlay(Overlay::running(req.verb_line()));
                        Flow::Continue(Effects {
                            dispatches: vec![(req, repo)],
                            ..Effects::default()
                        })
                    }
                }
                None => dashboard_verb_flow(s, verb),
            }
        }
    }
}

/// The confirm overlay's Enter/Confirm: swap Confirm → Running and emit the
/// pending dispatch with the repo resolved at confirm time.
pub(super) fn confirm_dispatch(s: &mut AppState) -> Flow {
    let Some(Overlay::Confirm { request, repo }) = s.app.overlay.take() else {
        return Flow::Continue(Effects::default());
    };
    s.app.open_overlay(Overlay::running(request.verb_line()));
    Flow::Continue(Effects {
        dispatches: vec![(request, repo)],
        ..Effects::default()
    })
}

/// A dashboard-tier verb dispatches to the SAME typed effects the panels
/// emit — the palette can never exceed a panel's privilege (the injected
/// dispatcher). Delegable verbs never reach here: `request_for` handled them.
pub(super) fn dashboard_verb_flow(s: &mut AppState, verb: Verb) -> Flow {
    match verb {
        Verb::Quit => Flow::Quit,
        Verb::Help => open_help(s),
        Verb::Focus(panel) => {
            focus_panel(s, panel);
            Flow::Continue(Effects::default())
        }
        Verb::Viewer(args) => {
            if let Some(repo) = args.repo {
                select_or_stage_repository(s, repo);
            }
            Flow::Continue(s.open_recall_viewer(args.target))
        }
        Verb::Select(r) => select_verb(s, &r),
        Verb::Add => add_verb(s),
        Verb::Remove(r) => {
            let Some(path) = resolve_project_ref(&s.app, &r) else {
                return no_match_status(s);
            };
            persist_flow(Some(remove_project_op(&s.app, path)))
        }
        Verb::Relabel(r, label) => {
            let Some(path) = resolve_project_ref(&s.app, &r) else {
                return no_match_status(s);
            };
            persist_flow(Some(PersistOp::RelabelProject { path, label }))
        }
        Verb::Settings => open_project_settings(s),
        // The /profile family: the same typed effects the menu
        // emits — two surfaces, one grammar.
        Verb::Profile(pv) => profile_verb_flow(s, pv),
        _ => Flow::Continue(Effects::default()),
    }
}

/// The "no matching project" alert (shared by the project-ref verbs) — a soft
/// miss, so a warning Alert.
pub(super) fn no_match_status(s: &mut AppState) -> Flow {
    s.open_alert(Message::warning(
        "no match",
        vec!["no matching project".into()],
    ));
    Flow::Continue(Effects::default())
}

/// Apply one `/profile` palette verb. Names resolve against the
/// live profiles; a miss or a refused name/guard is a soft warning Alert
/// (the message-routing rule). Every accepted act is the same op + reload the menu emits.
fn profile_verb_flow(s: &mut AppState, pv: ProfileVerb) -> Flow {
    if s.app.profiles.is_empty() {
        return profile_warning(s, "the profile plane is not wired in this run");
    }
    match pv {
        ProfileVerb::New(raw) => match normalize_profile_name(&raw, &s.app.profiles, None) {
            Some(name) => profile_persist_flow(PersistOp::CreateProfile { name }),
            None => profile_warning(s, NAME_RULE),
        },
        ProfileVerb::Rename(raw) => {
            let active = s.app.active_profile.clone();
            match normalize_profile_name(&raw, &s.app.profiles, Some(&active)) {
                Some(name) => profile_persist_flow(PersistOp::RenameProfile {
                    profile: active,
                    name,
                }),
                None => profile_warning(s, NAME_RULE),
            }
        }
        ProfileVerb::Switch(name) => match profile_id_by_name(&s.app, &name) {
            Some(id) if id == s.app.active_profile => Flow::Continue(Effects::default()),
            Some(id) => profile_persist_flow(PersistOp::SelectProfile { profile: id }),
            None => profile_warning(s, NO_SUCH_PROFILE),
        },
        ProfileVerb::Delete(name) => delete_profile_verb(s, &name),
        // `/profile duplicate <new-name>` clones the ACTIVE profile whole —
        // parallel to `/profile new`.
        ProfileVerb::Duplicate(raw) => match normalize_profile_name(&raw, &s.app.profiles, None) {
            Some(name) => profile_persist_flow(PersistOp::DuplicateProfile {
                source: s.app.active_profile.clone(),
                name,
            }),
            None => profile_warning(s, NAME_RULE),
        },
        // `/profile startup <name>` pins; the bare verb unpins (launch
        // follows last-selected again) — never a warn.
        ProfileVerb::Startup(None) => {
            profile_persist_flow(PersistOp::SetStartupProfile { profile: None })
        }
        ProfileVerb::Startup(Some(name)) => match profile_id_by_name(&s.app, &name) {
            Some(id) => profile_persist_flow(PersistOp::SetStartupProfile { profile: Some(id) }),
            None => profile_warning(s, NO_SUCH_PROFILE),
        },
    }
}

/// `/profile delete <name>`: the removal guards refuse here first — and
/// again in the store.
fn delete_profile_verb(s: &mut AppState, name: &str) -> Flow {
    match profile_id_by_name(&s.app, name) {
        Some(id) if id == s.app.active_profile => {
            profile_warning(s, "cannot delete the active profile")
        }
        Some(_) if s.app.profiles.len() <= 1 => {
            profile_warning(s, "cannot delete the last profile")
        }
        Some(id) => profile_persist_flow(PersistOp::RemoveProfile { profile: id }),
        None => profile_warning(s, NO_SUCH_PROFILE),
    }
}

/// A `/profile` soft miss or refusal: a warning Alert, nothing persisted.
fn profile_warning(s: &mut AppState, body: &str) -> Flow {
    s.open_alert(Message::warning("profiles", vec![body.to_string()]));
    Flow::Continue(Effects::default())
}

/// The stable id of the live profile called `name`.
fn profile_id_by_name(app: &App, name: &str) -> Option<String> {
    app.profiles
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.profile_id.clone())
}

/// `/help`: the grammar as a read-only results overlay.
fn open_help(s: &mut AppState) -> Flow {
    let lines: Vec<String> = crate::command::help_lines();
    s.app.open_overlay(Overlay::Results {
        title: "palette".into(),
        lines,
        scroll: 0,
        footer: None,
        truncated: false,
    });
    Flow::Continue(Effects::default())
}

/// `/focus <panel>`: the dashboard column, or the viewer pane it holds.
fn focus_panel(s: &mut AppState, panel: PanelName) {
    match s.app.surface {
        Surface::RecallViewer => {
            s.app.viewer.pane = match panel {
                PanelName::Projects => ViewerPane::Projects,
                PanelName::Detail | PanelName::Checkpoints => ViewerPane::Checkpoints,
                PanelName::Graphs | PanelName::Timeline => ViewerPane::Timeline,
                PanelName::Command => ViewerPane::Command,
            };
        }
        Surface::Dashboard => s.app.set_focus(match panel {
            PanelName::Projects => Panel::Projects,
            PanelName::Detail | PanelName::Checkpoints => Panel::Detail,
            PanelName::Graphs | PanelName::Timeline => Panel::Graphs,
            PanelName::Command => Panel::Command,
        }),
    }
}

/// `/viewer --repo <path>`: select the registered row for it, or stage a
/// session-only row. Keeps the reducer pure: repository discovery and
/// canonicalisation belong to the viewer data worker, so a palette deep link
/// is a transient row until that worker reports whether it can be opened,
/// and is never written to the project registry.
fn select_or_stage_repository(s: &mut AppState, path: String) {
    if let Some(index) = s
        .app
        .projects
        .iter()
        .position(|project| project.path == path)
    {
        s.app.selected = index;
        return;
    }
    let name = std::path::Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("transient repository")
        .to_owned();
    s.app.projects.push(Project {
        path,
        name,
        status: ProjectStatus::Idle,
        kind: EntryKind::Repo,
    });
    s.app.selected = s.app.projects.len().saturating_sub(1);
}

/// `/select <ref>`: move the selection with the bus effects a cursor move
/// owes — on the viewer surface, open the project's catalogue instead.
fn select_verb(s: &mut AppState, r: &ProjectRef) -> Flow {
    let before = s.app.selected_project().map(|p| p.path.clone());
    let Some(path) = resolve_project_ref(&s.app, r) else {
        return no_match_status(s);
    };
    s.app.select_path(&path);
    let mut eff = Effects::default();
    selection_change_effects(s, &before, &mut eff);
    Flow::Continue(eff)
}

/// `/add`: register the launch directory; with none known, an error Alert.
fn add_verb(s: &mut AppState) -> Flow {
    let Some(path) = s.app.launch_cwd.clone() else {
        s.open_alert(Message::error(
            "cannot add project",
            vec!["launch directory unknown — run `rift init` there".into()],
        ));
        return Flow::Continue(Effects::default());
    };
    Flow::Continue(Effects {
        persist: vec![PersistOp::AddProject { path }],
        ..Effects::default()
    })
}

/// Resolve a grammar project reference against the current rows.
fn resolve_project_ref(app: &App, r: &ProjectRef) -> Option<String> {
    match r {
        ProjectRef::Selected => app.selected_project().map(|p| p.path.clone()),
        ProjectRef::Index(n) => app
            .projects
            .get(n.saturating_sub(1))
            .map(|p| p.path.clone()),
        ProjectRef::Name(name) => app
            .projects
            .iter()
            .find(|p| p.name == *name || p.path == *name)
            .map(|p| p.path.clone()),
    }
}

/// The catalog word a dashboard verb bumps in the frecency log.
fn verb_word_of(verb: &Verb) -> &'static str {
    match verb {
        Verb::Select(_) => "select",
        Verb::Add => "add",
        Verb::Remove(_) => "remove",
        Verb::Relabel(..) => "relabel",
        Verb::Settings => "settings",
        Verb::Viewer(_) => "viewer",
        Verb::Focus(_) => "focus",
        Verb::Profile(_) => "profile",
        Verb::Help => "help",
        Verb::Quit => "quit",
        // Delegated verbs bump on DispatchDone, not here.
        _ => "",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{DispatchRequest, Severity};
    use crate::input::CanopyKey;
    use crate::state::fixtures::*;
    use crate::state::{update, Event};

    /// The two typed palette verbs the family view adds, parsed and routed:
    /// `/worktrees` reads in process, `/adopt` mutates and therefore confirms.
    #[test]
    fn the_palette_routes_worktrees_and_adopt_to_their_tiers() {
        use crate::command::{parse, Tier, Verb};
        assert_eq!(parse("/worktrees"), Ok(Verb::Worktrees));
        assert_eq!(parse("/adopt"), Ok(Verb::Adopt));
        assert_eq!(Verb::Worktrees.tier(), Tier::ReadOnly);
        assert_eq!(Verb::Adopt.tier(), Tier::Mutating);
        assert_eq!(
            crate::dispatch::request_for(&Verb::Worktrees),
            Some(DispatchRequest::Worktrees)
        );
        assert_eq!(
            crate::dispatch::request_for(&Verb::Adopt),
            Some(DispatchRequest::Adopt)
        );

        // `/worktrees` runs at once; `/adopt` stops at the confirm.
        let mut s = ready(vec![project("a")]);
        let e = effects_of(run_line(&mut s, "/worktrees"));
        assert_eq!(
            e.dispatches,
            vec![(DispatchRequest::Worktrees, "/r/a".to_owned())]
        );
        let mut s = ready(vec![project("a")]);
        let e = effects_of(run_line(&mut s, "/adopt"));
        assert!(e.dispatches.is_empty(), "a mutation asks first");
        assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        // …and the confirm's Enter is what dispatches it.
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.dispatches,
            vec![(DispatchRequest::Adopt, "/r/a".to_owned())]
        );
    }

    /// Type a slash line in command mode and press Enter.
    fn run_palette(s: &mut AppState, line: &str) -> Effects {
        update(s, Event::Key(CanopyKey::Char('/')));
        for c in line.chars() {
            update(s, Event::Key(CanopyKey::Char(c)));
        }
        effects_of(update(s, Event::Key(CanopyKey::Enter)))
    }

    #[test]
    fn profile_palette_verbs_map_onto_the_menu_ops() {
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile switch lab");
        assert_eq!(
            e.persist,
            vec![PersistOp::SelectProfile {
                profile: "pf2".into()
            }]
        );
        assert!(e.reload_profiles);

        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile new client");
        assert_eq!(
            e.persist,
            vec![PersistOp::CreateProfile {
                name: "client".into()
            }]
        );

        // Rename targets the ACTIVE profile.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile rename main");
        assert_eq!(
            e.persist,
            vec![PersistOp::RenameProfile {
                profile: "pf0".into(),
                name: "main".into()
            }]
        );

        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile delete lab");
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfile {
                profile: "pf2".into()
            }]
        );

        // Duplicate clones the ACTIVE profile whole.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile duplicate spare");
        assert_eq!(
            e.persist,
            vec![PersistOp::DuplicateProfile {
                source: "pf0".into(),
                name: "spare".into()
            }]
        );
        assert!(e.reload_profiles);

        // Startup pins by name; the bare verb unpins.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile startup lab");
        assert_eq!(
            e.persist,
            vec![PersistOp::SetStartupProfile {
                profile: Some("pf2".into())
            }]
        );
        let e = run_palette(&mut s, "profile startup");
        assert_eq!(
            e.persist,
            vec![PersistOp::SetStartupProfile { profile: None }]
        );
    }

    #[test]
    fn profile_palette_guards_and_misses_alert_softly() {
        // Deleting the active profile is refused with a warning Alert.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile delete default");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));

        // An unknown name is a soft miss.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile switch ghost");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));

        // A duplicate new-name is refused.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile new work");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));

        // A duplicate's colliding clone-name is refused the same way…
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile duplicate work");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));

        // …and pinning an unknown name is a soft miss.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile startup ghost");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));
    }

    #[test]
    fn read_only_verb_dispatches_behind_a_running_frame() {
        // `/status` (read-only tier) emits one dispatch with the selected
        // project's repo and opens the Running overlay — no confirm.
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "status".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        match update(&mut s, Event::Key(CanopyKey::Enter)) {
            Flow::Continue(e) => {
                assert_eq!(e.dispatches.len(), 1);
                let (req, repo) = &e.dispatches[0];
                assert_eq!(*req, DispatchRequest::Status);
                assert_eq!(repo, "/r/a");
            }
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(
            matches!(s.app.overlay, Some(Overlay::Running { .. })),
            "the running line shows while the dispatch holds the loop"
        );
    }

    #[test]
    fn mutating_verb_confirms_first_and_stray_keys_are_inert() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "rebuild".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        match update(&mut s, Event::Key(CanopyKey::Enter)) {
            Flow::Continue(e) => assert!(e.dispatches.is_empty(), "confirm first"),
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        // Stray keys never confirm a mutating modal — a buffered keystroke is
        // harmless, and Space (unlike Enter) never activates the Confirm control.
        // These keep focus on the default Confirm control.
        for k in [CanopyKey::Char('y'), CanopyKey::Char(' '), CanopyKey::Down] {
            match update(&mut s, Event::Key(k)) {
                Flow::Continue(e) => assert!(e.dispatches.is_empty(), "{k:?} must be inert"),
                Flow::Quit => panic!("{k:?} must not quit"),
            }
            assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        }
        // Enter confirms the default-focused Confirm control: Confirm → Running
        // + the dispatch effect.
        match update(&mut s, Event::Key(CanopyKey::Enter)) {
            Flow::Continue(e) => {
                assert_eq!(e.dispatches.len(), 1);
                assert_eq!(e.dispatches[0].0, DispatchRequest::Rebuild);
            }
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(matches!(s.app.overlay, Some(Overlay::Running { .. })));
    }

    #[test]
    fn tab_cycles_the_modal_focus_ring_and_enter_activates_the_focused_control() {
        // Open a mutating confirm (default focus = Confirm).
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "rebuild".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        assert_eq!(
            s.app.focused_overlay_control(),
            Some(crate::dispatch::OverlayControl::Confirm),
            "a confirm modal defaults focus to Confirm"
        );
        // Tab → Cancel; Enter now cancels (dismiss, no dispatch).
        update(&mut s, Event::Key(CanopyKey::Tab));
        assert_eq!(
            s.app.focused_overlay_control(),
            Some(crate::dispatch::OverlayControl::Cancel)
        );
        match update(&mut s, Event::Key(CanopyKey::Enter)) {
            Flow::Continue(e) => assert!(e.dispatches.is_empty(), "Cancel dispatches nothing"),
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(
            s.app.overlay.is_none(),
            "activating Cancel dismisses the modal"
        );
    }

    #[test]
    fn shift_tab_wraps_the_ring_to_the_close_control() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "rebuild".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        // Shift-Tab from the default (Confirm) wraps to the last control (Close).
        update(&mut s, Event::Key(CanopyKey::BackTab));
        assert_eq!(
            s.app.focused_overlay_control(),
            Some(crate::dispatch::OverlayControl::Close)
        );
        // Enter on Close dismisses.
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert!(s.app.overlay.is_none());
    }

    #[test]
    fn esc_cancels_a_confirm_dispatching_nothing() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "gc".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        match update(&mut s, Event::Key(CanopyKey::Esc)) {
            Flow::Continue(e) => assert!(e.dispatches.is_empty()),
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(s.app.overlay.is_none(), "cancel dismisses");
    }

    #[test]
    fn parse_error_renders_inline_and_keeps_the_buffer() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "statsu".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        let err = s.app.command_error.as_ref().expect("inline error");
        assert_eq!(err.hint, Some("status"), "nearest-verb hint");
        assert_eq!(
            s.app.command_line, "statsu",
            "buffer kept for the underline"
        );
        assert_eq!(s.app.mode, Mode::Command, "still editing");
        // The next edit clears the error.
        update(&mut s, Event::Key(CanopyKey::Backspace));
        assert!(s.app.command_error.is_none());
    }

    #[test]
    fn a_soft_miss_opens_a_warning_alert() {
        // `/select nope` with no matching project opens a warning Alert, not a
        // silent toast.
        let mut s = ready(vec![project("a")]);
        let flow = no_match_status(&mut s);
        assert!(matches!(flow, Flow::Continue(_)));
        assert!(
            matches!(&s.app.overlay, Some(Overlay::Alert(m)) if m.severity == Severity::Warning),
            "a no-match opens a warning Alert: {:?}",
            s.app.overlay
        );
    }

    #[test]
    fn add_without_a_launch_cwd_opens_an_error_alert() {
        let mut s = ready(vec![project("a")]);
        s.app.launch_cwd = None;
        let _ = dashboard_verb_flow(&mut s, Verb::Add);
        assert!(
            matches!(&s.app.overlay, Some(Overlay::Alert(m)) if m.severity == Severity::Error),
            "add with no cwd opens an error Alert: {:?}",
            s.app.overlay
        );
    }
}
