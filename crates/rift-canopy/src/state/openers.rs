//! The acts with two doors and one opener: Project Settings, the worktree
//! axis, the savings breakdown, the adopt confirm, and the Recall card's
//! period and unit.
//!
//! Separate because each of these is reachable by a key and by a click, and
//! the parity rule is enforced by construction — both doors call the one
//! function here, so they cannot drift apart.

use crate::dispatch::{DispatchRequest, Overlay};
use crate::Mode;

use super::{AppState, DataRequest, Effects, Flow};

/// Select the Recall card's period — the ONE act behind the `[24h][7d][30d]
/// [all]` buttons and the `1`–`4` keys, so the two doors cannot drift.
///
/// The window is adopted on the model immediately, before the bus has
/// answered: the pressed button has to light in the frame of the press, and
/// the panel has to be able to say which period it is *drawing* rather than
/// which it last received. The figures follow when the recompute lands.
///
/// Re-selecting the live window is inert beyond the press flash. The card is
/// already showing that period, and a request for it would still cost a read.
pub(super) fn select_recall_window(s: &mut AppState, window: crate::col3::RecallWindow) -> Flow {
    if s.col3.recall_window == window {
        return Flow::Continue(Effects::default());
    }
    let mut col3 = (*s.col3).clone();
    col3.recall_window = window;
    s.set_col3(col3);
    Flow::Continue(Effects {
        data_requests: vec![DataRequest::SetRecallWindow(window)],
        ..Effects::default()
    })
}

pub(super) fn open_project_settings(s: &mut AppState) -> Flow {
    let Some(project) = s.app.selected_project() else {
        return Flow::Continue(Effects::default());
    };
    let view = s
        .detail
        .as_deref()
        .filter(|detail| detail.path == project.path)
        .map_or_else(
            || crate::ProjectSettingsView::thin(project.path.clone(), project.name.clone()),
            |detail| (*detail.project_settings).clone(),
        );
    let path = view.path.clone();
    s.app.project_settings.open(view);
    s.app.mode = Mode::ProjectSettings;
    s.dirty = true;
    Flow::Continue(Effects {
        data_requests: vec![DataRequest::Refresh(path)],
        ..Effects::default()
    })
}

