//! Committed golden frames (insta) guarding visual drift in the deterministic
//! renderers: the ASCII dashboard, the static figlet banner, the 2D splash
//! keyframes, and the Full3d mesh raster (frames + the mesh splash tier point sets).
//! Every input is fixed (seed/size/state), so these are stable.
//!
//! The seeded states and the render helpers every assertion reads live in
//! `common/snapshot_fixture.rs`, so this file stays a list of what is pinned.

use rift_canopy::render::frame::{Frame, Size};
use rift_canopy::render::Capabilities;
use rift_canopy::splash::director::Tier;
use rift_canopy::splash::mesh3d::Resolution;
use rift_canopy::splash::render2d::draw_tier;
use rift_canopy::state::AppState;
use rift_canopy::{AsciiBackend, EntryKind, Project, ProjectStatus};

#[path = "common/snapshot_fixture.rs"]
mod snapshot_fixture;

use snapshot_fixture::{
    col3_text, col3_with_data, dashboard_state, frame_text, full3d_caps, full3d_text,
    member_row_detail, member_row_frame, point_set_text, project_settings_state,
    project_settings_state_with_codex_hooks, recall_text, recall_viewer_state, render_at,
    render_state, render_state_with_caps, suggest_state, system_text, widget_text, WIDGET_SERIES,
};

#[test]
fn golden_dashboard_frame() {
    let mut s = dashboard_state();
    let mut f = Frame::new(s.size);
    let mut b = AsciiBackend::new(Capabilities::empty());
    rift_canopy::view::draw(&s.snapshot(), &mut b, &mut f);
    insta::assert_snapshot!("dashboard", frame_text(&f));
}

#[test]
fn golden_dashboard_frame_wide() {
    // A wider terminal guards the 20/40/40 tiling at a second size.
    let mut s = dashboard_state();
    s.size = Size::new(120, 40);
    let mut f = Frame::new(s.size);
    let mut b = AsciiBackend::new(Capabilities::empty());
    rift_canopy::view::draw(&s.snapshot(), &mut b, &mut f);
    insta::assert_snapshot!("dashboard_wide", frame_text(&f));
}

#[test]
fn golden_project_settings_wide() {
    insta::assert_snapshot!(
        "project_settings_wide",
        render_state(&mut project_settings_state(
            Size::new(120, 40),
            rift_canopy::ProjectInitialization::Initialized,
            rift_canopy::ProjectPolicyState::Declared,
        ))
    );
}

#[test]
fn golden_project_settings_narrow() {
    insta::assert_snapshot!(
        "project_settings_narrow",
        render_state(&mut project_settings_state(
            Size::new(60, 20),
            rift_canopy::ProjectInitialization::Initialized,
            rift_canopy::ProjectPolicyState::Declared,
        ))
    );
}

#[test]
fn golden_project_settings_invalid_policy() {
    insta::assert_snapshot!(
        "project_settings_invalid_policy",
        render_state(&mut project_settings_state(
            Size::new(100, 32),
            rift_canopy::ProjectInitialization::Initialized,
            rift_canopy::ProjectPolicyState::Invalid,
        ))
    );
}

/// A clone whose `.rift/` arrived with the repository: the committed values
/// read as facts, the detected supported agents are pre-ticked, and the
/// initialize row names what setup on this machine will do.
#[test]
fn golden_project_settings_not_provisioned() {
    insta::assert_snapshot!(
        "project_settings_not_provisioned",
        render_state(&mut project_settings_state(
            Size::new(120, 40),
            rift_canopy::ProjectInitialization::NotProvisioned,
            rift_canopy::ProjectPolicyState::Declared,
        ))
    );
}

#[test]
fn golden_project_settings_incomplete_setup() {
    insta::assert_snapshot!(
        "project_settings_incomplete_setup",
        render_state(&mut project_settings_state(
            Size::new(120, 40),
            rift_canopy::ProjectInitialization::SetupIncomplete,
            rift_canopy::ProjectPolicyState::Declared,
        ))
    );
}

/// The two repair states a real install passes through — "hooks need refresh"
/// (Stale, refreshable by apply) and "hooks conflict" (refused) — were never
/// snapshot-pinned before the hooks-side shape migration made Stale the
/// sanctioned road out of a moved verb table.
#[test]
fn golden_project_settings_hooks_need_refresh() {
    insta::assert_snapshot!(
        "project_settings_hooks_need_refresh",
        render_state(&mut project_settings_state_with_codex_hooks(
            Size::new(120, 40),
            rift_canopy::ProjectInitialization::Initialized,
            rift_canopy::ProjectPolicyState::Declared,
            rift_canopy::NativeAgentHookWiring::Stale,
        ))
    );
}

