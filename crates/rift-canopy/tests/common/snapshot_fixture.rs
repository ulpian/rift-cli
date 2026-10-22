//! The deterministic inputs behind `snapshots.rs`: the seeded `AppState`s, the
//! populated view models and the render helpers that turn a frame into golden
//! text. They live beside the suite rather than inside it so the assertions
//! read as a list of what is pinned, and so a fixture change is a diff a
//! reviewer can read without scrolling past sixty goldens.
//!
//! Everything here is a pure function of its arguments: no clock, no RNG, no
//! terminal is ever consulted, which is what makes every golden stable.

use std::sync::Arc;

use rift_canopy::render::frame::{Frame, Size};
use rift_canopy::render::Capabilities;
use rift_canopy::splash::director::Tier;
use rift_canopy::splash::mesh3d::{self, Resolution};
use rift_canopy::splash::render2d::draw_tier;
use rift_canopy::state::AppState;
use rift_canopy::{
    AsciiBackend, EntryKind, Project, ProjectStatus, ViewerCheckpointRow, ViewerDetail, ViewerLine,
    ViewerLineKind, ViewerLoadState, ViewerOrigin, ViewerTimelineItem, ViewerTimelineSegment,
    ViewerTimestampProvenance,
};

// --- frames to golden text -------------------------------------------------