/// Open the graphs column's "by worktree" axis (the family view) as an
/// `Overlay::Results` — the **one** opener behind both the `w` key and a
/// click on the token panel's `Σ` total, so the two doors can never show
/// different things.
///
/// Reads the TRUTH col3, never the animated display: a mid-tween frame's
/// series must never become a table of counts. Always opens — with no facts
/// in scope the body says so — because a hinted key that sometimes does
/// nothing is a dead key some of the time (modal-surface parity).
pub(super) fn open_worktree_axis(s: &mut AppState) -> Flow {
    let lines = crate::col3::worktree_axis_lines(&s.col3);
    s.app.open_overlay(Overlay::Results {
        title: "token usage by worktree".to_owned(),
        lines,
        scroll: 0,
        footer: None,
        truncated: false,
    });
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// The recall savings breakdown — the **one** opener behind both doors: the
/// Recall headline and the `S` key.
///
/// The headline was the only way in until this existed, which made the whole
/// disclosed basis — the per-model rows, the per-family share, the window it
/// covers — mouse-only, on a dashboard whose standing rule is that anything a
/// hint teaches is reachable both ways. It was not caught by the mechanical
/// parity checks because the card is not drawn as a modal surface; now the
/// key is hinted where it is bound (`keymap::GRAPHS`) and the hint-derivation
/// proof covers it like every other.
///
/// Always opens, like the worktree axis and for the same reason: a hinted key
/// that sometimes does nothing is a dead key some of the time. With no savings
/// data the body says what is absent.
///
/// Reads the TRUTH col3, never the animated display.
pub(super) fn open_savings_breakdown(s: &mut AppState) -> Flow {
    let (title, lines) = match &s.col3.savings {
        crate::col3::RecallView::Data(sd) => (
            match sd.usd_label.as_deref() {
                Some(label) => format!("recall savings — {label}"),
                None => "recall savings".to_owned(),
            },
            crate::col3::usd_breakdown_lines(sd),
        ),
        crate::col3::RecallView::Quiet => (
            "recall savings".to_owned(),
            vec![
                "nothing has been served in this period, so there is".to_owned(),
                "no saving to break down.".to_owned(),
            ],
        ),
        crate::col3::RecallView::Disabled => (
            "recall savings".to_owned(),
            vec![
                "recall is not enabled for this repository, so".to_owned(),
                "nothing has been avoided here — open Project".to_owned(),
                "Settings to turn it on.".to_owned(),
            ],
        ),
    };
    s.app.open_overlay(Overlay::Results {
        title,
        lines,
        scroll: 0,
        footer: None,
        truncated: false,
    });
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// The Repository card's adopt act, from either door (the family view).
///
/// **One opener behind both**, exactly as the worktree axis has: the `A` key
/// and a click on the unadopted member's status row raise the SAME confirm
/// overlay for the SAME request, so the two can never show different things.
///
/// It is a MUTATING request and therefore confirms first (the injected dispatcher): adoption
/// writes descriptors and client fragments into a checkout, which is not
/// something a stray keystroke may do. The repository is the selected
/// project's path, resolved here and again at confirm time, like every other
/// delegated act.
pub(super) fn open_adopt_confirm(s: &mut AppState) -> Flow {
    let repo = s
        .app
        .selected_project()
        .map(|p| p.path.clone())
        .unwrap_or_default();
    s.app.open_overlay(Overlay::Confirm {
        request: DispatchRequest::Adopt,
        repo,
    });
    s.dirty = true;
    Flow::Continue(Effects::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit::Target;
    use crate::input::CanopyKey;
    use crate::state::click::apply_click;
    use crate::state::fixtures::*;
    use crate::state::{update, Event};
    use crate::{EntryKind, Panel, Project, ProjectStatus};

    #[test]
    fn project_settings_key_palette_actions_confirmation_and_lock_are_one_surface() {
        let mut s = project_settings_state(crate::ProjectInitialization::Initialized);
        let opened = effects_of(update(&mut s, Event::Key(CanopyKey::Char('s'))));
        assert_eq!(s.app.mode, Mode::ProjectSettings);
        assert_eq!(
            opened.data_requests,
            vec![DataRequest::Refresh("/r/a".into())]
        );
        let applied = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert!(matches!(
            applied.project_settings_actions.as_slice(),
            [crate::ProjectSettingsAction {
                kind: crate::ProjectSettingsActionKind::Set {
                    setting: crate::ProjectSettingId::TotalRecall,
                    enabled: true,
                },
                ..
            }]
        ));

        // Raw Off → On pauses for an explicit acknowledgement; its cancel
        // chip and Esc share the same return path.
        s.app.project_settings.cursor = 6;
        let warning = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert!(warning.project_settings_actions.is_empty());
        assert!(s.app.project_settings.confirm_raw);
        let _ = apply_click(&mut s, Target::ProjectSettingsCancel);
        assert!(!s.app.project_settings.confirm_raw);
        assert_eq!(s.app.mode, Mode::ProjectSettings);
        update(&mut s, Event::Key(CanopyKey::Enter));
        let confirmed = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert!(matches!(
            confirmed.project_settings_actions[0].kind,
            crate::ProjectSettingsActionKind::Set {
                setting: crate::ProjectSettingId::RawTranscripts,
                enabled: true
            }
        ));

        // A presented operation owns and locks the modal.
        s.app.project_settings.running = true;
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert_eq!(s.app.mode, Mode::ProjectSettings);
        assert!(effects_of(apply_click(&mut s, Target::ProjectSettingsClose)).is_empty());
        assert_eq!(s.app.mode, Mode::ProjectSettings);

        // `/settings` lands on the same modal after closing it.
        s.app.project_settings.running = false;
        update(&mut s, Event::Key(CanopyKey::Esc));
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for ch in "settings".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(ch)));
        }
        update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(s.app.mode, Mode::ProjectSettings);
    }

    /// A period button and its digit key are the same act: both adopt the
    /// window on the model in the frame of the press, both ask the data plane
    /// to recompute over it, and both arm the same press flash.
    ///
    /// Resolved through the pure hit-test, so the click half exercises the
    /// drawn cells rather than a target the test invented.
    #[test]
    fn a_period_button_and_its_digit_key_are_one_act() {
        use crate::col3::RecallWindow;
        for (digit, window) in [
            ('1', RecallWindow::Today),
            ('2', RecallWindow::Last7Days),
            ('3', RecallWindow::Last30Days),
        ] {
            // The click half.
            let mut clicked = ready(vec![project("a")]);
            clicked.app.set_focus(Panel::Graphs);
            let click = click_on(Target::RecallWindow(window), &mut clicked);
            let click_effects = effects_of(update(&mut clicked, Event::Mouse(click)));

            // The key half — the digits are bound only with the graphs column
            // focused, which is where the hint teaches them.
            let mut keyed = ready(vec![project("a")]);
            keyed.app.set_focus(Panel::Graphs);
            let key_effects = effects_of(update(&mut keyed, Event::Key(CanopyKey::Char(digit))));

            assert_eq!(clicked.col3.recall_window, window, "click: {digit}");
            assert_eq!(keyed.col3.recall_window, window, "key: {digit}");
            assert_eq!(
                click_effects.data_requests,
                vec![DataRequest::SetRecallWindow(window)]
            );
            assert_eq!(key_effects.data_requests, click_effects.data_requests);
            assert_eq!(clicked.pressed, Some(Target::RecallWindow(window)));
            assert_eq!(
                keyed.pressed, clicked.pressed,
                "a keyboard activation flashes the same button a click does"
            );
        }
    }

    /// Re-selecting the live period is inert beyond the flash: the card is
    /// already showing it, and asking the bus again would still cost a read.
    #[test]
    fn re_selecting_the_live_period_asks_for_no_recompute() {
        use crate::col3::RecallWindow;
        let mut s = ready(vec![project("a")]);
        s.app.set_focus(Panel::Graphs);
        assert_eq!(s.col3.recall_window, RecallWindow::AllTime, "the default");
        let click = click_on(Target::RecallWindow(RecallWindow::AllTime), &mut s);
        let effects = effects_of(update(&mut s, Event::Mouse(click)));
        assert!(effects.data_requests.is_empty());
        assert_eq!(s.col3.recall_window, RecallWindow::AllTime);
    }

    /// The digits are bound where they are hinted and nowhere else — the rail
    /// and the detail column must not swallow them.
    #[test]
    fn the_period_digits_are_bound_only_with_the_graphs_column_focused() {
        use crate::col3::RecallWindow;
        for panel in [Panel::Projects, Panel::Detail] {
            let mut s = ready(vec![project("a")]);
            s.app.set_focus(panel);
            let effects = effects_of(update(&mut s, Event::Key(CanopyKey::Char('1'))));
            assert!(effects.data_requests.is_empty(), "{panel:?}");
            assert_eq!(s.col3.recall_window, RecallWindow::AllTime, "{panel:?}");
        }
    }

    /// the priced savings estimate: the headline opens the breakdown as the existing read-only
    /// `Overlay::Results` — full disclosed basis, no new modal surface.
    #[test]
    fn clicking_the_priced_headline_opens_the_breakdown_modal() {
        use crate::col3::{RecallView, SavingsData, UsdRow};
        let mut s = ready(vec![project("a")]);
        let mut col3 = (*s.col3).clone();
        col3.savings = RecallView::Data(SavingsData {
            usd_saved_micros: Some(6_250_000),
            usd_label: Some("~$6.25".to_owned()),
            usd_rows: vec![
                UsdRow {
                    model: "claude-opus-4-8".to_owned(),
                    tokens: 750_000,
                    price_per_mtok_micros: Some(5_000_000),
                    subtotal_micros: Some(3_750_000),
                    rate_label: Some("$5.00/MTok".to_owned()),
                    subtotal_label: Some("~$3.75".to_owned()),
                },
                UsdRow {
                    model: "claude-fable-5".to_owned(),
                    tokens: 250_000,
                    price_per_mtok_micros: Some(10_000_000),
                    subtotal_micros: Some(2_500_000),
                    rate_label: Some("$10.00/MTok".to_owned()),
                    subtotal_label: Some("~$2.50".to_owned()),
                },
            ],
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: crate::col3::WorktreeAxis::default(),
            share: Box::new(crate::col3::ShareData::default()),
        });
        s.set_col3(col3);
        let _ = apply_click(&mut s, Target::RecallHeadline);
        let Some(Overlay::Results { title, lines, .. }) = &s.app.overlay else {
            panic!(
                "expected the breakdown Results overlay, got {:?}",
                s.app.overlay
            );
        };
        assert_eq!(title, "recall savings — ~$6.25");
        let body = lines.join("\n");
        assert!(body.contains("net tokens avoided   1,000,000"), "{body}");
        assert!(body.contains("claude-opus-4-8"), "{body}");
        assert!(body.contains("$5.00/MTok"), "{body}");
        assert!(body.contains("~$3.75"), "{body}");
        assert!(body.contains("total = ~$6.25"), "{body}");
        assert!(body.contains("estimate, not a bill"), "{body}");
        assert!(
            body.contains("cl100k_base"),
            "the tokenizer basis is disclosed: {body}"
        );
    }

    /// A Col3 whose token panel carries a total and a two-member axis.
    fn axis_col3() -> crate::col3::Col3 {
        use crate::col3::{
            BreakdownData, BreakdownView, TokensData, TokensView, WorktreeAxis, WorktreeSlice,
        };
        crate::col3::Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 2.0],
                total: 1_050.0,
                by_worktree: WorktreeAxis {
                    rows: vec![
                        WorktreeSlice {
                            name: Some("main".to_owned()),
                            tokens: 750,
                        },
                        WorktreeSlice {
                            name: None,
                            tokens: 300,
                        },
                    ],
                    family_only: 0,
                },
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 1.0,
                read: 1.0,
                output: 1.0,
                cache_write: 1.0,
                read_fraction: 0.5,
            }),
            ..crate::col3::Col3::default()
        }
    }

    /// the family view: the axis has two doors — `w` in the graphs column and
    /// a click on the token panel's `Σ` total — and **one** opener behind
    /// them, so they can never show different things — modal-surface parity in
    /// both directions: the hinted key is clickable and the clickable is
    /// keyed.
    #[test]
    fn the_worktree_axis_opens_from_both_doors_through_one_opener() {
        let by_key = {
            let mut s = ready(vec![project("a")]);
            s.set_col3(axis_col3());
            s.app.set_focus(Panel::Graphs);
            let _ = update(&mut s, Event::Key(CanopyKey::Char('w')));
            s
        };
        let by_click = {
            let mut s = ready(vec![project("a")]);
            s.set_col3(axis_col3());
            let _ = apply_click(&mut s, Target::TokensByWorktree);
            s
        };
        let Some(Overlay::Results { title, lines, .. }) = &by_key.app.overlay else {
            panic!("`w` opens the axis, got {:?}", by_key.app.overlay);
        };
        assert_eq!(title, "token usage by worktree");
        assert_eq!(
            Some(Overlay::Results {
                title: title.clone(),
                lines: lines.clone(),
                scroll: 0,
                footer: None,
                truncated: false,
            }),
            by_click.app.overlay,
            "the click's door opens byte-identically to the key's"
        );
        let body = lines.join("\n");
        assert!(body.contains("family total  1,050 tok"), "{body}");
        assert!(body.contains("main     750 tok"), "{body}");
        assert!(body.contains("unknown  300 tok"), "{body}");
        // The hint that teaches `w` is the graphs column's, and it is live.
        assert!(
            crate::keymap::hints(crate::keymap::HintContext::Graphs)
                .iter()
                .any(|h| h.keys == "w" && h.verb == "by worktree"),
            "the key is taught where it is bound"
        );
    }

    /// A member with nothing wired in it, selected, with the card's own detail
    /// carrying the same status — the one premise the adopt act is offered on.
    fn unadopted() -> AppState {
        let mut s = ready(vec![Project {
            name: "member".into(),
            path: "/r/member".into(),
            status: ProjectStatus::NotAdopted,
            kind: EntryKind::Worktree,
        }]);
        s.app.set_focus(Panel::Detail);
        s
    }

    /// ★ the family view + modal-surface parity: the adopt act has two doors — `A` and a
    /// click on the Repository card's status row — and **one** opener behind
    /// them, so they can never do different things.
    ///
    /// Parity in both directions is asserted, not assumed: the hinted key is
    /// clickable, the clickable is keyed, and the hint that teaches `A` is live
    /// in exactly the stage the key is bound in. The request is MUTATING, so
    /// both doors open a confirm rather than dispatching — adoption
    /// writes into a checkout and a stray keystroke may not.
    #[test]
    fn the_adopt_act_opens_from_both_doors_through_one_opener() {
        let by_key = {
            let mut s = unadopted();
            let _ = update(&mut s, Event::Key(CanopyKey::Char('A')));
            s
        };
        let by_click = {
            let mut s = unadopted();
            let _ = apply_click(&mut s, Target::RepositoryAdopt);
            s
        };
        let Some(Overlay::Confirm { request, repo }) = &by_key.app.overlay else {
            panic!("`A` opens the confirm, got {:?}", by_key.app.overlay);
        };
        assert_eq!(*request, DispatchRequest::Adopt);
        assert_eq!(repo, "/r/member");
        assert_eq!(
            by_key.app.overlay, by_click.app.overlay,
            "the click's door opens exactly what the key's does"
        );
        assert!(DispatchRequest::Adopt.is_mutating(), "confirm-gated");
        assert!(Target::RepositoryAdopt.is_button(), "button semantics");

        // The key is taught where it is bound, and NOWHERE else: an adopted
        // row's Detail bar must not advertise it (modal-surface parity's per-stage rule).
        let taught = |ctx| {
            crate::keymap::hints(ctx)
                .iter()
                .any(|h| h.keys == "A" && h.verb == "adopt")
        };
        assert!(taught(crate::keymap::HintContext::DetailNotAdopted));
        assert!(!taught(crate::keymap::HintContext::Detail));
        assert!(!taught(crate::keymap::HintContext::Rail));
        // …and the stage is derived from the same premise the reducer binds on.
        assert_eq!(
            crate::keymap::context(
                Mode::Normal,
                Panel::Detail,
                None,
                &crate::profile_menu::MenuStage::Browse,
                false,
                crate::input::selection_is_unadopted(&unadopted().app),
            ),
            crate::keymap::HintContext::DetailNotAdopted
        );

        // An ADOPTED row swallows the key rather than offering an act its bar
        // does not teach.
        let mut idle = ready(vec![project("a")]);
        idle.app.set_focus(Panel::Detail);
        let _ = update(&mut idle, Event::Key(CanopyKey::Char('A')));
        assert!(idle.app.overlay.is_none(), "unhinted stage, unbound key");
    }

    /// A hinted key is never dead (modal-surface parity): with nothing in scope the axis
    /// still opens and says so, rather than swallowing the keystroke.
    #[test]
    fn the_axis_key_opens_an_honest_statement_when_there_is_nothing_to_split() {
        let mut s = ready(vec![project("a")]);
        s.app.set_focus(Panel::Graphs);
        let _ = update(&mut s, Event::Key(CanopyKey::Char('w')));
        let Some(Overlay::Results { lines, .. }) = &s.app.overlay else {
            panic!("expected the axis overlay, got {:?}", s.app.overlay);
        };
        assert!(lines.join(" ").contains("no token facts in scope"));
        // …and `w` is bound only where it is hinted: the rail keeps it inert.
        let mut rail = ready(vec![project("a")]);
        rail.app.set_focus(Panel::Projects);
        let _ = update(&mut rail, Event::Key(CanopyKey::Char('w')));
        assert!(rail.app.overlay.is_none(), "unhinted column, unbound key");
    }

    /// ★ the family view: "No new modal surface is introduced — the axis
    /// lives in the existing `Overlay::Results`."
    ///
    /// The inventory is asserted rather than assumed. The two exhaustive
    /// matches below are the guard: adding an `Overlay` variant or a `Mode`
    /// stops this test compiling, which forces whoever adds one to classify
    /// it here and — if it is modal — to join it to
    /// `every_modal_surface_carries_the_close_affordance`, `keymap`'s
    /// `ALL`/`app_in`, and `Target::is_button` (modal-surface parity). The counts name
    /// what the classification found, so a silent addition cannot pass.
    #[test]
    fn no_new_modal_surface_was_introduced() {
        fn overlay_kind(o: &Overlay) -> &'static str {
            match o {
                Overlay::Results { .. } => "results",
                Overlay::Confirm { .. } => "confirm",
                Overlay::Running { .. } => "running",
                Overlay::Alert(_) => "alert",
                Overlay::Notice(_) => "notice",
            }
        }
        fn draws_a_modal_frame(m: Mode) -> bool {
            match m {
                Mode::ProfileMenu | Mode::ColourPicker | Mode::Explorer | Mode::ProjectSettings => {
                    true
                }
                Mode::Normal | Mode::Command | Mode::AddProject | Mode::PanelEditor => false,
            }
        }
        const OVERLAY_KINDS: [&str; 5] = ["results", "confirm", "running", "alert", "notice"];
        const MODAL_MODES: [Mode; 4] = [
            Mode::ProfileMenu,
            Mode::ColourPicker,
            Mode::Explorer,
            Mode::ProjectSettings,
        ];
        assert_eq!(
            OVERLAY_KINDS.len(),
            5,
            "five overlay kinds before this change, five after"
        );
        assert!(MODAL_MODES.into_iter().all(draws_a_modal_frame));
        assert_eq!(
            MODAL_MODES.len(),
            4,
            "four mode-modals, and every one of them is in the close-X sweep"
        );
        // The axis reuses one of them, from both doors, and disturbs no mode.
        for open in [
            &(|s: &mut AppState| {
                s.app.set_focus(Panel::Graphs);
                let _ = update(s, Event::Key(CanopyKey::Char('w')));
            }) as &dyn Fn(&mut AppState),
            &|s: &mut AppState| {
                let _ = apply_click(s, Target::TokensByWorktree);
            },
        ] {
            let mut s = ready(vec![project("a")]);
            s.set_col3(axis_col3());
            open(&mut s);
            assert_eq!(
                s.app.overlay.as_ref().map(overlay_kind),
                Some("results"),
                "the axis is a results overlay — a surface that already existed"
            );
            assert_eq!(s.app.mode, Mode::Normal, "and it opens no mode-modal");
            // Its hints are the existing results table, so no hint context
            // was minted for it either.
            assert_eq!(
                crate::keymap::context(
                    s.app.mode,
                    s.app.focus,
                    s.app.overlay.as_ref(),
                    &s.app.profile_menu.stage,
                    s.app.project_settings.running,
                    crate::input::selection_is_unadopted(&s.app),
                ),
                crate::keymap::HintContext::OverlayResults
            );
        }
    }

    /// The breakdown **always** opens and says what is absent, through the
    /// headline or through `S`.
    ///
    /// It used to open nothing without a dollar figure, which was defensible
    /// while a dollar chip was the only door — there was nothing to click. A
    /// hinted key cannot work that way: a key that sometimes does nothing is
    /// a dead key some of the time, so the modal answers on a card with no
    /// savings by saying it has none (the worktree axis's rule, and the same
    /// wording discipline: absence is stated, never drawn as a zero).
    #[test]
    fn the_savings_modal_answers_even_with_nothing_to_break_down() {
        let by_click = |s: &mut AppState| {
            let _ = apply_click(s, Target::RecallHeadline);
        };
        let by_key = |s: &mut AppState| {
            s.app.set_focus(crate::Panel::Graphs);
            let _ = update(s, Event::Key(CanopyKey::Char('S')));
        };
        let doors: [&dyn Fn(&mut AppState); 2] = [&by_click, &by_key];
        for door in doors {
            let mut s = ready(vec![project("a")]);
            door(&mut s);
            let Some(Overlay::Results { title, lines, .. }) = &s.app.overlay else {
                panic!("the breakdown opens from every door");
            };
            assert_eq!(title, "recall savings");
            assert!(
                lines.iter().any(|l| l.contains("recall is not enabled")),
                "it says what is absent: {lines:?}"
            );
        }
    }
}