#[test]
fn golden_project_settings_hooks_conflict() {
    insta::assert_snapshot!(
        "project_settings_hooks_conflict",
        render_state(&mut project_settings_state_with_codex_hooks(
            Size::new(120, 40),
            rift_canopy::ProjectInitialization::Initialized,
            rift_canopy::ProjectPolicyState::Declared,
            rift_canopy::NativeAgentHookWiring::Conflict,
        ))
    );
}

#[test]
fn golden_layout_two_col() {
    // 48 ≤ w < 76 → the two-column degraded grid.
    insta::assert_snapshot!("layout_two_col", render_at(60, 20));
}

#[test]
fn golden_layout_single_panel() {
    // w < 48 → one panel + a tab strip.
    insta::assert_snapshot!("layout_single_panel", render_at(36, 16));
}

#[test]
fn golden_layout_too_small() {
    // Below the floor → the centered notice.
    insta::assert_snapshot!("layout_too_small", render_at(22, 5));
}

#[test]
fn golden_recall_viewer_full_unicode() {
    let mut state = recall_viewer_state(Size::new(120, 40), rift_canopy::ViewerPane::Timeline);
    insta::assert_snapshot!(
        "recall_viewer_full_unicode",
        render_state_with_caps(&mut state, Capabilities::UNICODE)
    );
}

#[test]
fn golden_recall_viewer_two_col_ascii() {
    let mut state = recall_viewer_state(Size::new(60, 24), rift_canopy::ViewerPane::Checkpoints);
    insta::assert_snapshot!(
        "recall_viewer_two_col_ascii",
        render_state_with_caps(&mut state, Capabilities::empty())
    );
}

#[test]
fn golden_recall_viewer_single_ascii() {
    let mut state = recall_viewer_state(Size::new(44, 20), rift_canopy::ViewerPane::Timeline);
    insta::assert_snapshot!(
        "recall_viewer_single_ascii",
        render_state_with_caps(&mut state, Capabilities::empty())
    );
}

#[test]
fn golden_recall_viewer_too_small_ascii() {
    let mut state = recall_viewer_state(Size::new(22, 5), rift_canopy::ViewerPane::Projects);
    insta::assert_snapshot!(
        "recall_viewer_too_small_ascii",
        render_state_with_caps(&mut state, Capabilities::empty())
    );
}

#[test]
fn golden_col1_worktree_children() {
    // A repository row with an indented linked-worktree child.
    let mut s = AppState::new(
        vec![
            Project {
                name: "rift-cli".into(),
                path: "/r/rift-cli".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            },
            Project {
                name: "feature-wt".into(),
                path: "/r/rift-cli-wt".into(),
                status: ProjectStatus::NotInitialized,
                kind: EntryKind::Worktree,
            },
            Project {
                name: "rift-site".into(),
                path: "/r/rift-site".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            },
        ],
        Size::new(80, 18),
    );
    s.app.splash.skip();
    let mut f = Frame::new(s.size);
    let mut b = AsciiBackend::new(Capabilities::empty());
    rift_canopy::view::draw(&s.snapshot(), &mut b, &mut f);
    insta::assert_snapshot!("col1_worktree_children", frame_text(&f));
}

/// The family row selected: the rail carries every member-row state at the
/// Unicode tier (`●` running, `◔` not adopted, `◒` setup incomplete, `!` the
/// gone checkout), and the Repository card's status row names the members the
/// family's three sessions came from — the badge read back to its checkouts
/// (the family view, the serve accounting seam's discipline for numbers on screen).
#[test]
fn golden_col1_member_row_states() {
    let detail = member_row_detail(
        "rift-cli",
        "/r/rift-cli",
        EntryKind::Repo,
        ProjectStatus::Running {
            since_ms: 0,
            active_sessions: 3,
        },
        vec!["main".into(), "swt-a".into()],
    );
    let frame = member_row_frame(0, detail, Capabilities::TRUECOLOR | Capabilities::UNICODE);
    insta::assert_snapshot!("col1_member_row_states", frame_text(&frame));
}

/// The unadopted MEMBER selected, at the ASCII floor: its Repository card asks
/// for the one act that changes anything (the verb, never a key — the rail's
/// `a` is already "add project"), and `◔` degrades to `a` rather than
/// collapsing into `·`'s `-`.
#[test]
fn golden_member_row_not_adopted_ascii() {
    let detail = member_row_detail(
        "swt-b",
        "/w/swt-b",
        EntryKind::Worktree,
        ProjectStatus::NotAdopted,
        Vec::new(),
    );
    let frame = member_row_frame(2, detail, Capabilities::empty());
    insta::assert_snapshot!("member_row_not_adopted_ascii", frame_text(&frame));
}