/// Render a frame to a trailing-trimmed text block (stable across runs).
pub(crate) fn frame_text(frame: &Frame) -> String {
    let mut lines = Vec::new();
    for y in 0..frame.height() {
        let mut line = String::new();
        for x in 0..frame.width() {
            line.push(frame.get(x, y).map_or(' ', |c| c.ch));
        }
        lines.push(line.trim_end().to_string());
    }
    // Drop trailing blank lines for a compact golden.
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// Render `state` at its declared terminal size through the ASCII backend.
pub(crate) fn render_state(s: &mut AppState) -> String {
    let vm = s.snapshot();
    let mut frame = Frame::new(s.size);
    let mut backend = AsciiBackend::new(Capabilities::empty());
    rift_canopy::view::draw(&vm, &mut backend, &mut frame);
    frame_text(&frame)
}

/// The same render with a declared capability set, for the goldens whose
/// point is a glyph the ASCII floor cannot draw.
pub(crate) fn render_state_with_caps(state: &mut AppState, caps: Capabilities) -> String {
    let mut frame = Frame::new(state.size);
    let mut backend = AsciiBackend::new(caps);
    rift_canopy::view::draw(&state.snapshot(), &mut backend, &mut frame);
    frame_text(&frame)
}

// --- the dashboard every fixture starts from ------------------------------

pub(crate) fn dashboard_state() -> AppState {
    let mut s = AppState::new(
        vec![
            Project {
                name: "rift-cli".into(),
                path: "/r/rift-cli".into(),
                status: ProjectStatus::Running {
                    since_ms: 0,
                    active_sessions: 1,
                },
                kind: EntryKind::Repo,
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
    s
}

/// Render `dashboard_state()` at `size` and return the golden text (one
/// degradation mode per size).
pub(crate) fn render_at(w: u16, h: u16) -> String {
    let mut state = dashboard_state();
    state.size = Size::new(w, h);
    let mut f = Frame::new(state.size);
    let mut backend = AsciiBackend::new(Capabilities::empty());
    rift_canopy::view::draw(&state.snapshot(), &mut backend, &mut f);
    frame_text(&f)
}

/// A dashboard editing the given command buffer (suggest panel open).
pub(crate) fn suggest_state(buffer: &str) -> AppState {
    let mut s = dashboard_state();
    s.size = Size::new(100, 32);
    s.app.mode = rift_canopy::Mode::Command;
    s.app.command_line = buffer.to_string();
    s // suggest_fade rests at 1.0 → the settled (final) frame
}

// --- Project Settings ------------------------------------------------------

pub(crate) fn project_settings_state(
    size: Size,
    initialization: rift_canopy::ProjectInitialization,
    policy: rift_canopy::ProjectPolicyState,
) -> AppState {
    project_settings_state_with_codex_hooks(
        size,
        initialization,
        policy,
        rift_canopy::NativeAgentHookWiring::Managed,
    )
}

pub(crate) fn project_settings_state_with_codex_hooks(
    size: Size,
    initialization: rift_canopy::ProjectInitialization,
    policy: rift_canopy::ProjectPolicyState,
    codex_hooks: rift_canopy::NativeAgentHookWiring,
) -> AppState {
    let mut state = dashboard_state();
    state.size = size;
    state
        .app
        .project_settings
        .open(rift_canopy::ProjectSettingsView {
            permissions: Vec::new(),
            path: "/r/rift-cli".into(),
            project_name: "rift-cli".into(),
            initialization,
            settings: setting_rows(initialization, policy),
            policy: policy_summary(policy),
            integrations: integration_rows(codex_hooks),
        });
    state.app.dashboard_tab = rift_canopy::dashboard::DashboardTab::Settings;
    state.app.mode = rift_canopy::Mode::ProjectSettings;
    state
}

/// Every setting the page lists, with the capture plane ticked on and one row
/// carrying a detail line, so the goldens cover both row shapes.
///
/// `actionable` is the whole point of the fixture taking both arguments: a
/// policy the loader refused and a checkout that is not set up here each make
/// the same row un-actionable, by different routes.
fn setting_rows(
    initialization: rift_canopy::ProjectInitialization,
    policy: rift_canopy::ProjectPolicyState,
) -> Vec<rift_canopy::ProjectSettingRow> {
    use rift_canopy::{ProjectSettingId, ProjectSettingRow};

    ProjectSettingId::ALL
        .iter()
        .map(|id| ProjectSettingRow {
            id: *id,
            enabled: matches!(
                id,
                ProjectSettingId::TotalRecall
                    | ProjectSettingId::TurnInjection
                    | ProjectSettingId::Documents
                    | ProjectSettingId::Search
                    | ProjectSettingId::ToolCallCapture
                    | ProjectSettingId::TokenUsageCapture
            ),
            actionable: !matches!(
                policy,
                rift_canopy::ProjectPolicyState::Invalid | rift_canopy::ProjectPolicyState::Refused
            ) && initialization.settings_editable(),
            detail: (*id == ProjectSettingId::TurnInjection)
                .then(|| "custom effective budget: 777".into()),
        })
        .collect()
}

/// The policy card. The repair sentence belongs to an invalid config file and
/// to nothing else, so it is derived here rather than passed in.
fn policy_summary(policy: rift_canopy::ProjectPolicyState) -> rift_canopy::ProjectPolicySummary {
    rift_canopy::ProjectPolicySummary {
        state: policy,
        detail: matches!(policy, rift_canopy::ProjectPolicyState::Invalid)
            .then(|| ".rift/config.toml is invalid; repair it before changing settings".into()),
        sensitive_path_count: 11,
        loaded_redactor_packs: 2,
        failed_redactor_packs: 1,
    }
}

/// One row per adapter in registry order, seeded so a single frame carries
/// every wiring state the page can draw: managed, conflicting, invalid and
/// missing, over detected and undetected rows, with one row installed twice
/// (a `PATH` command and a verified extension) to pin the plural spelling.
fn integration_rows(
    codex_hooks: rift_canopy::NativeAgentHookWiring,
) -> Vec<rift_canopy::NativeAgentIntegrationRow> {
    use rift_canopy::{
        NativeAgentDetection as Detection, NativeAgentIdeHost as IdeHost,
        NativeAgentInstallSource as Source, NativeAgentInstallation as Installation,
        NativeAgentIntegrationRow, NativeAgentWiring as Wiring,
    };

    let agents = [
        ("claude-code", "Claude Code", "claude"),
        ("codex", "Codex", "codex"),
        ("gemini", "Gemini CLI", "gemini"),
        ("antigravity", "Antigravity", "agy"),
        ("cursor", "Cursor", "cursor"),
        ("copilot-cli", "Copilot CLI", "copilot"),
        ("opencode", "OpenCode", "opencode"),
        ("factory-droid", "Factory Droid", "droid"),
        ("ollama", "Ollama", "ollama"),
    ];
    agents
        .iter()
        .enumerate()
        .map(
            |(index, (agent, display_name, command))| NativeAgentIntegrationRow {
                agent: (*agent).into(),
                display_name: (*display_name).into(),
                detection: if index < 4 {
                    Detection::Detected
                } else {
                    Detection::NotDetected
                },
                executable_name: (index < 4).then(|| (*command).into()),
                installations: (index < 4)
                    .then_some(Installation {
                        source: Source::PathCommand,
                        version: None,
                    })
                    .into_iter()
                    .chain((*agent == "codex").then(|| Installation {
                        source: Source::IdeExtension(IdeHost::VsCode),
                        version: Some("26.803.61601".into()),
                    }))
                    .collect(),
                wiring: match index {
                    0 | 1 => Wiring::Managed,
                    2 => Wiring::Conflict,
                    3 => Wiring::Invalid,
                    _ => Wiring::Missing,
                },
                hook_wiring: (*agent == "codex").then_some(codex_hooks),
                // The alpha capture scope; single-sourced for real rows from
                // the registry-side list (`ALPHA_AGENTS` in
                // `crates/rift-shadow-hooks/src/lib.rs`).
                alpha_supported: matches!(*agent, "claude-code" | "codex" | "antigravity"),
                selected: index < 2,
                remedies: Vec::new(),
            },
        )
        .collect()
}

// --- the Recall viewer -----------------------------------------------------

/// The viewer with a two-row catalogue and a four-segment detail pane: enough
/// distinct shapes that every responsive tier has something to lay out.
pub(crate) fn recall_viewer_state(size: Size, pane: rift_canopy::ViewerPane) -> AppState {
    let mut state = dashboard_state();
    state.size = size;
    let _ = state.open_recall_viewer(None);
    state.app.viewer.pane = pane;
    state.app.viewer.catalogue_state = ViewerLoadState::Ready;
    state.app.viewer.detail_state = ViewerLoadState::Ready;
    state.app.viewer.catalogue = Arc::new(vec![
        projected_checkpoint_row(),
        unprojected_checkpoint_row(),
    ]);
    state.app.viewer.visible = vec![0, 1];
    state.app.viewer.detail = Arc::new(viewer_detail());
    state
}

/// A checkpoint the projection has seen: a recorded timestamp, two origins in
/// two dialects, a chain position, local branches and a recorded issue — the
/// row where every optional field the catalogue can draw is present.
fn projected_checkpoint_row() -> ViewerCheckpointRow {
    ViewerCheckpointRow {
        checkpoint_id: "a1b2c3d4e5f6789012345678901234567890abcd".into(),
        effective_at_ms: Some(1_755_000_000_000),
        timestamp_provenance: ViewerTimestampProvenance::Recorded,
        compact_utc: Some("2025-08-11T10:40Z".into()),
        excerpt: Some("Add read-only Recall Viewer fixtures".into()),
        excerpt_loading: false,
        anchor_commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        origins: vec![
            ViewerOrigin {
                storage_ref: "refs/heads/rift/checkpoints/v1".into(),
                dialect: "rift".into(),
                kind: "branch".into(),
                tip_oid: Some("1111111111111111111111111111111111111111".into()),
            },
            ViewerOrigin {
                storage_ref: "refs/legacy/checkpoints/a1b2c3d4".into(),
                dialect: "legacy".into(),
                kind: "per-checkpoint-ref".into(),
                tip_oid: Some("2222222222222222222222222222222222222222".into()),
            },
        ],
        chain_position: Some(42),
        local_branches: vec!["main".into(), "release/m5".into()],
        strategy: Some("compact".into()),
        projection: "partial".into(),
        issues: vec!["tasks: one malformed record".into()],
        metadata_search: String::new(),
        worktree: None,
    }
}

/// The counterpart row: an unprojected checkpoint whose time is borrowed from
/// its anchor commit and whose excerpt is still loading, so the goldens carry
/// the absent-field spellings beside the present ones.
fn unprojected_checkpoint_row() -> ViewerCheckpointRow {
    ViewerCheckpointRow {
        checkpoint_id: "99887766554433221100ffeeddccbbaa00998877".into(),
        effective_at_ms: Some(1_754_900_000_000),
        timestamp_provenance: ViewerTimestampProvenance::AnchorCommit,
        compact_utc: Some("2025-08-10T06:53Z".into()),
        excerpt: None,
        excerpt_loading: true,
        anchor_commit: Some("fedcba9876543210fedcba9876543210fedcba98".into()),
        origins: vec![ViewerOrigin {
            storage_ref: "refs/heads/legacy/checkpoints/v1".into(),
            dialect: "legacy".into(),
            kind: "branch".into(),
            tip_oid: None,
        }],
        chain_position: None,
        local_branches: Vec::new(),
        strategy: None,
        projection: "unprojected".into(),
        issues: Vec::new(),
        metadata_search: String::new(),
        worktree: None,
    }
}

/// The detail pane for the projected row: the four segments the viewer draws,
/// in the order it draws them.
fn viewer_detail() -> ViewerDetail {
    ViewerDetail {
        checkpoint_id: "a1b2c3d4e5f6789012345678901234567890abcd".into(),
        segments: vec![
            checkpoint_summary_segment(),
            recall_summary_segment(),
            transcript_segment(),
            assembly_segment(),
        ],
        notice: None,
    }
}

/// Facts about the checkpoint itself, over the three line kinds the summary
/// column styles differently.
fn checkpoint_summary_segment() -> ViewerTimelineSegment {
    ViewerTimelineSegment {
        title: "Checkpoint summary".into(),
        boundary: Some("facts grouped by field; not an event chronology".into()),
        ordered_by_time: false,
        items: vec![ViewerTimelineItem {
            id: "checkpoint-summary".into(),
            title: "checkpoint a1b2c3d4e5f6".into(),
            timestamp: Some("2025-08-11T10:40Z".into()),
            summary: vec![
                ViewerLine::plain(ViewerLineKind::Heading, "anchor: 0123456789ab"),
                ViewerLine::plain(ViewerLineKind::Text, "chain: #42 · strategy: compact"),
                ViewerLine::plain(ViewerLineKind::Warning, "partial projection"),
            ],
            body: Vec::new(),
            expanded_body: None,
            collapsed: false,
            full_session_only: false,
        }],
    }
}

/// Repository-wide Recall totals, which the segment's boundary line marks as
/// not being checkpoint events.
fn recall_summary_segment() -> ViewerTimelineSegment {
    ViewerTimelineSegment {
        title: "Recall summary".into(),
        boundary: Some("repository-wide totals are not checkpoint events".into()),
        ordered_by_time: false,
        items: vec![ViewerTimelineItem {
            id: "recall-summary".into(),
            title: "durable Recall state".into(),
            timestamp: None,
            summary: vec![ViewerLine::plain(
                ViewerLineKind::Text,
                "coverage: 2/2 IDs · 3 origins · 8 serves",
            )],
            body: Vec::new(),
            expanded_body: None,
            collapsed: false,
            full_session_only: false,
        }],
    }
}

/// One redacted transcript turn: the segment shape that carries a body and no
/// summary at all.
fn transcript_segment() -> ViewerTimelineSegment {
    ViewerTimelineSegment {
        title: "Transcript".into(),
        boundary: Some("redacted compact artifact order only".into()),
        ordered_by_time: false,
        items: vec![ViewerTimelineItem {
            id: "transcript:2".into(),
            title: "user".into(),
            timestamp: None,
            summary: Vec::new(),
            body: vec![ViewerLine::plain(
                ViewerLineKind::Text,
                "Show the checkpoint-new transcript slice.",
            )],
            expanded_body: None,
            collapsed: false,
            full_session_only: false,
        }],
    }
}

/// A one-file diff: a hunk header, a removal and an addition, so the goldens
/// pin the gutter numbering and the syntax classes together.
fn assembly_segment() -> ViewerTimelineSegment {
    ViewerTimelineSegment {
        title: "Checkpoint assembly".into(),
        boundary: Some("ordering unavailable".into()),
        ordered_by_time: false,
        items: vec![ViewerTimelineItem {
            id: "diff:src/viewer.rs".into(),
            title: "src/viewer.rs · +1 -1".into(),
            timestamp: None,
            summary: vec![ViewerLine::plain(
                ViewerLineKind::DiffFile,
                "src/viewer.rs · +1 -1",
            )],
            body: vec![
                diff_line(
                    None,
                    None,
                    ViewerLineKind::DiffHunk,
                    "@@ -12,4 +12,4 @@",
                    rift_canopy::SyntaxClass::Plain,
                ),
                diff_line(
                    Some(13),
                    None,
                    ViewerLineKind::Removal,
                    "let mode = Writable;",
                    rift_canopy::SyntaxClass::Keyword,
                ),
                diff_line(
                    None,
                    Some(13),
                    ViewerLineKind::Addition,
                    "let mode = ReadOnly;",
                    rift_canopy::SyntaxClass::Keyword,
                ),
            ],
            expanded_body: None,
            collapsed: false,
            full_session_only: false,
        }],
    }
}

/// One diff line with its gutter numbers and a single styled span.
fn diff_line(
    old_line: Option<u32>,
    new_line: Option<u32>,
    kind: ViewerLineKind,
    text: &str,
    class: rift_canopy::SyntaxClass,
) -> ViewerLine {
    ViewerLine {
        old_line,
        new_line,
        kind,
        spans: vec![rift_canopy::ViewerSpan {
            text: text.into(),
            class,
        }],
    }
}

// --- column 1: the member-row rail -----------------------------------------

/// A capability-bearing backend for the member-row goldens.
///
/// [`AsciiBackend`] reports the empty set whatever it is constructed with, so
/// it cannot render `◔` — and the whole point of a `NotAdopted` golden is the
/// glyph a reader actually sees. Present is a no-op (headless).
struct CapsBackend(Capabilities);

impl rift_canopy::RenderBackend for CapsBackend {
    fn name(&self) -> &'static str {
        "caps"
    }
    fn capabilities(&self) -> Capabilities {
        self.0
    }
    fn present(&mut self, _frame: &Frame) -> Result<(), rift_canopy::render::RenderError> {
        Ok(())
    }
}

/// The five member-row states the family view names, under one repository row.
fn member_row_projects() -> Vec<Project> {
    vec![
        Project {
            name: "rift-cli".into(),
            path: "/r/rift-cli".into(),
            status: ProjectStatus::Running {
                since_ms: 0,
                active_sessions: 3,
            },
            kind: EntryKind::Repo,
        },
        Project {
            name: "swt-a".into(),
            path: "/w/swt-a".into(),
            status: ProjectStatus::Running {
                since_ms: 0,
                active_sessions: 2,
            },
            kind: EntryKind::Worktree,
        },
        Project {
            name: "swt-b".into(),
            path: "/w/swt-b".into(),
            status: ProjectStatus::NotAdopted,
            kind: EntryKind::Worktree,
        },
        Project {
            name: "swt-c".into(),
            path: "/w/swt-c".into(),
            status: ProjectStatus::SetupIncomplete,
            kind: EntryKind::Worktree,
        },
        Project {
            name: "swt-d".into(),
            path: "/w/swt-d".into(),
            status: ProjectStatus::NotProvisioned,
            kind: EntryKind::Repo,
        },
        Project {
            name: "swt-e".into(),
            path: "/w/swt-e".into(),
            status: ProjectStatus::NotRepository,
            kind: EntryKind::Repo,
        },
        Project {
            name: "swt-gone".into(),
            path: "/w/swt-gone".into(),
            status: ProjectStatus::Error(rift_canopy::StatusError::RepoMissing),
            kind: EntryKind::Worktree,
        },
    ]
}

/// Render the member-row rail with `selected` in focus and `detail` delivered
/// for it, at `caps`.
pub(crate) fn member_row_frame(
    selected: usize,
    detail: rift_canopy::DetailView,
    caps: Capabilities,
) -> Frame {
    // The row's own status and path come from the detail, which carries both:
    // the two must agree anyway, and taking them from one place is how the
    // fixture cannot describe a row the reducer would refuse to attach.
    let status = detail.status;
    let path = detail.path.clone();
    let mut s = AppState::new(member_row_projects(), Size::new(120, 40));
    s.app.splash.skip();
    s.app.selected = selected;
    rift_canopy::update(
        &mut s,
        rift_canopy::Event::ProjectData(rift_canopy::ProjectData {
            project: path,
            status,
            detail: Some(detail),
        }),
    );
    let mut f = Frame::new(s.size);
    let mut b = CapsBackend(caps);
    rift_canopy::view::draw(&s.snapshot(), &mut b, &mut f);
    f
}

/// One detail view, with whatever running members and kind the case needs.
pub(crate) fn member_row_detail(
    label: &str,
    path: &str,
    kind: EntryKind,
    status: ProjectStatus,
    running_members: Vec<String>,
) -> rift_canopy::DetailView {
    rift_canopy::DetailView {
        git_summary: None,
        path: path.to_owned(),
        label: label.to_owned(),
        kind,
        status,
        models: Vec::new(),
        capture: rift_canopy::CaptureSummary::default(),
        running_members,
        project_settings: Box::new(rift_canopy::ProjectSettingsView::thin(
            path.to_owned(),
            label.to_owned(),
        )),
    }
}

// --- column 3: the six-section stack ---------------------------------------

/// The Recall card's figures, carried by every populated fixture below.
///
/// The card's headline **is** this share, so the base fixture carries one:
/// without it every golden here would draw the honest-absence card
/// (`saved —`, no bars) and the drawn one would have no coverage at all.
fn savings_fixture() -> rift_canopy::SavingsData {
    rift_canopy::SavingsData {
        by_worktree: rift_canopy::WorktreeAxis {
            rows: Vec::new(),
            family_only: 6_000,
        },
        share: Box::new(rift_canopy::ShareData {
            overall: Some(42.0),
            honesty: 5_400,
            avoided_tokens: 6_000,
            read_tokens: 8_285,
            sessions_with_reads: 3,
            ..rift_canopy::ShareData::default()
        }),
        ..rift_canopy::SavingsData::default()
    }
}

/// A fully-populated Col3 (every section carrying data), assembled from one
/// builder per panel so a golden that moves names the panel it came from.
pub(crate) fn col3_with_data() -> rift_canopy::Col3 {
    rift_canopy::Col3 {
        watch: rift_canopy::WatchMode::Native,
        tokens: rift_canopy::TokensView::Data(tokens_fixture()),
        breakdown: rift_canopy::BreakdownView::Data(breakdown_fixture()),
        nic: rift_canopy::NicView::Reading(nic_reading_fixture()),
        savings: rift_canopy::RecallView::Data(savings_fixture()),
        calls: rift_canopy::RecallView::Data(calls_fixture()),
        active_sessions: rift_canopy::ActiveSessionsView::Count(2),
        recall_window: rift_canopy::RecallWindow::AllTime,
        metrics: metrics_fixture(),
        dispatch: rift_canopy::DispatchView::Data(dispatch_fixture()),
        recap: rift_canopy::RecapView::Data(recap_fixture()),
    }
}

/// A token-rate series whose peaks are far enough apart that the sparkline's
/// autoscale has something to do.
fn tokens_fixture() -> rift_canopy::TokensData {
    rift_canopy::TokensData {
        rate: vec![120.0, 900.0, 300.0, 1500.0, 700.0, 2500.0, 400.0, 1100.0],
        total: 7520.0,
        by_worktree: rift_canopy::WorktreeAxis::default(),
    }
}

/// The four token classes, read-heavy — the shape the breakdown card exists to
/// show, and the one the read fraction is derived from.
fn breakdown_fixture() -> rift_canopy::BreakdownData {
    rift_canopy::BreakdownData {
        input: 5200.0,
        read: 48_000.0,
        output: 9100.0,
        cache_write: 3300.0,
        read_fraction: 48_000.0 / 53_200.0,
    }
}

/// A context-window reading in the middle band, where the gauge draws both a
/// filled and an empty arc.
fn nic_reading_fixture() -> rift_canopy::NicReading {
    rift_canopy::NicReading {
        pct: 62,
        band: "Moderate",
        model: "claude-fable-5".into(),
        active_sessions: 2,
    }
}

/// Serve calls on one channel, over a full thirty-bucket window.
fn calls_fixture() -> rift_canopy::CallsData {
    rift_canopy::CallsData {
        total: 17,
        window_end_ms: 1_800_000,
        series: vec![rift_canopy::ChannelCallSeries {
            channel: "hook_inject".into(),
            last_call_at: 1_799_000,
            rate: [1.0, 3.0, 2.0, 5.0, 4.0, 2.0]
                .into_iter()
                .cycle()
                .take(30)
                .collect(),
        }],
    }
}

/// The System panel's figures, every one of them present, both scopes reading
/// the sessions running now.
fn metrics_fixture() -> rift_canopy::PanelMetrics {
    rift_canopy::PanelMetrics {
        events_5m: Some(42),
        tokens_5m: Some(12_400),
        serve_calls_5m: Some(7),
        projection_bytes: Some(18_600_000),
        context_used: Some(124_000),
        context_window: Some(200_000),
        context_scope: rift_canopy::SystemScope::CurrentSessions,
        activity_scope: rift_canopy::SystemScope::CurrentSessions,
    }
}

/// A workflow panel with an attribution split and two touched files, one of
/// them sensitive, so the redaction marker is drawn beside an ordinary row.
fn dispatch_fixture() -> rift_canopy::DispatchData {
    rift_canopy::DispatchData {
        checkpoints: 3,
        derived: 1,
        unresolved: 1,
        sessions: 3,
        open_sessions: 2,
        agent_lines: 35,
        mixed_lines: 3,
        human_lines: 5,
        confidence_milli: Some(700),
        files_total: 2,
        files: vec![
            rift_canopy::FileTouch {
                label: "\"src/a.rs\"".into(),
                checkpoints: 2,
                sensitive: false,
            },
            rift_canopy::FileTouch {
                label: "\".env\"".into(),
                checkpoints: 1,
                sensitive: true,
            },
        ],
        sensitive: Some(1),
    }
}

/// The session recap, carrying a cost label and a two-agent split.
fn recap_fixture() -> rift_canopy::RecapData {
    rift_canopy::RecapData {
        sessions: 3,
        checkpoints: 3,
        tokens: 414,
        cost_label: Some("~<$0.01".into()),
        by_model: false,
        split_total: 2,
        split: vec![("\"claude-code\"".into(), 2), ("\"codex\"".into(), 1)],
        savings: Some(rift_canopy::RecapSavings {
            avoided_upper_bound: 1_000,
            injected: 40,
        }),
    }
}

/// Render just a col3 area of the given height from `col3` (the panels
/// fill the area directly — no outer border, like the column-2 cards).
pub(crate) fn col3_text(col3: &rift_canopy::Col3, h: u16) -> String {
    let mut f = Frame::new(Size::new(34, h));
    let mut b = AsciiBackend::new(Capabilities::empty());
    let area = f.rect();
    rift_canopy::col3::draw(
        col3,
        &rift_canopy::col3::SectionId::ALL,
        false,
        false,
        area,
        0,
        &mut b,
        &mut f,
    );
    frame_text(&f)
}

/// Render the System surface alone, at a size and collapse state of its own,
/// so a degraded row cannot hide behind the column stack's scroll viewport.
pub(crate) fn system_text(col3: &rift_canopy::Col3, w: u16, h: u16, collapsed: bool) -> String {
    let mut f = Frame::new(Size::new(w, h));
    let mut b = AsciiBackend::new(Capabilities::empty());
    rift_canopy::col3::draw_one(
        col3,
        rift_canopy::col3::SectionId::System,
        false,
        false,
        collapsed,
        f.rect(),
        &mut b,
        &mut f,
    );
    frame_text(&f)
}

/// Render the Recall surface alone so responsive states are not hidden behind
/// the column stack's scroll viewport.
pub(crate) fn recall_text(col3: &rift_canopy::Col3, w: u16, h: u16, collapsed: bool) -> String {
    let mut frame = Frame::new(Size::new(w, h));
    let mut backend = AsciiBackend::new(Capabilities::empty());
    let rect = frame.rect();
    rift_canopy::col3::draw_one(
        col3,
        rift_canopy::col3::SectionId::Recall,
        false,
        false,
        collapsed,
        rect,
        &mut backend,
        &mut frame,
    );
    frame_text(&frame)
}

// --- graph widgets: fixed series, block (floor) tier ------------------------

/// Render `points` into a bordered plot box and return the golden text.
pub(crate) fn widget_text(points: &[rift_canopy::plot::PlotPoint]) -> String {
    use rift_canopy::render::frame::Style;
    let mut f = Frame::new(Size::new(26, 8));
    let inner = f.border(f.rect(), "plot", Style::DEFAULT);
    rift_canopy::plot::draw_blocks(
        &mut f,
        inner,
        points,
        &rift_canopy::plot::Paint::Solid(Style::DEFAULT),
    );
    frame_text(&f)
}

pub(crate) const WIDGET_SERIES: [f64; 12] =
    [1.0, 3.0, 2.0, 6.0, 4.0, 9.0, 7.0, 5.0, 8.0, 2.0, 4.0, 6.0];

// --- Full3d (the mesh splash tier): the mesh raster -------------------------

/// The forced Full3d capability set (tests never read a real terminal).
pub(crate) fn full3d_caps() -> Capabilities {
    Capabilities::TRUECOLOR
        | Capabilities::UNICODE
        | Capabilities::SUBCELL
        | Capabilities::MESH_3D
        | Capabilities::RAMP_SHADER
}

/// The splash seed pinned by the goldens (the director's default).
const SPLASH_SEED: u64 = 0x5249_4654_5249_4654;

/// Render one Full3d frame at `(t, caps, res)` into a 48×16 golden.
pub(crate) fn full3d_text(t: f32, caps: Capabilities, res: Resolution) -> String {
    let mut b = AsciiBackend;
    let mut f = Frame::new(Size::new(48, 16));
    draw_tier(Tier::Full3d, t, SPLASH_SEED, &mut b, &mut f, caps, res);
    frame_text(&f)
}

/// Quantize a Full3d keyframe's point set into a stable text block: one
/// `x,y,lum` triple per point (positions ×1000, luminance ×100 — integers, so
/// the golden is float-format-proof), eight points per line.
pub(crate) fn point_set_text(t: f32) -> String {
    let points = mesh3d::scene_points(t, SPLASH_SEED, Resolution::Full, (48, 16));
    let mut triples = Vec::with_capacity(points.len());
    for p in &points {
        #[allow(clippy::cast_possible_truncation)]
        let (qx, qy, ql) = (
            (p.x * 1000.0).round() as i32,
            (p.y * 1000.0).round() as i32,
            (p.lum * 100.0).round() as i32,
        );
        triples.push(format!("{qx},{qy},{ql}"));
    }
    triples
        .chunks(8)
        .map(|c| c.join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}