/// A repository whose wired agent's capture is being lost, at the ASCII floor:
/// the rail's dot degrades to `!`, and the Repository card's status row names
/// the act — Project Settings, where the remedy is printed under the adapter.
#[test]
fn golden_repository_card_integration_error_ascii() {
    let detail = member_row_detail(
        "rift-cli",
        "/r/rift-cli",
        EntryKind::Repo,
        ProjectStatus::Error(rift_canopy::StatusError::Integration),
        Vec::new(),
    );
    let frame = member_row_frame(0, detail, Capabilities::empty());
    insta::assert_snapshot!(
        "repository_card_integration_error_ascii",
        frame_text(&frame)
    );
}

// --- column 3: the six-section stack ---------------------------------------

#[test]
fn golden_recall_usd_chip() {
    // The USD-estimate chip on the Recall headline: present
    // only with an honest figure, tilde mandatory, drawn from the same solver
    // the hit-test reads. The other goldens keep `None` and are untouched —
    // absence draws nothing.
    let mut col3 = col3_with_data();
    if let rift_canopy::RecallView::Data(sd) = &mut col3.savings {
        sd.usd_saved_micros = Some(105_900_000);
        sd.usd_label = Some("~$105.90".to_owned());
    }
    insta::assert_snapshot!("recall_usd_chip", recall_text(&col3, 40, 10, false));
}

/// The period row and the read-vs-saved bars, at the panel's registered
/// minimum height.
///
/// A period other than `all` and a window sentence of its own, so the goldens
/// cover a card that says which period it is drawing as well as one that lets
/// the family boundary decide.
#[test]
fn golden_recall_window_and_bars() {
    let mut col3 = col3_with_data();
    col3.recall_window = rift_canopy::RecallWindow::Last7Days;
    if let rift_canopy::RecallView::Data(sd) = &mut col3.savings {
        *sd.share = rift_canopy::ShareData {
            window: Some("since 7 days ago".to_owned()),
            overall: Some(37.5),
            honesty: 5_400,
            avoided_tokens: 6_000,
            read_tokens: 10_000,
            sessions_with_reads: 3,
            ..rift_canopy::ShareData::default()
        };
    }
    insta::assert_snapshot!("recall_window_and_bars", recall_text(&col3, 40, 13, false));
}

/// The same card read in dollars: the `[usd]` chip is live and both bars
/// carry the money authority's labels, tilde mandatory.
///
/// A second golden rather than a variant of the first, because the pair is
/// the point — the token view and the dollar view have to be diffable against
/// each other when either one drifts.
#[test]
fn golden_recall_bars_in_dollars() {
    let mut col3 = col3_with_data();
    col3.recall_window = rift_canopy::RecallWindow::Last7Days;
    col3.recall_unit = rift_canopy::RecallUnit::Usd;
    if let rift_canopy::RecallView::Data(sd) = &mut col3.savings {
        *sd.share = rift_canopy::ShareData {
            window: Some("since 7 days ago".to_owned()),
            overall: Some(37.5),
            honesty: 5_400,
            avoided_tokens: 6_000,
            read_tokens: 10_000,
            sessions_with_reads: 3,
            ..rift_canopy::ShareData::default()
        };
        sd.usd_saved_micros = Some(30_000);
        sd.usd_label = Some("~$0.03".to_owned());
        sd.usd_read_micros = Some(50_000);
        sd.usd_read_label = Some("~$0.05".to_owned());
    }
    insta::assert_snapshot!("recall_bars_in_dollars", recall_text(&col3, 40, 13, false));
}

#[test]
fn golden_col3_tall_all_expanded() {
    insta::assert_snapshot!("col3_tall", col3_text(&col3_with_data(), 30));
}

#[test]
fn golden_col3_short_scrolls_the_expanded_stack() {
    // Height pressure (the scrollbar rule): the fully-expanded panels overflow 19 rows, so
    // the column scrolls the window (offset 0 = the top of the expanded stack)
    // rather than collapsing — the scrollbar signals the panels below.
    insta::assert_snapshot!("col3_short", col3_text(&col3_with_data(), 19));
}

#[test]
fn golden_col3_placeholders() {
    // Every placeholder state at once: no token activity, no model calls,
    // recall disabled, no sessions — placeholders, never fake zero lines.
    insta::assert_snapshot!(
        "col3_placeholders",
        col3_text(&rift_canopy::Col3::default(), 20)
    );
}

/// **The seated stack's empty states at full height.** The
/// height-20 `col3_placeholders` golden is the scroll-window pin; below the
/// seated minimum (42) it cannot show every panel's empty sentence at once,
/// so a regenerated golden could drop one unnoticed. This one renders the
/// two workflow absences — `NoActivity` on Dispatch and Recap, which is a
/// projection with nothing captured, NOT the missing-projection placeholder —
/// at h=44, and asserts each absence sentence in the clear BEFORE the byte
/// lock, so the lock can never be re-frozen without them.
#[test]
fn golden_col3_empty_states_tall() {
    let col3 = rift_canopy::Col3 {
        dispatch: rift_canopy::DispatchView::NoActivity,
        recap: rift_canopy::RecapView::NoActivity,
        ..rift_canopy::Col3::default()
    };
    let text = col3_text(&col3, 44);
    for sentence in ["nothing captured yet", "no agent sessions yet"] {
        assert!(text.contains(sentence), "{sentence:?} missing:\n{text}");
    }
    assert!(
        !text.contains("no capture projection here"),
        "NoActivity is not the missing-projection placeholder:\n{text}"
    );
    insta::assert_snapshot!("col3_empty_states_tall", text);
}

/// Every placeholder at full height: the missing-projection sentence for both
/// workflow panels, plus the token/model/recall/session placeholders, each
/// asserted in the clear before the byte lock (the h=20 golden stays as the
/// scroll-window pin).
#[test]
fn golden_col3_placeholders_tall() {
    let text = col3_text(&rift_canopy::Col3::default(), 44);
    assert_eq!(
        text.matches("no capture projection here").count(),
        2,
        "both workflow panels name the missing projection:\n{text}"
    );
    assert!(!text.contains("nothing captured yet"), "{text}");
    assert!(!text.contains("no agent sessions yet"), "{text}");
    insta::assert_snapshot!("col3_placeholders_tall", text);
}

#[test]
fn golden_col3_unknown_model_and_quiet_recall() {
    // The remaining placeholder states: the explicit unknown-model indicator
    // and enabled-but-quiet recall (visibly distinct from disabled).
    let col3 = rift_canopy::Col3 {
        nic: rift_canopy::NicView::UnknownModel {
            model: "mystery-model-9000".into(),
            active_sessions: 1,
        },
        savings: rift_canopy::RecallView::Quiet,
        calls: rift_canopy::RecallView::Quiet,
        metrics: rift_canopy::PanelMetrics {
            context_used: Some(125),
            context_window: None,
            context_scope: rift_canopy::SystemScope::CurrentSessions,
            ..col3_with_data().metrics
        },
        ..col3_with_data()
    };
    insta::assert_snapshot!("col3_unknown_and_quiet", col3_text(&col3, 20));
}

/// **The System panel when wakes are poll-only**: the dim
/// "watch: poll-only" row at full height and the head suffix when collapsed,
/// asserted in the clear before the byte lock. The Native frames live in
/// `system_scope_states`, untouched — `Native` draws nothing.
#[test]
fn golden_system_watch_poll_only() {
    let col3 = rift_canopy::Col3 {
        watch: rift_canopy::WatchMode::PollOnly,
        ..col3_with_data()
    };
    let expanded = system_text(&col3, 40, 12, false);
    assert!(expanded.contains("watch: poll-only"), "{expanded}");
    let collapsed = system_text(&col3, 56, 3, true);
    assert!(collapsed.contains("·  poll-only"), "{collapsed}");
    insta::assert_snapshot!(
        "system_watch_poll_only",
        format!("{expanded}\n--- collapsed ---\n{collapsed}")
    );
}

#[test]
fn golden_system_scope_and_responsive_states() {
    let current = col3_with_data();
    let completed = rift_canopy::Col3 {
        nic: rift_canopy::NicView::Reading(rift_canopy::NicReading {
            active_sessions: 0,
            ..match current.nic.clone() {
                rift_canopy::NicView::Reading(reading) => reading,
                _ => unreachable!(),
            }
        }),
        active_sessions: rift_canopy::ActiveSessionsView::NoSessions,
        metrics: rift_canopy::PanelMetrics {
            context_scope: rift_canopy::SystemScope::LastCompletedSession,
            activity_scope: rift_canopy::SystemScope::LastCompletedSession,
            ..current.metrics.clone()
        },
        ..current.clone()
    };
    let held = rift_canopy::Col3 {
        metrics: rift_canopy::PanelMetrics {
            context_scope: rift_canopy::SystemScope::LastCompletedSession,
            activity_scope: rift_canopy::SystemScope::CurrentSessions,
            ..current.metrics.clone()
        },
        ..current.clone()
    };
    let unavailable = rift_canopy::Col3 {
        nic: rift_canopy::NicView::NoData,
        metrics: rift_canopy::PanelMetrics {
            projection_bytes: current.metrics.projection_bytes,
            ..rift_canopy::PanelMetrics::default()
        },
        ..current.clone()
    };
    let unknown = rift_canopy::Col3 {
        nic: rift_canopy::NicView::UnknownModel {
            model: "future-model".into(),
            active_sessions: 2,
        },
        metrics: rift_canopy::PanelMetrics {
            context_window: None,
            ..current.metrics.clone()
        },
        ..current.clone()
    };
    // The degraded-watch frame, for the System panel's truthfulness: the
    // multi-state golden carries the poll-only System panel at
    // full height and collapsed, so a regeneration cannot silently drop the
    // degrade row — the two `contains` asserts below are the lock.
    let poll_only = rift_canopy::Col3 {
        watch: rift_canopy::WatchMode::PollOnly,
        ..current.clone()
    };
    let snapshot = format!(
        "CURRENT\n{}\n\nLAST COMPLETED\n{}\n\nHELD PREVIOUS\n{}\n\nUNAVAILABLE\n{}\n\nUNKNOWN MODEL\n{}\n\nNARROW\n{}\n\nCOLLAPSED\n{}\n\nPOLL-ONLY\n{}\n\nPOLL-ONLY COLLAPSED\n{}",
        system_text(&current, 36, 14, false),
        system_text(&completed, 36, 14, false),
        system_text(&held, 36, 14, false),
        system_text(&unavailable, 36, 14, false),
        system_text(&unknown, 36, 14, false),
        system_text(&current, 24, 14, false),
        system_text(&current, 36, 3, true),
        system_text(&poll_only, 36, 14, false),
        system_text(&poll_only, 56, 3, true),
    );
    assert!(
        snapshot.contains("watch: poll-only"),
        "the degraded watch names itself at full height: {snapshot}"
    );
    assert!(
        snapshot.contains("\u{b7}  poll-only"),
        "the collapsed head carries the poll-only suffix (width 56, as the dedicated \
         golden renders it \u{2014} 36 truncates it): {snapshot}"
    );
    insta::assert_snapshot!("system_scope_states", snapshot);
}

#[test]
fn golden_recall_responsive_and_attribution_states() {
    let populated = col3_with_data();
    let legacy = rift_canopy::Col3 {
        calls: rift_canopy::RecallView::Quiet,
        ..populated.clone()
    };
    // Both serve channels at once — the widest the chart can ever get, since
    // `serve_call_event.channel` is CHECK-constrained to this closed pair.
    // (The former "four threads + hidden" case tested the retired per-thread
    // overflow affordance, which the channel axis makes unreachable; it went
    // with it.)
    let both_channels = rift_canopy::Col3 {
        calls: rift_canopy::RecallView::Data(rift_canopy::CallsData {
            total: 72,
            window_end_ms: 1_800_000,
            series: ["hook_inject", "skill_cli"]
                .into_iter()
                .enumerate()
                .map(|(i, channel)| rift_canopy::ChannelCallSeries {
                    channel: channel.to_owned(),
                    last_call_at: 1_799_000 - i64::try_from(i).unwrap_or(0),
                    rate: (0u16..30)
                        .map(|bucket| f64::from((bucket + u16::try_from(i).unwrap_or(0)) % 7))
                        .collect(),
                })
                .collect(),
        }),
        ..populated.clone()
    };

    let snapshot = format!(
        "COLLAPSED\n{}\n\nNARROW\n{}\n\nLEGACY SAVINGS ONLY\n{}\n\nBOTH CHANNELS\n{}",
        recall_text(&populated, 42, 5, true),
        recall_text(&populated, 24, 12, false),
        recall_text(&legacy, 42, 12, false),
        recall_text(&both_channels, 54, 12, false),
    );
    insta::assert_snapshot!("recall_responsive_states", snapshot);
}

// --- graph widgets: fixed series, block (floor) tier ------------------------

#[test]
fn golden_widget_sparkline() {
    let max = rift_canopy::plot::autoscale_max(&WIDGET_SERIES);
    insta::assert_snapshot!(
        "widget_sparkline",
        widget_text(&rift_canopy::plot::sparkline_points(&WIDGET_SERIES, max))
    );
}

#[test]
fn golden_widget_bars() {
    let max = rift_canopy::plot::autoscale_max(&WIDGET_SERIES);
    insta::assert_snapshot!(
        "widget_bars",
        widget_text(&rift_canopy::plot::bar_points(&WIDGET_SERIES, max, 24))
    );
}

#[test]
fn golden_widget_line() {
    let max = rift_canopy::plot::autoscale_max(&WIDGET_SERIES);
    insta::assert_snapshot!(
        "widget_line",
        widget_text(&rift_canopy::plot::line_points(&WIDGET_SERIES, max, 48))
    );
}

#[test]
fn golden_widget_gauge() {
    insta::assert_snapshot!(
        "widget_gauge",
        widget_text(&rift_canopy::plot::gauge_points(0.65, 48, 4))
    );
}

#[test]
fn golden_widget_braille_line() {
    // The sub-cell tier over the same fixed series (cell-aligned with the
    // block tier by the one-contract property; this pins the dot pattern).
    use rift_canopy::render::frame::Style;
    let max = rift_canopy::plot::autoscale_max(&WIDGET_SERIES);
    let pts = rift_canopy::plot::line_points(&WIDGET_SERIES, max, 48);
    let mut f = Frame::new(Size::new(26, 8));
    let inner = f.border(f.rect(), "plot", Style::DEFAULT);
    rift_canopy::plot::draw_braille(
        &mut f,
        inner,
        &pts,
        &rift_canopy::plot::Paint::Solid(Style::DEFAULT),
    );
    insta::assert_snapshot!("widget_braille_line", frame_text(&f));
}

#[test]
fn golden_static_banner() {
    let mut b = AsciiBackend;
    let mut f = Frame::new(Size::new(34, 7));
    draw_tier(
        Tier::StaticAscii,
        0.0,
        1,
        &mut b,
        &mut f,
        Capabilities::empty(),
        Resolution::Full,
    );
    insta::assert_snapshot!("splash_static_banner", frame_text(&f));
}

#[test]
fn golden_reduced2d_final_frame() {
    let mut b = AsciiBackend;
    let mut f = Frame::new(Size::new(48, 16));
    draw_tier(
        Tier::Reduced2d,
        1.0,
        42,
        &mut b,
        &mut f,
        Capabilities::empty(),
        Resolution::Full,
    );
    insta::assert_snapshot!("splash_reduced2d_final", frame_text(&f));
}

#[test]
fn golden_reduced2d_midframe() {
    let mut b = AsciiBackend;
    let mut f = Frame::new(Size::new(48, 16));
    draw_tier(
        Tier::Reduced2d,
        0.5,
        7,
        &mut b,
        &mut f,
        Capabilities::empty(),
        Resolution::Full,
    );
    insta::assert_snapshot!("splash_reduced2d_mid", frame_text(&f));
}

// ---- the dispatch overlay + inline parse errors ----------------------------

#[test]
fn golden_overlay_results_failed_verify() {
    let mut s = dashboard_state();
    s.size = Size::new(100, 32);
    let outcome = rift_canopy::DispatchOutcome {
        code: 65,
        human: "re-derived 2 checkpoint(s) from git; 2 shard(s) checked\nchain: BROKEN at #1"
            .into(),
        json: None,
    };
    s.app.overlay = Some(rift_canopy::Overlay::results(
        &rift_canopy::DispatchRequest::Verify,
        &outcome,
    ));
    insta::assert_snapshot!("overlay_verify_failed", render_state(&mut s));
}

#[test]
fn golden_overlay_results_truncated() {
    let mut s = dashboard_state();
    s.size = Size::new(100, 32);
    let big: String = (0..500).fold(String::new(), |mut acc, i| {
        use std::fmt::Write as _;
        let _ = writeln!(acc, "line {i}");
        acc
    });
    let outcome = rift_canopy::DispatchOutcome {
        code: 0,
        human: big,
        json: None,
    };
    s.app.overlay = Some(rift_canopy::Overlay::results(
        &rift_canopy::DispatchRequest::Status,
        &outcome,
    ));
    // Scroll to the truncation notice at the tail.
    if let Some(rift_canopy::Overlay::Results { scroll, lines, .. }) = s.app.overlay.as_mut() {
        *scroll = lines.len().saturating_sub(5);
    }
    insta::assert_snapshot!("overlay_truncated", render_state(&mut s));
}

#[test]
fn golden_overlay_confirm_prompt() {
    let mut s = dashboard_state();
    s.size = Size::new(100, 32);
    s.app.overlay = Some(rift_canopy::Overlay::Confirm {
        request: rift_canopy::DispatchRequest::ProjectsPrune,
        repo: "/r/rift-cli".into(),
    });
    insta::assert_snapshot!("overlay_confirm", render_state(&mut s));
}

// ---- the cartesian axis frame ---------------------------------------------

#[test]
fn golden_axis_frame() {
    // The shared axis helper over a token-usage fixture: a Y-tick gutter
    // (0 / mid / max humanized), an X-class row, and a class-colour legend.
    use rift_canopy::axis::{draw, AxisSpec};
    use rift_canopy::fmt_km;
    use rift_canopy::theme::{Theme, BREAKDOWN};
    let x: Vec<String> = ["input", "read", "output", "cache"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let legend = [
        (BREAKDOWN[0], "input"),
        (BREAKDOWN[1], "read"),
        (BREAKDOWN[2], "output"),
        (BREAKDOWN[3], "cache"),
    ];
    let spec = AxisSpec {
        max: 1_600_000.0,
        humanize: fmt_km,
        x_axis: rift_canopy::axis::XAxis::Categories(&x),
        legend: &legend,
    };
    let mut f = Frame::new(Size::new(40, 10));
    let r = rift_canopy::Rect {
        x: 0,
        y: 0,
        w: 40,
        h: 10,
    };
    let _plot = draw(&mut f, Theme::rift_dark(), r, &spec, true);
    insta::assert_snapshot!("axis_frame", frame_text(&f));
}

#[test]
fn golden_activity_heat_strips() {
    // A dense month rendered as three independently-scaled, peak-preserving
    // heat strips with the fixed relative-time scale.
    use rift_canopy::cards::{self, CardCtx, CardKind};
    use rift_canopy::{ActivityData, ActivityView, DayBar, DetailView};
    let detail = DetailView::thin("/r/rift-cli".into(), "rift-cli".into(), ProjectStatus::Idle);
    let days = ActivityView::Days(ActivityData {
        days: (0..30usize)
            .map(|i| {
                let n = u64::try_from(i).unwrap_or(0);
                DayBar {
                    weekday: "Mon",
                    events: if i == 8 { 120 } else { n * 2 },
                    sessions: if i == 22 { 18 } else { n / 3 },
                    checkpoints: n % 5,
                }
            })
            .collect(),
        ..ActivityData::default()
    });
    let ctx = CardCtx {
        detail: &detail,
        activity: &days,
        focused: false,
        unicode: false,
        pressed: None,
        press_pulse: 0.0,
    };
    // Render the full card stack and snapshot the activity card (its own slot).
    let area = rift_canopy::Rect {
        x: 0,
        y: 0,
        w: 44,
        h: 44,
    };
    let slots = cards::solve(&CardKind::ALL, area);
    let activity = slots.iter().find(|s| s.kind == CardKind::Activity);
    let (y0, y1) = activity.map_or((0u16, 44), |s| (s.rect.y, s.rect.y + s.rect.h));
    let mut f = Frame::new(Size::new(44, 44));
    let mut b = AsciiBackend::new(Capabilities::empty());
    cards::draw(
        &ctx,
        rift_canopy::theme::Theme::rift_dark(),
        &CardKind::ALL,
        area,
        0,
        &mut b,
        &mut f,
    );
    // Snapshot just the activity slot's rows.
    let mut out = String::new();
    for y in y0..y1 {
        for x in 0..f.width() {
            out.push(f.get(x, y).map_or(' ', |c| c.ch));
        }
        out.push('\n');
    }
    insta::assert_snapshot!("activity_heat_strips", out.trim_end());
}

#[test]
fn golden_model_usage_treemap_responsive_states() {
    use rift_canopy::cards::{self, CardCtx, CardKind};
    use rift_canopy::{ActivityView, DetailView, ModelProvenance, ModelUsage};

    let mut detail = DetailView::thin("/r/rift-cli".into(), "rift-cli".into(), ProjectStatus::Idle);
    detail.models = vec![
        ModelUsage {
            model: "claude-opus-4-1".into(),
            calls: 120,
            source: ModelProvenance::Runtime,
        },
        ModelUsage {
            model: "gpt-5-codex".into(),
            calls: 62,
            source: ModelProvenance::Runtime,
        },
        ModelUsage {
            model: "gemini-2.5-pro".into(),
            calls: 31,
            source: ModelProvenance::Runtime,
        },
        ModelUsage {
            model: "local-qwen".into(),
            calls: 9,
            source: ModelProvenance::Runtime,
        },
        ModelUsage {
            model: "configured-next".into(),
            calls: 0,
            source: ModelProvenance::Configured,
        },
    ];
    let activity = ActivityView::NoProjection;

    let render = |w: u16, h: u16, collapsed: bool| {
        let mut frame = Frame::new(Size::new(w, h));
        let mut backend = AsciiBackend::new(Capabilities::empty());
        cards::draw_one(
            &CardCtx {
                detail: &detail,
                activity: &activity,
                focused: false,
                unicode: false,
                pressed: None,
                press_pulse: 0.0,
            },
            rift_canopy::theme::Theme::rift_dark(),
            CardKind::Models,
            rift_canopy::Rect { x: 0, y: 0, w, h },
            collapsed,
            &mut backend,
            &mut frame,
        );
        frame_text(&frame)
    };

    let output = format!(
        "expanded\n{}\n\nfallback\n{}\n\ncollapsed\n{}",
        render(48, 13, false),
        render(22, 7, false),
        render(32, 3, true),
    );
    insta::assert_snapshot!("model_usage_treemap_responsive", output);
}

#[test]
fn golden_overlay_wizard_modal() {
    // The first-run wizard as a modal: message + the add-project chip.
    let mut s = AppState::new(vec![], Size::new(100, 32));
    s.app.splash.skip();
    s.size = Size::new(100, 32);
    insta::assert_snapshot!("overlay_wizard", render_state(&mut s));
}

#[test]
fn golden_overlay_alert_error() {
    let mut s = dashboard_state();
    s.size = Size::new(100, 32);
    s.app.overlay = Some(rift_canopy::Overlay::alert(
        "verify failed",
        vec![
            "chain: BROKEN at #1".into(),
            "run `rift verify` for detail".into(),
        ],
        rift_canopy::Severity::Error,
    ));
    insta::assert_snapshot!("overlay_alert_error", render_state(&mut s));
}

#[test]
fn golden_overlay_notice_success() {
    let mut s = dashboard_state();
    s.size = Size::new(100, 32);
    s.app.overlay = Some(rift_canopy::Overlay::notice(
        "registry",
        vec!["registered /r/rift-cli".into()],
    ));
    insta::assert_snapshot!("overlay_notice", render_state(&mut s));
}

#[test]
fn golden_inline_parse_error() {
    let mut s = dashboard_state();
    s.size = Size::new(100, 32);
    s.app.mode = rift_canopy::Mode::Command;
    s.app.command_line = "statsu".into();
    s.app.command_error = rift_canopy::parse("statsu").err();
    insta::assert_snapshot!("inline_parse_error", render_state(&mut s));
}

// --- Full3d (the mesh splash tier): the mesh raster -------------------------

#[test]
fn golden_full3d_final_frame() {
    // The braille-density resolve of the settled scene (skip parity lands here).
    insta::assert_snapshot!(
        "splash_full3d_final",
        full3d_text(1.0, full3d_caps(), Resolution::Full)
    );
}

#[test]
fn golden_full3d_midframe() {
    insta::assert_snapshot!(
        "splash_full3d_mid",
        full3d_text(0.5, full3d_caps(), Resolution::Full)
    );
}

#[test]
fn golden_full3d_final_halfblock_fallback() {
    // Forced caps without SUBCELL: the same scene resolved as half-blocks —
    // the "where cell caps demand" degradation, pinned.
    let caps = Capabilities::TRUECOLOR
        | Capabilities::UNICODE
        | Capabilities::MESH_3D
        | Capabilities::RAMP_SHADER;
    insta::assert_snapshot!(
        "splash_full3d_final_halfblock",
        full3d_text(1.0, caps, Resolution::Full)
    );
}

#[test]
fn golden_full3d_points_keyframes() {
    // The mesh splash tier contract: Full3d keyframes are deterministic point sets —
    // pure functions of (t, seed, resolution, size); injected clock upstream.
    insta::assert_snapshot!("full3d_points_t25", point_set_text(0.25));
    insta::assert_snapshot!("full3d_points_t50", point_set_text(0.5));
    insta::assert_snapshot!("full3d_points_t100", point_set_text(1.0));
}

// ---- the suggest panel (full / filtered / argument / no-match) -------------

#[test]
fn golden_suggest_full_palette() {
    insta::assert_snapshot!("suggest_full", render_state(&mut suggest_state("")));
}

#[test]
fn golden_suggest_filtered() {
    insta::assert_snapshot!("suggest_filtered", render_state(&mut suggest_state("se")));
}

#[test]
fn golden_suggest_argument() {
    insta::assert_snapshot!(
        "suggest_argument",
        render_state(&mut suggest_state("select "))
    );
}

#[test]
fn golden_suggest_no_match() {
    insta::assert_snapshot!(
        "suggest_no_match",
        render_state(&mut suggest_state("zzzzzz"))
    );
}
