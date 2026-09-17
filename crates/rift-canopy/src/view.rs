//! The pure dashboard renderer: `view::draw(&ViewModel, backend, &mut Frame)`.
//!
//! The single renderer both backends flush — no `&mut AppState`, no I/O, no
//! engine types — so it is a deterministic function of the snapshot and every
//! panel step's headless/snapshot test reuses it. The backend participates
//! only through the [`RenderBackend::plot`] seam (sub-cell graphs choose
//! braille or the block floor there); everything else is pure [`Frame`]
//! drawing.

use crate::col2::sanitize_for_terminal;
use crate::layout::{
    bottom_chrome_rows, choose_mode, single_panel, two_col, LayoutMode, Rect, MIN_NOTICE_COLS,
    MIN_NOTICE_ROWS,
};
use crate::layout_model::ColumnRole;
use crate::render::frame::{Frame, Style};
use crate::render::{Capabilities, RenderBackend};
use crate::state::ViewModel;
use crate::ProjectInitialization as Init;
use crate::{EntryKind, Mode, Panel};

/// The Rift accent style (selection + overlay chrome) — the theme's accent.
const ACCENT: Style = Style::fg(crate::theme::ACCENT);

/// The one surface theme — code, not config.
/// Render the whole dashboard for `vm` into `frame`, choosing the responsive
/// layout tier for the current size. `backend` participates through
/// the plot seam (column-3 graphs) and, once at the top, the Unicode
/// capability — the single glyph-substitution switch the theme's icon table
/// resolves through (draw code never branches on capability elsewhere).
pub fn draw(vm: &ViewModel, backend: &mut dyn RenderBackend, frame: &mut Frame) {
    let area = frame.rect();
    if area.w == 0 || area.h == 0 {
        return;
    }
    let unicode = backend.capabilities().contains(Capabilities::UNICODE);

    // The surface paints ITS OWN background first (the runtime surface theme: the background
    // slot is every element's backdrop, not just the header's) — every
    // foreground-only write beneath inherits it through the frame's
    // transparent-Default compose rule, so a profile's background repaints
    // panels, gaps, and bars alike. Floor tiers resolve the colour away.
    crate::chrome::fill(frame, area, Style::DEFAULT.with_bg(vm.theme.surface_bg()));

    // Below the minimum, show only the centered notice (no header/panels).
    if choose_mode(vm.size) == LayoutMode::TooSmall {
        draw_too_small(frame, area);
        return;
    }

    let header = Rect {
        x: area.x,
        y: area.y,
        w: area.w,
        h: 1,
    };
    crate::bars::draw_header(vm, vm.theme, unicode, frame, header);
    let body = Rect {
        x: area.x,
        y: area.y + 1,
        w: area.w,
        h: area.h.saturating_sub(1),
    };

    if vm.surface == crate::recall_viewer::Surface::RecallViewer {
        crate::recall_viewer::draw(vm, frame, unicode);
    } else {
        match choose_mode(vm.size) {
            LayoutMode::TwoCol => draw_two_col(vm, backend, frame, body, unicode),
            LayoutMode::SinglePanel => draw_single(vm, backend, frame, body, unicode),
            // Full3Col (TooSmall handled above).
            _ => draw_full(vm, backend, frame, body, unicode),
        }
    }

    // The suggest panel expands ABOVE the command bar, borrowing body rows as
    // an overlay draw — the fixed command rows and the layout solver are
    // untouched; it clamps away on tiny terminals.
    if !vm.suggestions.is_empty() {
        draw_suggest_panel(vm, frame);
    }

    // The profile menu: a mode, not an overlay — a centered frame
    // over the shared modal geometry and scrim, its state on the snapshot.
    if vm.mode == crate::input::Mode::ProfileMenu {
        draw_profile_menu(vm, unicode, frame);
    }

    // The colour picker: two stages over the same modal geometry;
    // the dashboard beneath is the live preview.
    if vm.mode == crate::input::Mode::ColourPicker {
        draw_colour_picker(vm, unicode, frame);
    }

    // The destination explorer: the same modal geometry and scrim,
    // its state on the snapshot. Drawn before the overlay layer, so a success
    // notice paints on top of it exactly like it does over the dashboard.
    if vm.mode == crate::input::Mode::Explorer {
        draw_explorer(vm, unicode, frame);
    }

    if vm.mode == crate::input::Mode::ProjectSettings {
        draw_project_settings(vm, frame);
    }

    // The dispatch overlay layers over the body (the dispatch posture):
    // results scroll, confirms gate, the running line holds during a dispatch.
    // It sizes itself as a centered modal from `vm.size`.
    if vm.overlay.is_some() {
        draw_overlay(vm, unicode, frame);
    }

    // The one-shot startup sweep: a top-to-bottom reveal as the panels
    // first fill after the splash. At 1.0 (reduced motion / headless / settled)
    // it is a no-op; while sweeping, rows below the reveal line are blanked so
    // the dashboard wipes in rather than popping. Pure dt-tween — the settled
    // frame (which every headless/snapshot render sees) is always the full one.
    apply_startup_sweep(vm, frame, body);
}

/// Blank the body rows below the sweep's reveal line (a no-op at rest).
fn apply_startup_sweep(vm: &ViewModel, frame: &mut Frame, body: Rect) {
    let sweep = vm.sweep.clamp(0.0, 1.0);
    if sweep >= 1.0 || body.h == 0 {
        return;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let revealed = (f32::from(body.h) * sweep) as u16;
    for dy in revealed..body.h {
        for dx in 0..body.w {
            frame.set(
                body.x + dx,
                body.y + dy,
                ' ',
                Style::DEFAULT.with_bg(vm.theme.surface_bg()),
            );
        }
    }
}

/// The suggest panel's row cap (a bounded list; adapts down to the body).
pub const SUGGEST_MAX_ROWS: usize = 8;

/// Where the suggest panel sits: directly above the bottom chrome (hint bar +
/// command bar), bounded by [`SUGGEST_MAX_ROWS`], the suggestion count, and
/// the body's spare rows — zero-sized when there are none (the panel clamps
/// away; the bar's own line remains). Shared with `hit_test` so clicks
/// resolve to the drawn rows.
#[must_use]
pub fn suggest_panel_rect(size: crate::render::frame::Size, rows: usize) -> Rect {
    let body = Rect {
        x: 0,
        y: 1,
        w: size.w,
        h: size.h.saturating_sub(1),
    };
    let chrome = bottom_chrome_rows(body.h);
    let avail = body.h.saturating_sub(chrome + 1);
    let want = u16::try_from(rows.min(SUGGEST_MAX_ROWS))
        .unwrap_or(0)
        .saturating_add(2);
    let h = want.min(avail);
    if h < 3 || body.w < 12 {
        return Rect {
            x: body.x,
            y: body.y,
            w: 0,
            h: 0,
        };
    }
    let y = body
        .y
        .saturating_add(body.h)
        .saturating_sub(chrome)
        .saturating_sub(h);
    Rect {
        x: body.x,
        y,
        w: body.w,
        h,
    }
}

/// Draw the suggest panel: `verb  usage` + a muted one-line description per
/// row, the highlight bar, and the animator contract fade — unselected text ramps from
/// the muted tone to the settled foreground as `suggest_fade` advances
/// (reduced motion and ASCII land at 1.0, snapshotting the final frame).
/// Every string passes the shared sanitizer (labels are user-controlled).
fn draw_suggest_panel(vm: &ViewModel, frame: &mut Frame) {
    let rect = suggest_panel_rect(vm.size, vm.suggestions.len());
    if rect.h == 0 {
        return;
    }
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(
                rect.x + dx,
                rect.y + dy,
                ' ',
                Style::DEFAULT.with_bg(vm.theme.surface_bg()),
            );
        }
    }
    let inner = frame.border(rect, "suggest", ACCENT);
    let t = vm.suggest_fade.clamp(0.0, 1.0);
    let text_fg = crate::theme::lerp_color(crate::theme::MUTED, vm.theme.body_fg(), t);
    let visible = usize::from(inner.h);
    for (i, sug) in vm.suggestions.iter().take(visible).enumerate() {
        let Ok(dy) = u16::try_from(i) else { break };
        let selected = i == vm.suggest_highlight;
        let marker = if selected { '>' } else { ' ' };
        let line = format!(
            "{marker} {:<30} {}",
            sanitize_for_terminal(&format!("{}  {}", sug.insert, sug.usage)),
            sanitize_for_terminal(sug.description)
        );
        let style = if selected { ACCENT } else { Style::fg(text_fg) };
        clip_line(frame, inner, dy, &line, style);
    }
}

/// The smallest readable modal box: a modal clamps *up* to this even
/// when 80% of a small terminal would be fewer cells.
pub const MIN_MODAL_COLS: u16 = 40;
/// The smallest readable modal box height — see [`MIN_MODAL_COLS`].
pub const MIN_MODAL_ROWS: u16 = 12;

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

/// The confirm overlay's two button labels (shared with hit-testing).
pub const CONFIRM_BUTTONS: (&str, &str) = ("[ Enter: confirm ]", "[ Esc: cancel ]");

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

/// The primary-action chip string for a message modal (`[ Add a project ]`), or
/// `None` for a dismiss-only message. One source for draw + measurement, so the
/// drawn cells and the hit-test width agree.
fn primary_chip(action: crate::dispatch::MessageAction) -> Option<String> {
    action.primary_label().map(|label| format!("[ {label} ]"))
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

/// The profile menu's row rect for profile index `i`: one full-width
/// row per live profile, starting one row below the modal's inner top. One
/// source for the renderer and the hit-test, so a click lands exactly on the
/// drawn row; zero-sized when the row would overrun the frame's usable body
/// (the bottom rows are reserved for the input/confirm line).
#[must_use]
pub fn profile_menu_row_rect(modal: Rect, i: usize) -> Rect {
    let empty = Rect {
        x: modal.x,
        y: modal.y,
        w: 0,
        h: 0,
    };
    if modal.w < 4 || modal.h < 5 {
        return empty;
    }
    let inner = Rect {
        x: modal.x + 1,
        y: modal.y + 1,
        w: modal.w - 2,
        h: modal.h - 2,
    };
    let Ok(dy) = u16::try_from(i + 1) else {
        return empty;
    };
    // The last two inner rows host the naming/confirm/hint line and a gap.
    if dy >= inner.h.saturating_sub(2) {
        return empty;
    }
    Rect {
        x: inner.x,
        y: inner.y + dy,
        w: inner.w,
        h: 1,
    }
}

/// The `[settings]` chip's rect on a hub row — right-aligned
/// inside the row, one source for the draw and the hit-test. Zero-sized when
/// the row is (or is too narrow to host a chip beside a name).
#[must_use]
pub fn profile_menu_settings_rect(modal: Rect, i: usize) -> Rect {
    let rr = profile_menu_row_rect(modal, i);
    let chip_w = u16::try_from(PROFILE_SETTINGS_CHIP.chars().count()).unwrap_or(0);
    if rr.h == 0 || rr.w < chip_w.saturating_add(20) {
        return Rect {
            x: rr.x,
            y: rr.y,
            w: 0,
            h: 0,
        };
    }
    Rect {
        x: rr.x + rr.w - chip_w - 1,
        y: rr.y,
        w: chip_w,
        h: 1,
    }
}

/// The hub's `[settings]` chip text (one source for draw + measurement).
pub const PROFILE_SETTINGS_CHIP: &str = "[settings]";

/// The members editor's action chips, in display order (the rail pattern:
/// chips act on the cursor row) — one source for draw, hit, and the target
/// mapping.
pub const MEMBERS_CHIPS: [&str; 4] = ["[↑]", "[↓]", "[remove]", "[ add ]"];

/// The members editor's action-chip rects on its trailing row (index
/// `members_len`) — one solver for the draw and the hit-test (modal-surface parity).
/// Empty when the frame cannot host the row.
#[must_use]
pub fn members_action_chips(modal: Rect, members_len: usize) -> Vec<(Rect, usize, &'static str)> {
    let rr = profile_menu_row_rect(modal, members_len);
    if rr.h == 0 {
        return Vec::new();
    }
    let mut x = rr.x + 1;
    let mut out = Vec::with_capacity(MEMBERS_CHIPS.len());
    for (i, text) in MEMBERS_CHIPS.iter().enumerate() {
        let w = u16::try_from(text.chars().count()).unwrap_or(0);
        if x + w > rr.x + rr.w {
            break; // too narrow: trailing chips clip away whole
        }
        out.push((
            Rect {
                x,
                y: rr.y,
                w,
                h: 1,
            },
            i,
            *text,
        ));
        x += w + 1;
    }
    out
}

/// The hit target one members chip resolves to (keyed by its text, the same
/// constant the draw uses — the mapping cannot drift).
#[must_use]
pub fn members_chip_target(text: &str) -> crate::hit::Target {
    match text {
        "[↑]" => crate::hit::Target::MemberUp,
        "[↓]" => crate::hit::Target::MemberDown,
        "[remove]" => crate::hit::Target::MemberRemove,
        _ => crate::hit::Target::MemberAddOpen,
    }
}

/// The hub's trailing `[ + new profile ]` row text.
pub const PROFILE_NEW_ROW: &str = " [ + new profile ]";

/// One logical Project Settings display row. Only [`Self::Action`] carries a
/// hit target; policy facts and headings are intentionally not cursor stops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectSettingsDisplayRow {
    /// Section heading.
    Heading(String),
    /// Safe read-only fact.
    Info(String),
    /// Actionable row with its index in the modal's actionable-only list.
    Action {
        /// Actionable-only index.
        index: usize,
        /// Left-side label/detail.
        label: String,
        /// Right-aligned checkbox/action chip.
        chip: String,
    },
    /// Visual spacer.
    Spacer,
}

/// Project Settings rows in their required section order.
#[must_use]
pub fn project_settings_display_rows(
    state: &crate::ProjectSettingsModalState,
) -> Vec<ProjectSettingsDisplayRow> {
    use crate::{ProjectPolicyState, ProjectSettingId, ProjectSettingsItem};
    let Some(view) = state.view.as_deref() else {
        return Vec::new();
    };
    let items = state.items();
    let item_index = |wanted: &ProjectSettingsItem| items.iter().position(|item| item == wanted);
    let mut rows = vec![ProjectSettingsDisplayRow::Heading("Recall".into())];
    for id in [
        ProjectSettingId::TotalRecall,
        ProjectSettingId::TurnInjection,
        ProjectSettingId::Documents,
        ProjectSettingId::Search,
    ] {
        if let Some(row) = view.settings.iter().find(|row| row.id == id) {
            push_setting_row(&mut rows, &item_index, row, view.initialization);
        }
    }
    rows.push(ProjectSettingsDisplayRow::Spacer);
    rows.push(ProjectSettingsDisplayRow::Heading("Capture".into()));
    for id in [
        ProjectSettingId::ToolCallCapture,
        ProjectSettingId::TokenUsageCapture,
        ProjectSettingId::RawTranscripts,
    ] {
        if let Some(row) = view.settings.iter().find(|row| row.id == id) {
            push_setting_row(&mut rows, &item_index, row, view.initialization);
        }
    }
    rows.push(ProjectSettingsDisplayRow::Spacer);
    rows.push(ProjectSettingsDisplayRow::Heading(
        "Security / Policy".into(),
    ));
    let source = match view.policy.state {
        ProjectPolicyState::Declared => "policy: declared · valid",
        ProjectPolicyState::Default => "policy: secure defaults · valid",
        ProjectPolicyState::Invalid => "policy: invalid · fail-closed",
        ProjectPolicyState::Refused => "policy: refused · fail-closed",
    };
    rows.push(ProjectSettingsDisplayRow::Info(source.into()));
    rows.push(ProjectSettingsDisplayRow::Info(format!(
        "sensitive paths: {}",
        view.policy.sensitive_path_count
    )));
    rows.push(ProjectSettingsDisplayRow::Info(format!(
        "redactor packs: {} loaded · {} failed",
        view.policy.loaded_redactor_packs, view.policy.failed_redactor_packs
    )));
    if let Some(detail) = &view.policy.detail {
        rows.push(ProjectSettingsDisplayRow::Info(detail.clone()));
    }
    rows.push(ProjectSettingsDisplayRow::Spacer);
    rows.push(ProjectSettingsDisplayRow::Heading("Integrations".into()));
    for integration in &view.integrations {
        let item = ProjectSettingsItem::Integration(integration.agent.clone());
        let Some(index) = item_index(&item) else {
            continue;
        };
        let detection = integration_detection_label(integration);
        let wiring = integration_wiring_label(integration);
        let enabled = if view.initialization == Init::Initialized {
            integration.enabled()
        } else {
            integration.selected
        };
        rows.push(ProjectSettingsDisplayRow::Action {
            index,
            label: if let Some(hooks) = integration.hook_wiring {
                let hooks = match hooks {
                    crate::NativeAgentHookWiring::Missing => "hooks absent",
                    crate::NativeAgentHookWiring::Managed => "hooks managed",
                    crate::NativeAgentHookWiring::Stale => "hooks need refresh",
                    crate::NativeAgentHookWiring::Conflict => "hooks conflict",
                    crate::NativeAgentHookWiring::Invalid => "hooks invalid",
                };
                format!(
                    "{} · {detection} · capture {wiring} · {hooks}",
                    integration.display_name
                )
            } else {
                format!("{} · {detection} · {wiring}", integration.display_name)
            },
            chip: checkbox(enabled),
        });
    }
    if view.initialization.offers_setup() {
        rows.push(ProjectSettingsDisplayRow::Spacer);
        if let Some(index) = item_index(&ProjectSettingsItem::Initialize) {
            let label = if view.initialization == Init::NotProvisioned {
                "set up this machine: projection, git hooks, selected agents"
            } else {
                "apply every selected local setup step"
            };
            rows.push(ProjectSettingsDisplayRow::Action {
                index,
                label: label.into(),
                chip: "[ initialize project ]".into(),
            });
        }
    }
    rows
}

/// The wiring half of an integration row label. A coming-soon row says so
/// instead of implying a wiring the manager is guaranteed to refuse; a
/// leftover artifact's real state stays visible beside the scope fact rather
/// than hidden by it.
fn integration_wiring_label(integration: &crate::NativeAgentIntegrationRow) -> String {
    use crate::NativeAgentWiring;
    let wiring = match integration.wiring {
        NativeAgentWiring::Missing => "not wired",
        NativeAgentWiring::Managed => "managed",
        NativeAgentWiring::Conflict => "conflict",
        NativeAgentWiring::Invalid => "invalid",
    };
    if integration.alpha_supported {
        wiring.to_owned()
    } else if integration.wiring == NativeAgentWiring::Missing {
        "coming soon".to_owned()
    } else {
        format!("{wiring} · coming soon")
    }
}

/// The row's install evidence, one rule for every agent: the sources found,
/// without versions (the modal's description carries those), or `not
/// installed`. A Codex-only branch used to live here; the data plane now
/// reads every agent's extensions and vendor installs, so the row reads them
/// too.
fn integration_detection_label(integration: &crate::NativeAgentIntegrationRow) -> String {
    if integration.installations.is_empty() {
        return "not installed".to_owned();
    }
    integration
        .installations
        .iter()
        .map(|installation| installation.source.label())
        .collect::<Vec<_>>()
        .join(" + ")
}

fn push_setting_row(
    rows: &mut Vec<ProjectSettingsDisplayRow>,
    item_index: &impl Fn(&crate::ProjectSettingsItem) -> Option<usize>,
    row: &crate::ProjectSettingRow,
    initialization: Init,
) {
    let label = row.detail.as_ref().map_or_else(
        || row.id.label().to_owned(),
        |detail| format!("{} · {detail}", row.id.label()),
    );
    if let Some(index) = item_index(&crate::ProjectSettingsItem::Setting(row.id)) {
        rows.push(ProjectSettingsDisplayRow::Action {
            index,
            label,
            chip: checkbox(row.enabled),
        });
    } else {
        // A configured-elsewhere project shows the committed value as a
        // fact: setup on this machine never rewrites the repository's shared
        // configuration. Anything else locked says so.
        let info = if initialization == Init::NotProvisioned {
            format!(
                "{label} · {} · committed by the repository",
                checkbox(row.enabled)
            )
        } else {
            format!("{label} · locked")
        };
        rows.push(ProjectSettingsDisplayRow::Info(info));
    }
}

fn checkbox(enabled: bool) -> String {
    if enabled { "[x]" } else { "[ ]" }.to_owned()
}

/// Scrollable Project Settings body; the last three inner rows are reserved for
/// focused-row help, inline result text, and stage-honest hints.
#[must_use]
pub fn project_settings_body_rect(modal: Rect) -> Rect {
    Rect {
        x: modal.x + 1,
        y: modal.y + 1,
        w: modal.w.saturating_sub(2),
        h: modal.h.saturating_sub(5),
    }
}

/// Right-aligned checkbox/action-chip rects for visible modal actions. Mouse
/// activation is deliberately limited to these exact cells.
#[must_use]
pub fn project_settings_action_rects(
    modal: Rect,
    state: &crate::ProjectSettingsModalState,
) -> Vec<(usize, Rect)> {
    let body = project_settings_body_rect(modal);
    let rows = project_settings_display_rows(state);
    let scroll = project_settings_scroll(state, &rows, usize::from(body.h));
    rows.iter()
        .enumerate()
        .filter_map(|(line_index, row)| {
            let ProjectSettingsDisplayRow::Action { index, chip, .. } = row else {
                return None;
            };
            let dy = line_index.checked_sub(scroll)?;
            let dy = u16::try_from(dy).ok()?;
            if dy >= body.h {
                return None;
            }
            let w = u16::try_from(chip.chars().count()).unwrap_or(0).min(body.w);
            Some((
                *index,
                Rect {
                    x: body.x + body.w.saturating_sub(w),
                    y: body.y + dy,
                    w,
                    h: 1,
                },
            ))
        })
        .collect()
}

/// Raw-transcript warning's explicit cancel chip, paired with `Esc`.
#[must_use]
pub fn project_settings_cancel_rect(
    modal: Rect,
    state: &crate::ProjectSettingsModalState,
) -> Option<Rect> {
    if !state.confirm_raw || modal.w < 10 || modal.h < 4 {
        return None;
    }
    let inner = Rect {
        x: modal.x + 1,
        y: modal.y + 1,
        w: modal.w.saturating_sub(2),
        h: modal.h.saturating_sub(2),
    };
    let w = 8u16.min(inner.w);
    Some(Rect {
        x: inner.x + inner.w.saturating_sub(w),
        y: inner.y + inner.h.saturating_sub(1),
        w,
        h: 1,
    })
}

fn project_settings_scroll(
    state: &crate::ProjectSettingsModalState,
    rows: &[ProjectSettingsDisplayRow],
    visible: usize,
) -> usize {
    let selected_line = rows.iter().position(|row| {
        matches!(row, ProjectSettingsDisplayRow::Action { index, .. } if *index == state.cursor)
    });
    let max = rows.len().saturating_sub(visible);
    let mut scroll = state.scroll.min(max);
    if state.follow_cursor {
        if let Some(line) = selected_line {
            if line < scroll {
                scroll = line;
            } else if line >= scroll.saturating_add(visible) {
                scroll = line.saturating_add(1).saturating_sub(visible);
            }
        }
    }
    scroll.min(max)
}

#[allow(clippy::too_many_lines)] // one linear draw pass over the modal's sections
fn draw_project_settings(vm: &ViewModel, frame: &mut Frame) {
    let rect = modal_rect(vm.size);
    if rect.w < 20 || rect.h < 7 {
        return;
    }
    draw_scrim(frame, rect);
    crate::chrome::fill(frame, rect, Style::DEFAULT.with_bg(vm.theme.surface_bg()));
    let view = vm.project_settings.view.as_deref();
    let name = view.map_or("project", |view| view.project_name.as_str());
    let stage = view.map_or("", |view| match view.initialization {
        Init::Initialized => "",
        Init::NotInitialized => " · setup required",
        Init::NotProvisioned => " · not set up on this machine",
        Init::NotRepository => " · not a git repository",
        Init::SetupIncomplete => " · setup incomplete",
    });
    let title = format!("Project Settings · {}{stage}", sanitize_for_terminal(name));
    let inner = frame.border(rect, &title, ACCENT);
    if let Some(close) = modal_close_rect(rect) {
        frame.put_str(
            close.x,
            close.y,
            "[x]",
            menu_close_style(vm, &crate::hit::Target::ProjectSettingsClose),
        );
    }
    let body = project_settings_body_rect(rect);
    let rows = project_settings_display_rows(&vm.project_settings);
    let scroll = project_settings_scroll(&vm.project_settings, &rows, usize::from(body.h));
    for (line_index, row) in rows.iter().enumerate().skip(scroll) {
        let Ok(dy) = u16::try_from(line_index - scroll) else {
            break;
        };
        if dy >= body.h {
            break;
        }
        let y = body.y + dy;
        match row {
            ProjectSettingsDisplayRow::Heading(text) => {
                clip_at(frame, body.x, y, body.w, text, ACCENT);
            }
            ProjectSettingsDisplayRow::Info(text) => {
                clip_at(
                    frame,
                    body.x + 2,
                    y,
                    body.w.saturating_sub(2),
                    &sanitize_for_terminal(text),
                    vm.theme.dim(),
                );
            }
            ProjectSettingsDisplayRow::Action { index, label, chip } => {
                let selected = *index == vm.project_settings.cursor;
                let style = if selected {
                    vm.theme.selection()
                } else {
                    vm.theme.body()
                };
                // The shared cursor cue: the row filled, and the marker in its
                // first cell, so the floor tiers still show which row is live.
                if selected {
                    crate::chrome::cursor_row(
                        frame,
                        Rect {
                            x: body.x,
                            y,
                            w: body.w,
                            h: 1,
                        },
                        style,
                    );
                }
                let chip_w = u16::try_from(chip.chars().count()).unwrap_or(0).min(body.w);
                let label_w = body.w.saturating_sub(chip_w + 3);
                clip_at(
                    frame,
                    body.x + 2,
                    y,
                    label_w,
                    &sanitize_for_terminal(label),
                    style,
                );
                clip_at(
                    frame,
                    body.x + body.w.saturating_sub(chip_w),
                    y,
                    chip_w,
                    chip,
                    style,
                );
            }
            ProjectSettingsDisplayRow::Spacer => {}
        }
    }
    let description = vm
        .project_settings
        .focused_description()
        .map_or_else(String::new, |text| format!("About: {text}"));
    clip_at(
        frame,
        inner.x,
        inner.y + inner.h.saturating_sub(3),
        inner.w,
        &sanitize_for_terminal(&description),
        vm.theme.dim(),
    );
    let result_y = inner.y + inner.h.saturating_sub(2);
    let result = if vm.project_settings.running {
        "working…"
    } else {
        vm.project_settings.result.as_deref().unwrap_or("")
    };
    clip_at(
        frame,
        inner.x,
        result_y,
        inner.w,
        &sanitize_for_terminal(result),
        vm.theme.dim(),
    );
    let hints = if vm.project_settings.confirm_raw {
        " enter acknowledge · esc cancel"
    } else if vm.project_settings.running {
        " settings locked while operation completes"
    } else if vm.project_settings.failed_action.is_some() {
        " j/k move · enter/space apply · r retry · esc/x close"
    } else {
        " j/k move · enter/space apply · pgup/pgdn scroll · esc/x close"
    };
    clip_at(
        frame,
        inner.x,
        inner.y + inner.h.saturating_sub(1),
        inner.w,
        hints,
        vm.theme.dim(),
    );
    if let Some(cancel) = project_settings_cancel_rect(rect, &vm.project_settings) {
        frame.put_str(
            cancel.x,
            cancel.y,
            "[cancel]",
            menu_close_style(vm, &crate::hit::Target::ProjectSettingsCancel),
        );
    }
}

/// A mode-modal's close `[x]` style: press-flash aware, muted at rest — the
/// pointer twin of `Esc` (modal-surface parity; the target closes the MODE, never an
/// overlay).
fn menu_close_style(vm: &ViewModel, target: &crate::hit::Target) -> Style {
    let bg = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(target))
        .unwrap_or(vm.theme.surface_bg());
    Style::fg(crate::theme::MUTED).with_bg(bg)
}

/// Draw the profile menu (hub-and-spoke, per the profile hub): the shared
/// centered modal frame + scrim and, per stage — the hub (live profiles with
/// `[settings]` chips and the `[ + new profile ]` row), the per-profile
/// settings page, a naming input (create / rename / duplicate, with the
/// inline name-rule error), or the inline delete confirm. Every stage carries
/// the close `[x]`. Pure drawing over the snapshot; every name
/// passes the shared sanitizer.
#[allow(clippy::too_many_lines)] // one linear pass per menu stage; splitting would obscure the layout
fn draw_profile_menu(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    use crate::profile_menu::{MenuStage, SettingsRow};
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 5 {
        return;
    }
    draw_scrim(frame, rect);
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(
                rect.x + dx,
                rect.y + dy,
                ' ',
                Style::DEFAULT.with_bg(vm.theme.surface_bg()),
            );
        }
    }
    let name_of = |id: &str| {
        vm.profiles
            .iter()
            .find(|p| p.profile_id == id)
            .map(|p| sanitize_for_terminal(&p.name))
    };
    let title = match &vm.profile_menu.stage {
        MenuStage::Settings { profile } => name_of(profile).map(|n| format!("profile · {n}")),
        MenuStage::Members { profile } | MenuStage::MembersAdd { profile } => {
            name_of(profile).map(|n| format!("projects · {n}"))
        }
        _ => None,
    }
    .unwrap_or_else(|| "profiles".to_string());
    let inner = frame.border(rect, &title, ACCENT);
    // The close affordance, on every stage: the pointer twin of
    // Esc, resolving to the MODE's own close target.
    if let Some(cr) = modal_close_rect(rect) {
        let style = menu_close_style(vm, &crate::hit::Target::ProfileMenuClose);
        frame.put_str(cr.x, cr.y, "[x]", style);
    }

    match &vm.profile_menu.stage {
        MenuStage::Settings { profile } => {
            // The settings page: one activatable row per SettingsRow::ALL.
            for (i, row) in SettingsRow::ALL.iter().enumerate() {
                let rr = profile_menu_row_rect(rect, i);
                if rr.h == 0 {
                    break;
                }
                let on_cursor = i == vm.profile_menu.settings_cursor;
                let suffix = match row {
                    SettingsRow::StartupPin => {
                        if vm.startup_profile.as_deref() == Some(profile.as_str()) {
                            "  [on]"
                        } else {
                            "  [off]"
                        }
                    }
                    _ => "",
                };
                let line = format!(
                    "{}{}{suffix}",
                    crate::chrome::row_prefix(on_cursor),
                    row.label()
                );
                let style = if on_cursor {
                    vm.theme.selection()
                } else if matches!(row, SettingsRow::Delete) {
                    severity_style(crate::dispatch::Severity::Warning)
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    crate::chrome::cursor_row(frame, rr, style);
                }
                clip_line(frame, inner, rr.y - inner.y, &line, style);
            }
        }
        MenuStage::Members { .. } => {
            // The members editor: the profile's ordered view, then the
            // action-chip row (the rail pattern — chips act on the cursor
            // row, the click twins of d / ⇧j/k / a — modal-surface parity).
            if vm.profile_menu.members.is_empty() {
                clip_line(
                    frame,
                    inner,
                    1,
                    " (no projects in this profile)",
                    Style::fg(crate::theme::MUTED),
                );
            }
            for (i, p) in vm.profile_menu.members.iter().enumerate() {
                let rr = profile_menu_row_rect(rect, i);
                if rr.h == 0 {
                    break;
                }
                let on_cursor = i == vm.profile_menu.member_cursor;
                let style = if on_cursor {
                    vm.theme.selection()
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    crate::chrome::cursor_row(frame, rr, style);
                }
                clip_line(
                    frame,
                    inner,
                    rr.y - inner.y,
                    &format!(
                        "{}{}",
                        crate::chrome::row_prefix(on_cursor),
                        sanitize_for_terminal(&p.name)
                    ),
                    style,
                );
            }
            for (chip_rect, _, text) in members_action_chips(rect, vm.profile_menu.members.len()) {
                let target = members_chip_target(text);
                let bg = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(&target))
                    .unwrap_or(vm.theme.surface_bg());
                frame.put_str(
                    chip_rect.x,
                    chip_rect.y,
                    text,
                    Style::fg(crate::theme::MUTED).with_bg(bg),
                );
            }
        }
        MenuStage::MembersAdd { .. } => {
            // The add stage: catalogue projects not yet in this view.
            let candidates = vm.profile_menu.candidates();
            if candidates.is_empty() {
                clip_line(
                    frame,
                    inner,
                    1,
                    " (every catalogue project is already here)",
                    Style::fg(crate::theme::MUTED),
                );
            }
            for (i, p) in candidates.iter().enumerate() {
                let rr = profile_menu_row_rect(rect, i);
                if rr.h == 0 {
                    break;
                }
                let on_cursor = i == vm.profile_menu.add_cursor;
                let style = if on_cursor {
                    vm.theme.selection()
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    crate::chrome::cursor_row(frame, rr, style);
                }
                clip_line(
                    frame,
                    inner,
                    rr.y - inner.y,
                    &format!(
                        "{}{}",
                        crate::chrome::row_prefix(on_cursor),
                        sanitize_for_terminal(&p.name)
                    ),
                    style,
                );
            }
        }
        _ => {
            // The hub: the live profiles, each with its `[settings]` chip,
            // then the `[ + new profile ]` row.
            for (i, p) in vm.profiles.iter().enumerate() {
                let rr = profile_menu_row_rect(rect, i);
                if rr.h == 0 {
                    break; // the frame is full; the rest clip away whole
                }
                let is_active = p.profile_id == vm.active_profile;
                let on_cursor = i == vm.profile_menu.cursor
                    && matches!(vm.profile_menu.stage, MenuStage::Browse);
                let marker = if is_active {
                    crate::theme::Icon::Profile.glyph(unicode)
                } else {
                    ' '
                };
                let line = format!(
                    "{}{marker} {}",
                    crate::chrome::row_prefix(on_cursor),
                    sanitize_for_terminal(&p.name)
                );
                let style = if on_cursor {
                    vm.theme.selection()
                } else if is_active {
                    ACCENT
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    crate::chrome::cursor_row(frame, rr, style);
                }
                clip_line(frame, inner, rr.y - inner.y, &line, style);
                // The row's settings chip — a click twin for `s` (modal-surface parity).
                let cr = profile_menu_settings_rect(rect, i);
                if cr.w > 0 {
                    let target = crate::hit::Target::ProfileSettings(i);
                    let bg = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(&target))
                        .or_else(|| on_cursor.then_some(crate::theme::SELECT_BG))
                        .unwrap_or(vm.theme.surface_bg());
                    frame.put_str(
                        cr.x,
                        cr.y,
                        PROFILE_SETTINGS_CHIP,
                        Style::fg(crate::theme::MUTED).with_bg(bg),
                    );
                }
            }
            // The `[ + new profile ]` row — the click twin for `n`.
            let new_rr = profile_menu_row_rect(rect, vm.profiles.len());
            if new_rr.h > 0 {
                let target = crate::hit::Target::ProfileNew;
                let bg = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(&target))
                    .unwrap_or(vm.theme.surface_bg());
                clip_line(
                    frame,
                    inner,
                    new_rr.y - inner.y,
                    PROFILE_NEW_ROW,
                    Style::fg(crate::theme::MUTED).with_bg(bg),
                );
            }
        }
    }

    // The stage line, on the inner bottom row: stage hints, the naming input,
    // or the inline delete confirm (an explicit second act — the profile menu and header identity).
    if inner.h == 0 {
        return;
    }
    let stage_dy = inner.h - 1;
    let muted = Style::fg(crate::theme::MUTED);
    match &vm.profile_menu.stage {
        MenuStage::Browse => {
            clip_line(
                frame,
                inner,
                stage_dy,
                " ⏎ switch · s settings · n new · d delete · esc close",
                muted,
            );
        }
        MenuStage::Settings { .. } => {
            clip_line(frame, inner, stage_dy, " ⏎ open · esc back", muted);
        }
        MenuStage::Members { .. } => {
            clip_line(
                frame,
                inner,
                stage_dy,
                " d remove · ⇧j/k reorder · a add · esc back",
                muted,
            );
        }
        MenuStage::MembersAdd { .. } => {
            clip_line(frame, inner, stage_dy, " ⏎ add · esc back", muted);
        }
        MenuStage::NamingCreate
        | MenuStage::NamingRename { .. }
        | MenuStage::NamingDuplicate { .. } => {
            let verb = match &vm.profile_menu.stage {
                MenuStage::NamingCreate => "new profile",
                MenuStage::NamingDuplicate { .. } => "duplicate as",
                _ => "rename to",
            };
            let line = format!(" {verb}: {}▏", sanitize_for_terminal(&vm.command_line));
            clip_line(frame, inner, stage_dy, &line, ACCENT);
            if vm.profile_menu.name_error && stage_dy > 0 {
                clip_line(
                    frame,
                    inner,
                    stage_dy - 1,
                    " name must be 1–24 chars and unique",
                    severity_style(crate::dispatch::Severity::Warning),
                );
            }
        }
        MenuStage::ConfirmDelete { profile, .. } => {
            let name = vm
                .profiles
                .iter()
                .find(|p| &p.profile_id == profile)
                .map(|p| sanitize_for_terminal(&p.name))
                .unwrap_or_default();
            let line = format!(" delete {name}? ⏎ confirm · esc cancel");
            clip_line(
                frame,
                inner,
                stage_dy,
                &line,
                severity_style(crate::dispatch::Severity::Warning),
            );
        }
    }
}

// ---- The destination explorer's geometry --------------------
//
// One solver per region feeds BOTH the draw and the hit-test (modal-surface parity), and
// the row solver is the tree's first to honour a scroll offset — the profile
// menu simply stops drawing past the frame, which a directory listing cannot
// afford. Layout inside the modal's inner box: the `⬑ ..` back row on top, the
// scrolling list beneath it, then the status line and the button row on the two
// bottom rows (7 entry rows at the 40×12 modal floor).

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

/// Draw the destination explorer: the shared centered modal frame +
/// scrim, the open folder in the title (elided from the LEFT so the basename
/// survives), the `⬑ ..` back row, the scrolling entry list (directories
/// enterable, files dimmed and inert), an overflow scrollbar, the status line,
/// and the two action chips. Every stage carries the close `[x]`.
/// Pure drawing over the snapshot; every name passes the shared sanitizer.
#[allow(clippy::too_many_lines)] // one linear pass over the surface's regions
fn draw_explorer(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 5 {
        return;
    }
    draw_scrim(frame, rect);
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(
                rect.x + dx,
                rect.y + dy,
                ' ',
                Style::DEFAULT.with_bg(vm.theme.surface_bg()),
            );
        }
    }
    // The title names what `[ select destination ]` will register — so it is
    // elided from the left, keeping the basename that identifies the folder.
    // The budget leaves room for the corner glyphs and the close `[x]`.
    const TITLE_PREFIX: &str = "add project · ";
    let budget = usize::from(rect.w.saturating_sub(8));
    let cwd = sanitize_for_terminal(&vm.explorer.display_cwd());
    let title = if budget > TITLE_PREFIX.chars().count() {
        format!(
            "{TITLE_PREFIX}{}",
            elide_left(&cwd, budget - TITLE_PREFIX.chars().count())
        )
    } else {
        "add project".to_owned()
    };
    let inner = frame.border(rect, &title, ACCENT);
    if let Some(cr) = modal_close_rect(rect) {
        let style = menu_close_style(vm, &crate::hit::Target::ExplorerClose);
        frame.put_str(cr.x, cr.y, "[x]", style);
    }

    // The back row — dimmed and inert at the filesystem root (a calm edge).
    let up = explorer_up_rect(rect);
    if up.h > 0 {
        let at_root = vm.explorer.cwd.parent().is_none();
        let glyph = if unicode { "⬑" } else { "^" };
        let style = if at_root {
            vm.theme.dim()
        } else {
            let bg = crate::chrome::press_flash_bg(
                vm.theme,
                vm.button_flash(&crate::hit::Target::ExplorerUp),
            )
            .unwrap_or(vm.theme.surface_bg());
            Style::fg(crate::theme::MUTED).with_bg(bg)
        };
        clip_line(frame, inner, up.y - inner.y, &format!(" {glyph} .."), style);
    }
    // The dot-toggle's click twin (modal-surface parity), right-aligned in the back row:
    // the status line teaches `.`, so a pointer must be able to press it too.
    // Lit in the accent while dot-entries are shown, muted while hidden — the
    // state reads off the chip itself.
    let dots = explorer_dotfiles_rect(rect);
    if dots.w > 0 {
        let bg = crate::chrome::press_flash_bg(
            vm.theme,
            vm.button_flash(&crate::hit::Target::ExplorerDotfiles),
        )
        .unwrap_or(vm.theme.surface_bg());
        let fg = if vm.explorer.show_hidden {
            crate::theme::ACCENT
        } else {
            crate::theme::MUTED
        };
        frame.put_str(
            dots.x,
            dots.y,
            EXPLORER_DOTFILES_CHIP,
            Style::fg(fg).with_bg(bg),
        );
    }

    // The entries, through the scroll-aware solver.
    let visible = vm.explorer.visible();
    let list = explorer_list_area(rect);
    if visible.is_empty() && list.h > 0 {
        clip_line(
            frame,
            inner,
            list.y - inner.y,
            " (empty directory)",
            Style::fg(crate::theme::MUTED),
        );
    }
    // The cursor cue is the one every list surface shares (the rail's rule): a
    // cursor-only marker glyph in the row's first cell, so the highlighted row
    // survives the colourless tiers where the selection wash resolves away,
    // plus the bold selection pill on the tiers that carry it. A directory is
    // told apart by its `/` suffix, so the marker column belongs to the cursor.
    for (i, entry) in visible.iter().enumerate() {
        let rr = explorer_row_rect(rect, i, vm.explorer.scroll);
        if rr.h == 0 {
            continue; // scrolled out of the viewport
        }
        let on_cursor = i == vm.explorer.cursor;
        let style = if on_cursor {
            vm.theme.selection()
        } else if entry.is_dir {
            Style::fg(vm.theme.body_fg())
        } else {
            vm.theme.dim()
        };
        if on_cursor {
            crate::chrome::cursor_row(frame, rr, style);
        }
        let name = sanitize_for_terminal(&entry.name);
        let suffix = if entry.is_dir { "/" } else { "" };
        // A junction says so — and an unlistable one is a dim, inert row like a
        // file, so the operator is never walked into an access-denied listing.
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
    // The overflow indicator only (the scrollbar rule) — a fitting listing draws none.
    draw_overflow_bar(
        frame,
        list,
        vm.explorer.scroll,
        vm.explorer.visible_len(),
        vm.theme,
        unicode,
    );

    // The status line, then the chips, on the two bottom inner rows.
    if inner.h >= 2 {
        let (line, is_error) = explorer_status_line(&vm.explorer, usize::from(inner.w));
        let style = if is_error {
            severity_style(crate::dispatch::Severity::Warning)
        } else {
            Style::fg(crate::theme::MUTED)
        };
        clip_line(frame, inner, inner.h - 2, &line, style);
    }
    for (chip, target, text) in explorer_button_rects(rect) {
        let bg = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(&target))
            .unwrap_or(vm.theme.surface_bg());
        frame.put_str(
            chip.x,
            chip.y,
            text,
            Style::fg(crate::theme::MUTED).with_bg(bg),
        );
    }
}

/// The colour picker's row rect for row index `i` — the same shape
/// as [`profile_menu_row_rect`] (one source for draw + hit), reused so the two
/// modal surfaces cannot drift apart.
#[must_use]
pub fn colour_picker_row_rect(modal: Rect, i: usize) -> Rect {
    profile_menu_row_rect(modal, i)
}

/// The number of rows the picker's current stage lists (slots, or presets +
/// the reset row) — shared by the renderer and the hit-test.
#[must_use]
pub fn colour_picker_rows(stage: crate::colour_picker::PickerStage) -> usize {
    match stage {
        crate::colour_picker::PickerStage::Slots => crate::theme::ThemeSlot::ALL.len(),
        crate::colour_picker::PickerStage::Presets => crate::theme::PRESETS.len() + 1,
    }
}

/// Draw the colour picker: stage one lists the five slots with each
/// slot's current swatch (or `default`); stage two lists the ten presets —
/// name beside a solid swatch box — plus the reset row. The swatch is
/// colour-only, so on mono/ASCII the name alone carries the choice. Pure
/// drawing over the snapshot; the dashboard behind the scrim is the preview.
#[allow(clippy::too_many_lines)] // one linear pass per picker stage; slots then presets
fn draw_colour_picker(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    use crate::colour_picker::{PickerStage, RESET_ROW};
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 5 {
        return;
    }
    draw_scrim(frame, rect);
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(
                rect.x + dx,
                rect.y + dy,
                ' ',
                Style::DEFAULT.with_bg(vm.theme.surface_bg()),
            );
        }
    }
    // The title carries the TARGET profile — the page edits
    // that profile, active or not.
    let target_name = vm
        .profiles
        .iter()
        .find(|p| p.profile_id == vm.colour_picker.target_profile)
        .map(|p| sanitize_for_terminal(&p.name));
    let title = match (vm.colour_picker.stage, target_name) {
        (PickerStage::Slots, Some(n)) => format!("colours · {n}"),
        (PickerStage::Slots, None) => "colours".to_string(),
        (PickerStage::Presets, Some(n)) => {
            format!("colours · {n} · {}", vm.colour_picker.slot().key())
        }
        (PickerStage::Presets, None) => format!("colours · {}", vm.colour_picker.slot().key()),
    };
    let inner = frame.border(rect, &title, ACCENT);
    // The close affordance: the picker's own close — back to
    // the profile menu whence it came, the pointer twin of Esc-to-the-end.
    if let Some(cr) = modal_close_rect(rect) {
        let style = menu_close_style(vm, &crate::hit::Target::PickerClose);
        frame.put_str(cr.x, cr.y, "[x]", style);
    }
    let swatch = if unicode { "██" } else { "##" };

    match vm.colour_picker.stage {
        PickerStage::Slots => {
            for (i, slot) in crate::theme::ThemeSlot::ALL.iter().enumerate() {
                let rr = colour_picker_row_rect(rect, i);
                if rr.h == 0 {
                    break;
                }
                let on_cursor = i == vm.colour_picker.slot_cursor;
                let style = if on_cursor {
                    vm.theme.selection()
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    crate::chrome::cursor_row(frame, rr, style);
                }
                let x = frame.put_str(
                    rr.x,
                    rr.y,
                    &format!("{}{:<12}", crate::chrome::row_prefix(on_cursor), slot.key()),
                    style,
                );
                // The slot's current value — the TARGET's overrides, not the
                // live theme: a non-active profile's page tells
                // the truth. A swatch in its override, or the word `default`.
                match vm.colour_picker.target_overrides.get(*slot) {
                    Some(c) => {
                        frame.put_str(x, rr.y, swatch, Style::fg(c).with_bg(style.bg));
                    }
                    None => {
                        frame.put_str(x, rr.y, "default", vm.theme.dim().with_bg(style.bg));
                    }
                }
            }
        }
        PickerStage::Presets => {
            for i in 0..=RESET_ROW {
                let rr = colour_picker_row_rect(rect, i);
                if rr.h == 0 {
                    break;
                }
                let on_cursor = i == vm.colour_picker.preset_cursor;
                let style = if on_cursor {
                    vm.theme.selection()
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    crate::chrome::cursor_row(frame, rr, style);
                }
                let prefix = crate::chrome::row_prefix(on_cursor);
                match crate::theme::PRESETS.get(i) {
                    // A preset row: the name beside a box of the colour itself.
                    Some((name, colour)) => {
                        let x = frame.put_str(rr.x, rr.y, &format!("{prefix}{name:<8}"), style);
                        frame.put_str(x, rr.y, swatch, Style::fg(*colour).with_bg(style.bg));
                    }
                    // The reset row sits beside the presets (the colour picker and its presets).
                    None => {
                        frame.put_str(rr.x, rr.y, &format!("{prefix}reset to default"), style);
                    }
                }
            }
        }
    }

    if inner.h > 0 {
        let hint = match vm.colour_picker.stage {
            PickerStage::Slots => " ⏎ choose · esc menu",
            PickerStage::Presets => " ⏎ apply (live) · esc slots",
        };
        clip_line(
            frame,
            inner,
            inner.h - 1,
            hint,
            Style::fg(crate::theme::MUTED),
        );
    }
}

/// A message severity's frame colour: red error, amber warning, green
/// success, accent info. Colour-only, so the mono/ASCII tiers drop it and the
/// border glyphs stand — the one styling axis decided at draw, not per call site.
fn severity_style(severity: crate::dispatch::Severity) -> Style {
    use crate::dispatch::Severity;
    Style::fg(match severity {
        Severity::Error => crate::theme::STATUS_ERROR,
        Severity::Warning => crate::theme::HEALTH_WARN,
        Severity::Success | Severity::Info => crate::theme::ACCENT,
    })
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

/// Draw the dispatch overlay as a centered modal: a themed scrim over
/// the dashboard, then a bordered, scrollable pane. Geometry comes from the one
/// shared [`modal_rect`], so pixels and hit-test targets can never disagree.
fn draw_overlay(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    use crate::dispatch::Overlay;
    let Some(overlay) = &vm.overlay else { return };
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 3 {
        return; // clamped away on tiny terminals; the bar still works
    }
    draw_scrim(frame, rect);
    // Blank the region so the panels underneath never bleed through.
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(
                rect.x + dx,
                rect.y + dy,
                ' ',
                Style::DEFAULT.with_bg(vm.theme.surface_bg()),
            );
        }
    }
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
        Overlay::Confirm { request, repo } => {
            let inner = frame.border(rect, "confirm", ACCENT);
            clip_line(
                frame,
                inner,
                0,
                &sanitize_for_terminal(&format!("will run: {}", request.verb_line())),
                Style::DEFAULT,
            );
            clip_line(
                frame,
                inner,
                1,
                &sanitize_for_terminal(&format!("target repository: {repo}")),
                Style::DEFAULT,
            );
            // Confirm / Cancel as two focus-ring buttons: the focused
            // one carries the focus wash, a pressed one the flash.
            // The gap (3) and columns mirror the hit-test exactly.
            use crate::dispatch::OverlayControl;
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
        Overlay::Running { title, lines } => draw_running(frame, rect, title, lines),
        // A message: an Alert (blocking) or a Notice (auto-dismissing)
        // renders from `{ title, body, severity, action }` through one path.
        Overlay::Alert(msg) | Overlay::Notice(msg) => draw_message(vm, frame, rect, msg),
    }
    // The universal close X on the top-right border, for modals that
    // carry a Close control (Results / Confirm; Running has none). The focus
    // wash marks it when the ring rests on Close; the flash when it is pressed.
    if overlay
        .controls()
        .contains(&crate::dispatch::OverlayControl::Close)
    {
        if let Some(cr) = modal_close_rect(rect) {
            let control = crate::dispatch::OverlayControl::Close;
            let style = overlay_button_style(vm, &crate::hit::Target::OverlayClose, control);
            let focused = vm.focused_overlay_control() == Some(control);
            frame.put_str(cr.x, cr.y, &focus_brackets("[x]", focused), style);
        }
    }
}

/// The full three-column grid, tiled from the data-driven layout model
/// (the data-driven layout model). Each physical column renders by its role — the default model
/// places the rail, cards, and panels in columns 1/2/3 at 20/40/40, so this is
/// byte-identical to the shipped grid; a rearranged model moves and
/// resizes the columns and this same loop draws them where the model says.
fn draw_full(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    let sl = crate::layout_model::solve_model(&vm.layout, body);
    let editing = vm.mode == crate::input::Mode::PanelEditor;
    for col in &sl.columns {
        if col.rect.h < 2 || col.rect.w == 0 {
            continue;
        }
        // In edit mode every column renders through the one per-unit path, so the
        // grips (computed from the same `solve_column`) sit exactly on the drawn
        // units. In normal mode the homogeneous columns keep their richer stacks
        // (scroll, scrollbars) — byte-identical to before.
        if editing || col.role() == ColumnRole::Mixed {
            draw_column_units(vm, backend, frame, col.rect, &col.units, unicode);
        } else {
            match col.role() {
                ColumnRole::Rail => draw_projects(vm, frame, col.rect, unicode),
                ColumnRole::Cards => {
                    draw_detail(vm, backend, frame, col.rect, unicode, &col.card_kinds());
                }
                ColumnRole::Sections => {
                    draw_graphs(vm, backend, frame, col.rect, &col.section_ids());
                }
                ColumnRole::Mixed | ColumnRole::Empty => {}
            }
        }
    }
    // In panel-editor mode, overlay grips + column dividers over the columns.
    if editing {
        draw_editor_grips(vm, frame, &sl, unicode);
    }
    crate::bars::draw_hint_bar(vm, vm.theme, frame, sl.hint);
    draw_command(vm, frame, sl.command);
}

/// Draw a column's `units` in order through the one shared per-unit path — the
/// grid's edit-mode render and the mixed-column render. Each unit
/// draws via its own `draw_one`; the geometry is the shared `solve_column`, so
/// the grips (same solver) sit exactly on the drawn units.
fn draw_column_units(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    rect: Rect,
    units: &[crate::layout_model::Unit],
    unicode: bool,
) {
    let (slots, _) = crate::layout_model::solve_column(units, rect, 0);
    for slot in &slots {
        draw_unit(
            vm,
            backend,
            frame,
            slot.unit,
            slot.rect,
            slot.collapsed,
            unicode,
        );
    }
}

/// Draw one unit into `rect` — the rail, a card, or a panel (panel-editor mode).
fn draw_unit(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    unit: crate::layout_model::Unit,
    rect: Rect,
    collapsed: bool,
    unicode: bool,
) {
    use crate::layout_model::Unit;
    match unit {
        Unit::Rail => draw_projects(vm, frame, rect, unicode),
        Unit::Card(k) => {
            if let Some(detail) = vm.detail.as_deref() {
                let ctx = crate::cards::CardCtx {
                    detail,
                    activity: &vm.activity,
                    focused: vm.focus == Panel::Detail,
                    unicode,
                    pressed: vm.pressed.clone(),
                    press_pulse: vm.press_pulse,
                };
                crate::cards::draw_one(&ctx, vm.theme, k, rect, collapsed, backend, frame);
            }
        }
        Unit::Section(s) => crate::col3::draw_one(
            &vm.col3,
            s,
            vm.focus == Panel::Graphs,
            unicode,
            collapsed,
            rect,
            backend,
            frame,
        ),
    }
}

/// Draw the panel-editor grips: a grip glyph on each unit's top border
/// row (accent-washed; the cursor's unit and a picked unit stand out) and a
/// highlighted divider on each shared column border. Grips are computed from the
/// same `solve_column` the edit-mode render uses, so they sit on the drawn units.
/// Colour + one glyph only, so the paint-never-moves-a-lit-cell property holds.
fn draw_editor_grips(
    vm: &ViewModel,
    frame: &mut Frame,
    sl: &crate::layout_model::SolvedLayout,
    unicode: bool,
) {
    let grip = if unicode { '⣿' } else { ':' };
    for (ci, col) in sl.columns.iter().enumerate() {
        // The right divider (between this column and the next) — a highlighted bar.
        if ci + 1 < sl.columns.len() && col.rect.w > 0 {
            let x = col.rect.x + col.rect.w.saturating_sub(1);
            for dy in 0..col.rect.h {
                frame.set(x, col.rect.y + dy, if unicode { '┃' } else { '|' }, ACCENT);
            }
        }
        let (slots, _) = crate::layout_model::solve_column(&col.units, col.rect, 0);
        for (ui, slot) in slots.iter().enumerate() {
            if slot.rect.w == 0 || slot.rect.h == 0 {
                continue;
            }
            let is_cursor = ci == vm.editor.cursor.col && ui == vm.editor.cursor.unit;
            let style = if is_cursor && vm.editor.picked {
                vm.theme.selection()
            } else if is_cursor {
                ACCENT
            } else {
                vm.theme.dim()
            };
            // The grip sits at the unit's top-left border corner (chrome, never a
            // data cell — the border glyph is already there).
            frame.set(slot.rect.x, slot.rect.y, grip, style);
            // A short handle run to read as a bar, clipped to the border width.
            let run = slot.rect.w.min(4);
            for dx in 1..run {
                frame.set(slot.rect.x + dx, slot.rect.y, grip, style);
            }
        }
    }
}

/// The two-column degraded grid: projects left, detail over graphs right.
fn draw_two_col(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    let r = two_col(body);
    draw_projects(vm, frame, r.col1, unicode);
    draw_detail(
        vm,
        backend,
        frame,
        r.col2,
        unicode,
        &crate::cards::CardKind::ALL,
    );
    draw_graphs(vm, backend, frame, r.col3, &crate::col3::SectionId::ALL);
    crate::bars::draw_hint_bar(vm, vm.theme, frame, r.hint);
    draw_command(vm, frame, r.command);
}

/// The single-panel degraded layout: a tab strip + the focused panel.
fn draw_single(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    let (tabs, panel, hint, command) = single_panel(body);
    draw_tab_strip(vm, frame, tabs);
    match vm.focus {
        Panel::Detail => draw_detail(
            vm,
            backend,
            frame,
            panel,
            unicode,
            &crate::cards::CardKind::ALL,
        ),
        Panel::Graphs => draw_graphs(vm, backend, frame, panel, &crate::col3::SectionId::ALL),
        // Projects (and Command focus) show the projects panel.
        Panel::Projects | Panel::Command => draw_projects(vm, frame, panel, unicode),
    }
    crate::bars::draw_hint_bar(vm, vm.theme, frame, hint);
    draw_command(vm, frame, command);
}

/// The `[Projects] Detail Graphs` tab strip for the single-panel layout.
fn draw_tab_strip(vm: &ViewModel, frame: &mut Frame, tabs: Rect) {
    if tabs.h == 0 {
        return;
    }
    let mut strip = String::new();
    for (panel, label) in [
        (Panel::Projects, "Projects"),
        (Panel::Detail, "Detail"),
        (Panel::Graphs, "Graphs"),
    ] {
        if vm.focus == panel {
            strip.push('[');
            strip.push_str(label);
            strip.push(']');
        } else {
            strip.push(' ');
            strip.push_str(label);
            strip.push(' ');
        }
    }
    clip_at(frame, tabs.x, tabs.y, tabs.w, &strip, ACCENT);
}

/// The centered "terminal too small" notice.
fn draw_too_small(frame: &mut Frame, area: Rect) {
    let msg = format!("Terminal too small (need ≥ {MIN_NOTICE_COLS}×{MIN_NOTICE_ROWS})");
    let len = u16::try_from(msg.chars().count()).unwrap_or(u16::MAX);
    let x = area.x + area.w.saturating_sub(len) / 2;
    let y = area.y + area.h / 2;
    clip_at(frame, x, y, area.w, &msg, Style::DEFAULT);
}

/// Draw the bottom command / add-project bar (mode-aware), with a block cursor.
/// Restyled as chrome: themed border + title, the prompt in the
/// key-hint accent, the input in body text.
pub(crate) fn draw_command(vm: &ViewModel, frame: &mut Frame, command: Rect) {
    if command.h == 0 {
        return;
    }
    let (btitle, prefix) = match vm.mode {
        Mode::AddProject => ("add project (path)", ""),
        _ => ("command", "/"),
    };
    let focused = vm.focus == Panel::Command;
    let inner = crate::chrome::panel(
        frame,
        command,
        btitle,
        vm.theme.border(crate::theme::Section::Command, focused),
        vm.theme.title(crate::theme::Section::Command),
    );
    let px = frame.put_str(inner.x, inner.y, prefix, vm.theme.key_hint());
    let line = format!("{}\u{2588}", vm.command_line);
    let room = (inner.x + inner.w).saturating_sub(px);
    let clipped: String = line.chars().take(usize::from(room)).collect();
    frame.put_str(px, inner.y, &clipped, vm.theme.body());
    // A parse error renders inline under the input — the caret run under the
    // offending span plus the reason (and the nearest-verb hint), drawn over
    // the bar's bottom border row so no layout shifts.
    if let Some(err) = &vm.command_error {
        // The bar draws a virtual `/` before the buffer: spans shift right by
        // the prefix so the caret run sits under the typed glyphs.
        let start =
            (err.span.0 + prefix.chars().count()).min(usize::from(inner.w).saturating_sub(1));
        let len = err.span.1.saturating_sub(err.span.0).max(1);
        let mut marker = " ".repeat(start);
        marker.push_str(&"^".repeat(len.min(usize::from(inner.w).saturating_sub(start))));
        marker.push(' ');
        marker.push_str(&sanitize_for_terminal(&err.reason));
        if let Some(hint) = err.hint {
            marker.push_str(&format!(" — did you mean `{hint}`?"));
        }
        let y = command.y.saturating_add(command.h.saturating_sub(1));
        clip_at(frame, inner.x, y, inner.w, &marker, ACCENT);
    }
}

/// Column 1 — the projects rail: count badge in the title, per-row
/// kind icon + label + trailing status dot, the selection as a filled pill.
/// Windowed to keep the selection in view; every label passes the sanitizer.
/// The rail's bottom action bar: the list area (rows above the bar) and
/// the clickable `[glyph]` buttons — `+` add always, and `↑`/`↓`/`✕` (reorder
/// up/down, remove selected) when a project is selected. One shared source for
/// `draw_projects` and the hit-test, so the chips and their click targets are
/// the same cells. Returns `(list_area, buttons)`; no buttons when the rail is
/// too short to spare a row.
pub(crate) fn rail_action_layout(
    inner: Rect,
    has_selection: bool,
) -> (Rect, Vec<(Rect, crate::hit::Target)>) {
    if inner.h < 2 {
        return (inner, Vec::new());
    }
    let bar_y = inner.y + inner.h - 1;
    let list = Rect {
        h: inner.h - 1,
        ..inner
    };
    let mut targets = vec![crate::hit::Target::RailAdd];
    if has_selection {
        targets.push(crate::hit::Target::RailReorderUp);
        targets.push(crate::hit::Target::RailReorderDown);
        targets.push(crate::hit::Target::RailRemove);
    }
    // Chips abut (`[+][↑][↓][✕]`) so all four fit even in a ~14-cell rail; a
    // gap would clip the rightmost (remove) at narrow widths.
    let chip = 3u16;
    let right = inner.x + inner.w;
    let mut x = inner.x;
    let mut buttons = Vec::with_capacity(targets.len());
    for t in targets {
        if x + chip > right {
            break;
        }
        buttons.push((
            Rect {
                x,
                y: bar_y,
                w: chip,
                h: 1,
            },
            t,
        ));
        x += chip;
    }
    (list, buttons)
}

/// The glyph inside a rail action chip; the ASCII floor substitutes.
fn rail_action_glyph(target: &crate::hit::Target, unicode: bool) -> char {
    use crate::hit::Target;
    match target {
        Target::RailAdd => '+',
        Target::RailReorderUp => {
            if unicode {
                '↑'
            } else {
                '^'
            }
        }
        Target::RailReorderDown => {
            if unicode {
                '↓'
            } else {
                'v'
            }
        }
        Target::RailRemove => {
            if unicode {
                '✕'
            } else {
                'x'
            }
        }
        _ => '?',
    }
}

/// Draw the rail action chips, each `[glyph]` — muted at rest, flashing toward
/// the accent when pressed (the press-flash rule).
fn draw_rail_actions(
    vm: &ViewModel,
    frame: &mut Frame,
    buttons: &[(Rect, crate::hit::Target)],
    unicode: bool,
) {
    for (r, target) in buttons {
        let g = rail_action_glyph(target, unicode);
        let style = match crate::chrome::press_flash_bg(vm.theme, vm.button_flash(target)) {
            Some(bg) => vm.theme.dim().with_bg(bg),
            None => vm.theme.dim(),
        };
        frame.put_str(r.x, r.y, &format!("[{g}]"), style);
    }
}

fn draw_projects(vm: &ViewModel, frame: &mut Frame, rect: Rect, unicode: bool) {
    draw_projects_impl(vm, frame, rect, unicode, false);
}

/// Draw the same Projects rail without its write-capable add/remove/reorder
/// action row. The Recall Viewer calls this read-only variant.
pub(crate) fn draw_projects_read_only(
    vm: &ViewModel,
    frame: &mut Frame,
    rect: Rect,
    unicode: bool,
) {
    draw_projects_impl(vm, frame, rect, unicode, true);
}

#[allow(clippy::too_many_lines)] // one linear draw pass shared by writable and read-only rails
fn draw_projects_impl(
    vm: &ViewModel,
    frame: &mut Frame,
    rect: Rect,
    unicode: bool,
    read_only: bool,
) {
    use crate::theme::{Icon, Section};
    let focused = if read_only {
        vm.viewer.pane == crate::recall_viewer::ViewerPane::Projects
    } else {
        vm.focus == Panel::Projects
    };
    let inner = crate::chrome::panel(
        frame,
        rect,
        "projects",
        vm.theme.border(Section::Projects, focused),
        vm.theme.title(Section::Projects),
    );
    // The registered-project count as a badge on the title row, after the title
    // (the count is a fact you see, not a scroll you take).
    if rect.w >= 16 && rect.h >= 2 {
        let bx = rect.x + 12; // just past ` projects ` (title starts at x+3)
        crate::chrome::badge(
            frame,
            bx,
            rect.y,
            &vm.projects.len().to_string(),
            vm.theme.badge(Section::Projects.accent()),
        );
    }
    let (list, buttons) = if read_only {
        (inner, Vec::new())
    } else {
        rail_action_layout(inner, !vm.projects.is_empty())
    };
    if vm.projects.is_empty() {
        clip_line(
            frame,
            list,
            0,
            if read_only {
                "No readable project selected"
            } else {
                "No projects yet — press 'a' or [+]"
            },
            vm.theme.body(),
        );
        draw_rail_actions(vm, frame, &buttons, unicode);
        return;
    }
    let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(list.h));
    // `> ▣ label … ●` — cursor, kind icon, label, trailing status dot.
    let label_w = usize::from(inner.w).saturating_sub(6);
    for (row, p) in vm.projects.iter().enumerate().take(end).skip(start) {
        let Ok(dy) = u16::try_from(row.saturating_sub(start)) else {
            break;
        };
        let y = inner.y + dy;
        let selected = row == vm.selected;
        // The selection pill: the row filled in the selection wash (BOLD keeps
        // it visible on the colourless floors, beside the `>` marker).
        let (row_style, marker) = if selected {
            // The selection pulse: the pill flashes toward the accent
            // on a cursor move, then decays to the resting selection wash.
            let pill = if vm.select_pulse > 0.0 {
                let bg = crate::theme::lerp_color(
                    crate::theme::ACCENT,
                    crate::theme::SELECT_BG,
                    1.0 - vm.select_pulse.clamp(0.0, 1.0),
                );
                vm.theme.selection().with_bg(bg)
            } else {
                vm.theme.selection()
            };
            crate::chrome::fill(
                frame,
                Rect {
                    x: inner.x,
                    y,
                    w: inner.w,
                    h: 1,
                },
                pill,
            );
            (pill, '>')
        } else {
            (vm.theme.body(), ' ')
        };
        // Linked worktrees render as indented children of their repository.
        let indent = match p.kind {
            EntryKind::Repo => "",
            EntryKind::Worktree => "  ",
        };
        // The label is untrusted (a directory name); sanitize before eliding.
        let label = elide(
            &sanitize_for_terminal(&p.name),
            label_w.saturating_sub(indent.len()),
        );
        frame.set(inner.x, y, marker, row_style);
        let kind_style = if selected { row_style } else { vm.theme.dim() };
        let mut x = frame.put_str(inner.x + 1, y, indent, row_style);
        frame.set(x, y, Icon::for_kind(p.kind).glyph(unicode), kind_style);
        x = x.saturating_add(1);
        frame.put_str(x + 1, y, &label, row_style);
        // The trailing status dot, pinned to the right edge in status colour.
        if inner.w >= 2 {
            let dot_x = inner.x + inner.w - 1;
            let mut dot = vm.theme.status_dot(p.status);
            if selected {
                dot = dot.with_bg(crate::theme::SELECT_BG);
            }
            frame.set(dot_x, y, Icon::for_status(p.status).glyph(unicode), dot);
        }
    }
    draw_rail_actions(vm, frame, &buttons, unicode);
    // The rail overflow scrollbar: tracks the selection window when
    // more projects exist than the list has rows — the visual half of the
    // silent paging `visible_window` already does.
    let bar_area = Rect {
        x: rect.x,
        y: list.y,
        w: rect.w,
        h: list.h,
    };
    draw_overflow_bar(frame, bar_area, start, vm.projects.len(), vm.theme, unicode);
}

/// Column 2 — the card stack: repository · models · activity, each
/// individually bordered and themed, sized by the
/// shared stack solver. Every projection-derived string is escape-sanitized in
/// [`crate::cards`] before it can reach a cell (the security model). No project
/// selected → a single placeholder line.
fn draw_detail(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    rect: Rect,
    unicode: bool,
    units: &[crate::cards::CardKind],
) {
    let Some(detail) = vm.detail.as_deref() else {
        clip_line(frame, rect, 0, "no project selected", vm.theme.dim());
        return;
    };
    let ctx = crate::cards::CardCtx {
        detail,
        activity: &vm.activity,
        focused: vm.focus == Panel::Detail,
        unicode,
        pressed: vm.pressed.clone(),
        press_pulse: vm.press_pulse,
    };
    crate::cards::draw(
        &ctx,
        vm.theme,
        units,
        rect,
        vm.detail_scroll,
        backend,
        frame,
    );
    // The card-stack overflow scrollbar: shown only when the column is
    // too short to host every card (the collapse-solve's lossy tier — cards are
    // one row each there, so the row/card counts line up).
    draw_overflow_bar(
        frame,
        rect,
        vm.detail_scroll,
        crate::cards::content_len(units),
        vm.theme,
        unicode,
    );
}

/// Column 3 — the live panels: token usage, system (the donut + domain
/// metrics), recall, and the two workflow cards — each an individually-bordered themed
/// card filling the column, solved by the shared stack. No outer border (like
/// the column-2 cards); focus accents the panel borders.
fn draw_graphs(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    rect: Rect,
    ids: &[crate::col3::SectionId],
) {
    let unicode = backend.capabilities().contains(Capabilities::UNICODE);
    crate::col3::draw(
        &vm.col3,
        ids,
        vm.focus == Panel::Graphs,
        unicode,
        rect,
        vm.graphs_scroll,
        backend,
        frame,
    );
    // The overflow scrollbar: drawn on the column's right edge only
    // when the fully-expanded panels exceed the column — the visible sign of the
    // "j/k scroll" the hint bar teaches.
    draw_overflow_bar(
        frame,
        rect,
        vm.graphs_scroll,
        usize::from(crate::col3::content_rows(ids)),
        vm.theme,
        unicode,
    );
}

/// Draw an overflow scrollbar on `area`'s right edge when its content
/// exceeds the viewport, tracking `(offset, content_len)`. Chrome — the bar's
/// presence honestly means "there is more"; a fitting region draws nothing.
/// Returns whether a bar was drawn (so the caller can reserve for it / hit-test).
fn draw_overflow_bar(
    frame: &mut Frame,
    area: Rect,
    offset: usize,
    content_len: usize,
    theme: crate::theme::Theme,
    unicode: bool,
) -> bool {
    if let Some(bar) = crate::chrome::scrollbar(area, offset, content_len) {
        crate::chrome::draw_scrollbar(frame, &bar, theme, unicode);
        true
    } else {
        false
    }
}

/// Print `text` on row `dy` of `area`, truncated to the area's width and dropped
/// when it falls below the area — so a panel never bleeds into its neighbor.
fn clip_line(frame: &mut Frame, area: Rect, dy: u16, text: &str, style: Style) {
    if dy >= area.h {
        return;
    }
    let max = usize::from(area.w);
    let clipped: String = text.chars().take(max).collect();
    frame.put_str(area.x, area.y + dy, &clipped, style);
}

/// Print `text` at `(x, y)`, clipped to `width` cells.
fn clip_at(frame: &mut Frame, x: u16, y: u16, width: u16, text: &str, style: Style) {
    let clipped: String = text.chars().take(usize::from(width)).collect();
    frame.put_str(x, y, &clipped, style);
}

/// Truncate `s` to at most `w` display cells, appending an ellipsis when it is cut.
fn elide(s: &str, w: usize) -> String {
    let count = s.chars().count();
    if count <= w {
        return s.to_string();
    }
    match w {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let mut out: String = s.chars().take(w - 1).collect();
            out.push('…');
            out
        }
    }
}

/// Truncate `s` to at most `w` cells from the **left**, prefixing an ellipsis
/// when it is cut — the mirror of [`elide`], for paths, where the tail (the
/// basename) is the part that identifies what you are looking at.
fn elide_left(s: &str, w: usize) -> String {
    let count = s.chars().count();
    if count <= w {
        return s.to_string();
    }
    match w {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let mut out = String::from("…");
            out.extend(s.chars().skip(count - (w - 1)));
            out
        }
    }
}

/// The visible `[start, end)` slice of a `len`-item list of `height` rows that
/// keeps `selected` in view (scroll-to-selection). Deterministic in its inputs.
/// Shared with [`crate::hit`] so a click resolves to the same project row the
/// renderer drew.
pub(crate) fn visible_window(selected: usize, len: usize, height: usize) -> (usize, usize) {
    if height == 0 || len == 0 {
        return (0, 0);
    }
    let h = height.min(len);
    // Pin so `selected` stays within the window; clamp the top to the last page.
    let start = selected.saturating_sub(h - 1).min(len - h);
    (start, start + h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;
    use crate::state::{update, AppState, Event, ProjectData};
    use crate::{CaptureSummary, DetailView, EntryKind, ModelUsage, Project, ProjectStatus};

    /// The label matrix for the alpha capture scope: a coming-soon row says
    /// so instead of "not wired" (which advertises a toggle the manager
    /// refuses), while a leftover artifact keeps its real state visible
    /// beside the scope fact and a supported row is untouched.
    #[test]
    fn a_coming_soon_integration_row_reads_coming_soon_not_not_wired() {
        let row = |agent: &str, wiring, alpha_supported| crate::NativeAgentIntegrationRow {
            agent: agent.to_owned(),
            display_name: agent.to_owned(),
            detection: crate::NativeAgentDetection::Detected,
            executable_name: Some(agent.to_owned()),
            installations: vec![crate::NativeAgentInstallation {
                source: crate::NativeAgentInstallSource::PathCommand,
                version: None,
            }],
            wiring,
            hook_wiring: None,
            alpha_supported,
            selected: false,
        };
        let mut state = crate::ProjectSettingsModalState::default();
        state.open(crate::ProjectSettingsView {
            path: "/r/demo".into(),
            project_name: "demo".into(),
            initialization: crate::ProjectInitialization::Initialized,
            settings: Vec::new(),
            policy: crate::ProjectPolicySummary::default(),
            integrations: vec![
                row("gemini", crate::NativeAgentWiring::Missing, false),
                row("antigravity", crate::NativeAgentWiring::Missing, true),
                row("cursor", crate::NativeAgentWiring::Conflict, false),
            ],
        });
        let labels: Vec<String> = project_settings_display_rows(&state)
            .into_iter()
            .filter_map(|row| match row {
                ProjectSettingsDisplayRow::Action { label, .. } => Some(label),
                _ => None,
            })
            .collect();
        assert!(
            labels
                .iter()
                .any(|l| l.contains("gemini · PATH command · coming soon")),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|l| l.contains("antigravity · PATH command · not wired")),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|l| l.contains("cursor · PATH command · conflict · coming soon")),
            "{labels:?}"
        );
    }

    fn vm() -> ViewModel {
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
            Size::new(120, 40),
        );
        s.app.splash.skip();
        s.snapshot()
    }

    fn render_to_text(vm: &ViewModel) -> String {
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(vm, &mut b, &mut f);
        let mut out = String::new();
        for y in 0..f.height() {
            for x in 0..f.width() {
                out.push(f.get(x, y).map_or(' ', |c| c.ch));
            }
            out.push('\n');
        }
        out
    }

    /// A view-model in a picker/menu-capable state (one live profile).
    fn profiled_state() -> AppState {
        let mut s = AppState::new(
            vec![Project {
                name: "rift-cli".into(),
                path: "/r/rift-cli".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        s.app.set_profiles(
            vec![crate::profile::ProfileEntry {
                profile_id: "pf0".into(),
                name: "default".into(),
                created_at: 0,
            }],
            "pf0".into(),
        );
        s
    }

    #[test]
    fn profile_menu_lists_profiles_with_the_stage_line() {
        let mut s = profiled_state();
        crate::input::enter_profile_menu(&mut s.app);
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("profiles"), "the menu frame titles: {text}");
        assert!(text.contains("default"), "the live profile lists");
        assert!(text.contains("n new"), "the browse stage line teaches keys");
        // The hub's mouse affordances (the profile hub / modal-surface parity): a settings
        // chip per row, the new-profile row, and the close X.
        assert!(text.contains(PROFILE_SETTINGS_CHIP), "chips render: {text}");
        assert!(text.contains("[ + new profile ]"), "{text}");
        assert!(text.contains("[x]"), "the close X renders: {text}");
    }

    // ---- the destination explorer -------------------------------------------

    fn dir(name: &str) -> crate::explorer::ExplorerEntry {
        crate::explorer::ExplorerEntry {
            name: name.to_owned(),
            is_dir: true,
            tag: None,
        }
    }

    fn file(name: &str) -> crate::explorer::ExplorerEntry {
        crate::explorer::ExplorerEntry {
            name: name.to_owned(),
            is_dir: false,
            tag: None,
        }
    }

    /// A capability-bearing backend, so a glyph test can exercise BOTH sides of
    /// the one Unicode switch (`AsciiBackend` always reports the empty set).
    struct CapsBackend {
        caps: crate::render::Capabilities,
    }
    impl crate::render::RenderBackend for CapsBackend {
        fn name(&self) -> &'static str {
            "caps"
        }
        fn capabilities(&self) -> crate::render::Capabilities {
            self.caps
        }
        fn present(&mut self, _frame: &Frame) -> Result<(), crate::render::RenderError> {
            Ok(())
        }
    }

    /// The drawn text of one row of the explorer's modal — so a status-line
    /// assertion reads the modal itself, not the dashboard's hint bar beneath.
    fn explorer_row_text(s: &mut AppState, dy_from_inner_bottom: u16) -> String {
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&vm, &mut b, &mut f);
        let modal = modal_rect(vm.size);
        let y = modal.y + modal.h - 1 - dy_from_inner_bottom;
        (modal.x..modal.x + modal.w)
            .map(|x| f.get(x, y).map_or(' ', |c| c.ch))
            .collect()
    }

    /// A state browsing `/home/dev/codebase` with two dirs and a file listed.
    fn explorer_state() -> AppState {
        let mut s = profiled_state();
        s.app.home_dir = Some("/home/dev/codebase".into());
        crate::input::enter_explorer(&mut s.app);
        s.app.explorer.apply(crate::explorer::DirListing {
            path: std::path::PathBuf::from("/home/dev/codebase"),
            entries: vec![dir("alpha"), dir("beta"), file("notes.md")],
            omitted: 0,
            error: None,
        });
        s
    }

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

    /// The cursor cue on the floor: every modal list surface, rendered through
    /// the glyph-only ASCII backend, shows exactly one row wearing the marker
    /// in the modal's first inner column — the cursor's row — and the marker
    /// moves with the cursor. This is the property the colour-only pill lacked
    /// (the text goldens could not see it, and neither could a Windows
    /// operator, whose console the environment probe read as colourless).
    #[test]
    #[allow(clippy::too_many_lines)] // one stanza per list surface
    fn every_list_surface_marks_exactly_one_cursor_row_on_the_floor() {
        use crate::profile_menu::MenuStage;

        /// The rows inside the modal whose first inner cell carries the marker.
        fn marker_rows(s: &mut AppState) -> (Rect, Vec<u16>) {
            let vm = s.snapshot();
            let mut f = Frame::new(vm.size);
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&vm, &mut b, &mut f);
            let modal = modal_rect(vm.size);
            let rows = (modal.y..modal.y + modal.h)
                .filter(|&y| {
                    f.get(modal.x + 1, y)
                        .is_some_and(|c| c.ch == crate::chrome::CURSOR_MARKER)
                })
                .collect();
            (modal, rows)
        }
        fn assert_one_marker(name: &str, s: &mut AppState, expected_y: impl Fn(Rect) -> u16) {
            let (modal, rows) = marker_rows(s);
            assert_eq!(
                rows,
                vec![expected_y(modal)],
                "{name}: exactly one marked row, and it is the cursor's"
            );
        }
        let entry = |id: &str, name: &str| crate::profile::ProfileEntry {
            profile_id: id.to_owned(),
            name: name.to_owned(),
            created_at: 0,
        };

        // The destination explorer: over directories and the file alike.
        let mut explorer = explorer_state();
        for cursor in 0..3 {
            explorer.app.explorer.cursor = cursor;
            assert_one_marker(&format!("explorer cursor {cursor}"), &mut explorer, |m| {
                explorer_row_rect(m, cursor, 0).y
            });
        }

        // The profile menu, every list stage.
        let mut profile = profiled_state();
        profile.app.set_profiles(
            vec![entry("pf0", "default"), entry("pf1", "work")],
            "pf0".into(),
        );
        crate::input::enter_profile_menu(&mut profile.app);
        for cursor in 0..2 {
            profile.app.profile_menu.cursor = cursor;
            assert_one_marker(&format!("profile hub cursor {cursor}"), &mut profile, |m| {
                profile_menu_row_rect(m, cursor).y
            });
        }
        profile.app.profile_menu.stage = MenuStage::Settings {
            profile: "pf1".to_owned(),
        };
        for cursor in 0..2 {
            profile.app.profile_menu.settings_cursor = cursor;
            assert_one_marker(
                &format!("profile settings cursor {cursor}"),
                &mut profile,
                |m| profile_menu_row_rect(m, cursor).y,
            );
        }
        profile.app.set_membership_lists(
            vec![project("alpha"), project("beta")],
            vec![project("alpha"), project("beta"), project("gamma")],
        );
        profile.app.profile_menu.stage = MenuStage::Members {
            profile: "pf1".to_owned(),
        };
        for cursor in 0..2 {
            profile.app.profile_menu.member_cursor = cursor;
            assert_one_marker(&format!("members cursor {cursor}"), &mut profile, |m| {
                profile_menu_row_rect(m, cursor).y
            });
        }
        profile.app.profile_menu.stage = MenuStage::MembersAdd {
            profile: "pf1".to_owned(),
        };
        profile.app.profile_menu.add_cursor = 0;
        assert_one_marker("members add cursor 0", &mut profile, |m| {
            profile_menu_row_rect(m, 0).y
        });

        // The colour picker, both stages.
        let mut picker = profiled_state();
        crate::input::enter_profile_menu(&mut picker.app);
        picker.app.mode = crate::input::Mode::ColourPicker;
        for cursor in 0..2 {
            picker.app.colour_picker.slot_cursor = cursor;
            assert_one_marker(&format!("picker slot cursor {cursor}"), &mut picker, |m| {
                colour_picker_row_rect(m, cursor).y
            });
        }
        picker.app.colour_picker.stage = crate::colour_picker::PickerStage::Presets;
        for cursor in [0, crate::RESET_ROW] {
            picker.app.colour_picker.preset_cursor = cursor;
            assert_one_marker(
                &format!("picker preset cursor {cursor}"),
                &mut picker,
                |m| colour_picker_row_rect(m, cursor).y,
            );
        }

        // Project Settings: the action rows, which the cursor walks.
        let mut settings = state_with_detail(ProjectStatus::Idle, "model");
        let view = settings.snapshot().detail.as_deref().map_or_else(
            || crate::ProjectSettingsView::thin("/r/p".into(), "p".into()),
            |detail| detail.project_settings.as_ref().clone(),
        );
        settings.app.project_settings.open(view);
        settings.app.mode = crate::input::Mode::ProjectSettings;
        let rows = project_settings_display_rows(&settings.app.project_settings);
        let actions: Vec<(usize, usize)> = rows
            .iter()
            .enumerate()
            .filter_map(|(line, row)| match row {
                ProjectSettingsDisplayRow::Action { index, .. } => Some((line, *index)),
                _ => None,
            })
            .collect();
        assert!(
            !actions.is_empty(),
            "the fixture lists at least one action: {actions:?}"
        );
        for (line, index) in actions.iter().take(2) {
            settings.app.project_settings.cursor = *index;
            let line = u16::try_from(*line).expect("a short listing");
            assert_one_marker(
                &format!("project settings action {index}"),
                &mut settings,
                |m| project_settings_body_rect(m).y + line,
            );
        }
    }

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

    #[test]
    fn the_background_slot_repaints_panels_and_modals() {
        // A finding from real use (2026-07-17): a background override repainted
        // the header but not the panel or modal backdrops. The surface now
        // paints its own background and every foreground-only write inherits
        // it (the frame's transparent-Default compose rule) — so the slot
        // applies to EVERY element's backdrop, per the runtime surface theme
        let bg = crate::render::frame::Color::Rgb(0x11, 0x22, 0x33);
        let mut s = profiled_state();
        let mut overrides = crate::theme::ThemeOverrides::NONE;
        overrides.set(crate::theme::ThemeSlot::Background, Some(bg));
        s.set_theme(overrides);
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::UNICODE);
        draw(&vm, &mut b, &mut f);
        // A panel-interior cell (mid-body) wears the override…
        let mid = f
            .get(vm.size.w / 2, vm.size.h / 2)
            .expect("mid-body cell exists");
        assert_eq!(mid.style.bg, bg, "panel backdrops wear the background slot");
        // …and so does a modal's interior once the menu opens.
        crate::input::enter_profile_menu(&mut s.app);
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        draw(&vm, &mut b, &mut f);
        let modal = modal_rect(vm.size);
        let inside = f
            .get(modal.x + modal.w / 2, modal.y + modal.h - 2)
            .expect("modal interior cell exists");
        assert_eq!(
            inside.style.bg, bg,
            "modal backdrops wear the background slot"
        );
        // Unthemed, the surface paints Rift dark for itself — never the
        // terminal's own background.
        let mut s = profiled_state();
        s.set_theme(crate::theme::ThemeOverrides::NONE);
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        draw(&vm, &mut b, &mut f);
        let mid = f.get(vm.size.w / 2, vm.size.h / 2).unwrap();
        assert_eq!(mid.style.bg, crate::theme::SURFACE_BG);
    }

    #[test]
    fn settings_page_lists_rows_with_the_pin_state() {
        use crate::profile_menu::{MenuStage, SettingsRow};
        let mut s = profiled_state();
        crate::input::enter_profile_menu(&mut s.app);
        s.app.profile_menu.stage = MenuStage::Settings {
            profile: "pf0".into(),
        };
        s.app.startup_profile = Some("pf0".into());
        let text = render_to_text(&s.snapshot());
        assert!(
            text.contains("profile · default"),
            "the frame titles the page's profile: {text}"
        );
        for row in SettingsRow::ALL {
            assert!(text.contains(row.label()), "row {row:?} renders: {text}");
        }
        assert!(
            text.contains("startup pin  [on]"),
            "the pin row shows its state: {text}"
        );
        assert!(text.contains("⏎ open · esc back"), "{text}");
        // Unpinned, the same row reads [off].
        s.app.startup_profile = None;
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("startup pin  [off]"), "{text}");
    }

    #[test]
    fn picker_lists_presets_as_name_beside_a_swatch() {
        // the colour picker and its presets: stage two is one row per preset — the NAME beside a
        // solid swatch box — plus the reset row. On the colour tiers the
        // swatch carries the preset's own RGB; on mono the name alone carries
        // (the glyphs still render; colour resolves away by construction).
        let mut s = profiled_state();
        crate::input::enter_profile_menu(&mut s.app);
        s.app.mode = crate::input::Mode::ColourPicker;
        s.app.colour_picker.stage = crate::colour_picker::PickerStage::Presets;
        let v = s.snapshot();
        let mut f = Frame::new(v.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::UNICODE);
        draw(&v, &mut b, &mut f);
        let modal = modal_rect(v.size);
        for (i, (name, colour)) in crate::theme::PRESETS.iter().enumerate() {
            let rr = colour_picker_row_rect(modal, i);
            assert!(rr.h > 0, "every preset row fits the frame");
            let row: String = (rr.x..rr.x + rr.w)
                .map(|x| f.get(x, rr.y).map_or(' ', |c| c.ch))
                .collect();
            assert!(row.contains(name), "row {i} names its preset: {row}");
            // The swatch box sits right after the padded name, in the colour.
            let sx = rr.x + 2 + 8;
            let cell = f.get(sx, rr.y).unwrap();
            assert_eq!(cell.ch, '#', "a solid swatch box (ASCII backend)");
            assert_eq!(cell.style.fg, *colour, "the swatch IS the colour");
        }
        // The reset row sits beside the presets.
        let rr = colour_picker_row_rect(modal, crate::colour_picker::RESET_ROW);
        let row: String = (rr.x..rr.x + rr.w)
            .map(|x| f.get(x, rr.y).map_or(' ', |c| c.ch))
            .collect();
        assert!(row.contains("reset to default"), "{row}");
    }

    #[test]
    fn picker_slot_stage_shows_current_values() {
        // Target-aware: the slot stage reads the TARGET's
        // overrides — here a profile whose border is overridden — not the
        // live theme, so a non-active page tells the truth.
        let mut state = profiled_state();
        let mut overrides = crate::theme::ThemeOverrides::NONE;
        overrides.set(
            crate::theme::ThemeSlot::Border,
            Some(crate::theme::PRESETS[1].1),
        );
        state.app.mode = crate::input::Mode::ColourPicker;
        state.app.colour_picker.target_profile = "pf0".into();
        state.app.colour_picker.target_overrides = overrides;
        let view_model = state.snapshot();
        let mut frame = Frame::new(view_model.size);
        let mut backend = crate::render::AsciiBackend::new(crate::render::Capabilities::UNICODE);
        draw(&view_model, &mut backend, &mut frame);
        let modal = modal_rect(view_model.size);
        let text: String = (0..frame.height())
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(text.contains("background"), "the slots list");
        assert!(text.contains("default"), "an unset slot reads default");
        // The overridden border slot shows a swatch in its colour.
        let rr = colour_picker_row_rect(modal, 1);
        let sx = rr.x + 2 + 12;
        let cell = frame.get(sx, rr.y).unwrap();
        assert_eq!(cell.ch, '#');
        assert_eq!(cell.style.fg, crate::theme::PRESETS[1].1);
    }

    #[test]
    fn draws_three_panels_and_command_bar() {
        let text = render_to_text(&vm());
        // The header: mark + product + the right-aligned actions.
        assert!(text.contains("RIFT"));
        assert!(text.contains("Canopy"));
        assert!(text.contains("? Help"));
        assert!(text.contains("q Quit"));
        assert!(text.contains("projects"));
        assert!(text.contains("rift-cli"));
        // Column 2 is the card stack, not a single "detail" panel.
        assert!(text.contains("repository"));
        assert!(text.contains("models"));
        // The hint bar teaches the focused (projects) keymap.
        assert!(text.contains("j/k navigate"));
        // Column 3 regroups into its own panels, each honest
        // about absent data (never a fake zero).
        assert!(text.contains("token usage"));
        assert!(text.contains("no token activity yet"));
        assert!(text.contains("system"));
        assert!(text.contains("recall"));
    }

    #[test]
    fn a_long_rail_shows_a_scrollbar_a_short_one_does_not() {
        use crate::render::frame::Size;
        // More projects than rows → the rail carries an overflow scrollbar (its
        // track/thumb glyph on the right edge); a fitting rail carries none.
        let many: Vec<Project> = (0..30)
            .map(|i| Project {
                name: format!("proj-{i}"),
                path: format!("/r/{i}"),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            })
            .collect();
        let mut s = AppState::new(many, Size::new(90, 20));
        s.app.splash.skip();
        let text = render_to_text(&s.snapshot());
        let has_bar = |t: &str| t.contains('█') || t.contains('│') || t.contains('#');
        assert!(
            has_bar(&text),
            "the overflowing rail shows a scrollbar:\n{text}"
        );

        let mut few = AppState::new(
            vec![Project {
                name: "solo".into(),
                path: "/r/solo".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(90, 20),
        );
        few.app.splash.skip();
        // The projects rail itself carries no thumb glyph when a single row fits
        // (the rail is column 1; a `█` there would be the bar). Scan column-1 rows.
        let vm = few.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&vm, &mut b, &mut f);
        let col1 = crate::layout::solve(Rect {
            x: 0,
            y: 1,
            w: 90,
            h: 19,
        })
        .col1;
        let edge = col1.x + col1.w - 1;
        for y in col1.y..(col1.y + col1.h) {
            assert_ne!(
                f.get(edge, y).map(|c| c.ch),
                Some('#'),
                "a fitting rail draws no scrollbar thumb"
            );
        }
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

    #[test]
    fn add_project_bar_shows_path() {
        let mut s = AppState::new(vec![], Size::new(100, 30));
        s.app.splash.skip();
        s.app.mode = Mode::AddProject;
        s.app.command_line = "/tmp/repo".into();
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("/tmp/repo"));
        assert!(text.contains("add project"));
    }

    #[test]
    fn focused_panel_border_is_highlighted() {
        use crate::render::frame::Color;
        use crate::Panel;
        let accent = Color::Rgb(0x6c, 0xe0, 0x9a);

        let mut s = AppState::new(
            vec![Project {
                name: "p".into(),
                path: "/r/p".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        s.app.focus = Panel::Projects;
        let mut f = Frame::new(s.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&s.snapshot(), &mut b, &mut f);

        // col1's top-left corner sits at (0, 1), just below the title row.
        let c1 = f.get(0, 1).unwrap();
        assert_eq!(c1.ch, '╭');
        assert_eq!(
            c1.style.fg, accent,
            "the focused panel's border is accented"
        );

        // At width 120 col1 is 24 wide, so col2's border begins at x = 24.
        let c2 = f.get(24, 1).unwrap();
        assert_eq!(c2.ch, '╭');
        assert_ne!(
            c2.style.fg, accent,
            "an unfocused panel's border is not accented"
        );
    }

    #[test]
    fn rail_shows_count_badge_status_dots_and_selection_pill() {
        use crate::theme::{status_color, SELECT_BG};
        let mut s = AppState::new(
            vec![
                Project {
                    name: "alpha".into(),
                    path: "/r/alpha".into(),
                    status: ProjectStatus::Running {
                        since_ms: 0,
                        active_sessions: 1,
                    },
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "beta".into(),
                    path: "/r/beta".into(),
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
            ],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        let mut f = Frame::new(s.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&s.snapshot(), &mut b, &mut f);

        // The count badge shows "2" on the title row (col1 spans x=0..24).
        let title: String = (0..24).map(|x| f.get(x, 1).unwrap().ch).collect();
        assert!(title.contains('2'), "the project count badge: {title}");

        // Row 0 of the rail (inner y=2) is the selected `alpha` — its trailing
        // status dot is running-green, and the pill wash tints the row.
        let dot_x = 22; // col1 inner right edge (width 24 → inner 1..23)
        assert_eq!(
            f.get(dot_x, 2).unwrap().style.fg,
            status_color(ProjectStatus::Running {
                since_ms: 0,
                active_sessions: 1
            }),
            "the running dot is green"
        );
        assert_eq!(
            f.get(2, 2).unwrap().style.bg,
            SELECT_BG,
            "the selected row carries the pill wash"
        );
        // Row 1 (`beta`, unselected) has no pill wash — just the surface the
        // dashboard paints for itself (the background slot's canvas).
        assert_eq!(f.get(2, 3).unwrap().style.bg, crate::theme::SURFACE_BG);
        // …and its idle dot is the idle slate.
        assert_eq!(
            f.get(dot_x, 3).unwrap().style.fg,
            status_color(ProjectStatus::Idle)
        );
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (2, 3), (5, 2)] {
            let mut s = AppState::new(vec![], Size::new(w, h));
            s.app.splash.skip();
            let mut f = Frame::new(Size::new(w, h));
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&s.snapshot(), &mut b, &mut f); // must not panic
        }
    }

    /// A populated state at the given size, past the splash.
    fn sized(w: u16, h: u16) -> AppState {
        let mut s = AppState::new(
            vec![
                Project {
                    name: "alpha".into(),
                    path: "/r/alpha".into(),
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "beta".into(),
                    path: "/r/beta".into(),
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
            ],
            Size::new(w, h),
        );
        s.app.splash.skip();
        s
    }

    #[test]
    fn two_col_stacks_detail_over_graphs() {
        assert_eq!(choose_mode(Size::new(60, 20)), LayoutMode::TwoCol);
        let text = render_to_text(&sized(60, 20).snapshot());
        assert!(text.contains("projects"));
        // Both columns are short here and collapse to headlines — the card
        // stack's `<name> · <status>` sits above the col-3 panels' placeholder.
        let detail_row = text.lines().position(|l| l.contains("· idle"));
        let graphs_row = text.lines().position(|l| l.contains("no token activity"));
        assert!(
            detail_row < graphs_row && detail_row.is_some(),
            "the card stack sits above graphs in two-column mode ({detail_row:?} < {graphs_row:?})"
        );
    }

    #[test]
    fn single_panel_tab_switches_active() {
        assert_eq!(choose_mode(Size::new(40, 16)), LayoutMode::SinglePanel);
        let mut s = sized(40, 16);

        s.app.focus = Panel::Detail;
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("[Detail]"), "focused tab is bracketed");
        // The detail column is the card stack; the (collapsed) repository card
        // headline shows the project name + status.
        assert!(text.contains("· idle"), "the card stack is shown");

        s.app.focus = Panel::Graphs;
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("[Graphs]"));
        // The col-3 panels are short here → collapsed to the token headline.
        assert!(
            text.contains("no token activity"),
            "the graphs panel is shown"
        );
    }

    #[test]
    fn too_small_shows_notice_and_recovers() {
        let text = render_to_text(&sized(20, 5).snapshot());
        assert!(text.contains("too small"), "the notice is shown when tiny");
        // Enlarging recovers the full dashboard.
        let text = render_to_text(&sized(120, 40).snapshot());
        assert!(!text.contains("too small"));
        assert!(text.contains("projects"));
    }

    #[test]
    fn long_names_truncate_with_ellipsis() {
        let mut s = sized(120, 40);
        s.app.projects = vec![Project {
            name: "a-really-long-project-name-that-cannot-possibly-fit".into(),
            path: "/r/x".into(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }];
        let text = render_to_text(&s.snapshot());
        assert!(text.contains('…'), "an over-long label is ellipsized");
    }

    /// A single-project state whose selected project's rich detail carries `model`.
    fn state_with_detail(status: ProjectStatus, model: &str) -> AppState {
        let mut s = AppState::new(
            vec![Project {
                name: "p".into(),
                path: "/r/p".into(),
                status,
                kind: EntryKind::Repo,
            }],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        update(
            &mut s,
            Event::ProjectData(ProjectData {
                project: "/r/p".into(),
                status,
                detail: Some(DetailView {
                    running_members: Vec::new(),
                    path: "/r/p".into(),
                    label: "p".into(),
                    kind: EntryKind::Repo,
                    status,
                    models: vec![ModelUsage {
                        model: model.into(),
                        calls: 3,
                        source: crate::col2::ModelProvenance::Runtime,
                    }],
                    capture: CaptureSummary::default(),
                    project_settings: Box::new(crate::ProjectSettingsView::thin(
                        "/r/p".into(),
                        "p".into(),
                    )),
                }),
            }),
        );
        s
    }

    #[test]
    fn model_name_with_ansi_escape_is_sanitized() {
        // A hostile model name carrying an SGR escape must not reach any cell as a
        // control character — the ESC is stripped, the inert text remains.
        let mut s = state_with_detail(ProjectStatus::Idle, "evil\x1b[31mmodel");
        let mut f = Frame::new(s.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&s.snapshot(), &mut b, &mut f);
        for y in 0..f.height() {
            for x in 0..f.width() {
                if let Some(cell) = f.get(x, y) {
                    assert!(
                        !cell.ch.is_control(),
                        "cell ({x},{y}) holds a control char {:?}",
                        cell.ch
                    );
                }
            }
        }
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("evil[31mmodel"), "the inert text survives");
    }

    #[test]
    fn uninitialized_project_shows_project_settings_entry_point() {
        let mut s = state_with_detail(ProjectStatus::NotInitialized, "unused");
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("not initialized"));
        assert!(text.contains("Project Settings"));
    }

    #[test]
    fn detail_renders_sections_when_ready() {
        let mut s = state_with_detail(ProjectStatus::Idle, "claude-opus");
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("models"));
        assert!(text.contains("claude-opus"));
        assert!(text.contains("activity"));
        assert!(!text.contains("│ settings"));
        assert!(!text.contains("│ features"));
        assert!(!text.contains("│ integrations"));
        // The retired capture card draws no title. Matched on the card's
        // border-and-title shape, not the bare word: the dispatch panel's
        // "no capture projection here" placeholder contains it too, and an
        // assertion those two states cannot tell apart proves nothing.
        assert!(
            !text.contains("- capture -"),
            "the capture card was retired from the stack: {text}"
        );
    }

    /// Render one immutable snapshot with the capability-bearing backend and
    /// return a tiny observation so the compiler cannot discard the draw.
    fn render_observation(vm: &ViewModel, caps: crate::render::Capabilities) -> usize {
        let mut frame = Frame::new(vm.size);
        let mut backend = CapsBackend { caps };
        draw(vm, &mut backend, &mut frame);
        (0..frame.height())
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .filter(|(x, y)| frame.get(*x, *y).is_some_and(|cell| cell.ch != ' '))
            .count()
    }

    fn project(name: &str) -> Project {
        Project {
            name: name.to_owned(),
            path: format!("/r/{name}"),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one case per live modal/layout state
    fn every_factual_surface_state_renders_through_the_root_view() {
        use crate::dispatch::{DispatchRequest, Message, Overlay};
        use crate::profile_menu::{MenuStage, SettingsRow};

        let profiles = ["default", "work"]
            .iter()
            .enumerate()
            .map(|(index, name)| crate::profile::ProfileEntry {
                profile_id: format!("pf{index}"),
                name: (*name).to_owned(),
                created_at: i64::try_from(index).unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let members = vec![project("alpha"), project("beta")];
        let catalogue = vec![project("alpha"), project("beta"), project("gamma")];
        let mut observations = Vec::new();

        let mut profile = profiled_state();
        profile.app.set_profiles(profiles, "pf0".to_owned());
        crate::input::enter_profile_menu(&mut profile.app);
        profile.app.profile_menu.cursor = 1;
        let mut view = profile.snapshot();
        view.pressed = Some(crate::hit::Target::ProfileSettings(1));
        view.press_pulse = 0.75;
        observations.push(render_observation(
            &view,
            crate::render::Capabilities::UNICODE,
        ));

        profile.app.profile_menu.stage = MenuStage::Settings {
            profile: "pf1".to_owned(),
        };
        profile.app.profile_menu.settings_cursor = SettingsRow::ALL.len() - 1;
        observations.push(render_observation(
            &profile.snapshot(),
            crate::render::Capabilities::empty(),
        ));
        profile.app.startup_profile = Some("pf1".to_owned());
        observations.push(render_observation(
            &profile.snapshot(),
            crate::render::Capabilities::UNICODE,
        ));

        profile
            .app
            .set_membership_lists(members.clone(), catalogue.clone());
        profile.app.profile_menu.stage = MenuStage::Members {
            profile: "pf1".to_owned(),
        };
        profile.app.profile_menu.member_cursor = 1;
        let mut view = profile.snapshot();
        view.pressed = Some(crate::hit::Target::MemberDown);
        view.press_pulse = 0.5;
        observations.push(render_observation(
            &view,
            crate::render::Capabilities::UNICODE,
        ));
        profile.app.profile_menu.members.clear();
        observations.push(render_observation(
            &profile.snapshot(),
            crate::render::Capabilities::empty(),
        ));

        profile
            .app
            .set_membership_lists(members.clone(), catalogue.clone());
        profile.app.profile_menu.stage = MenuStage::MembersAdd {
            profile: "pf1".to_owned(),
        };
        observations.push(render_observation(
            &profile.snapshot(),
            crate::render::Capabilities::UNICODE,
        ));
        profile
            .app
            .set_membership_lists(catalogue.clone(), catalogue);
        observations.push(render_observation(
            &profile.snapshot(),
            crate::render::Capabilities::empty(),
        ));

        for stage in [
            MenuStage::NamingCreate,
            MenuStage::NamingRename {
                profile: "pf1".to_owned(),
            },
            MenuStage::NamingDuplicate {
                source: "pf1".to_owned(),
            },
        ] {
            profile.app.profile_menu.stage = stage;
            profile.app.profile_menu.name_error = true;
            profile.app.command_line = "duplicate name".to_owned();
            observations.push(render_observation(
                &profile.snapshot(),
                crate::render::Capabilities::UNICODE,
            ));
        }
        profile.app.profile_menu.stage = MenuStage::ConfirmDelete {
            profile: "pf1".to_owned(),
            back_to_settings: true,
        };
        observations.push(render_observation(
            &profile.snapshot(),
            crate::render::Capabilities::empty(),
        ));

        let mut settings = state_with_detail(ProjectStatus::Idle, "model");
        let settings_view = settings.snapshot().detail.as_deref().map_or_else(
            || crate::ProjectSettingsView::thin("/r/p".into(), "p".into()),
            |detail| detail.project_settings.as_ref().clone(),
        );
        settings.app.project_settings.open(settings_view);
        settings.app.mode = crate::input::Mode::ProjectSettings;
        settings.app.project_settings.running = true;
        settings.app.project_settings.result = Some("landed".to_owned());
        observations.push(render_observation(
            &settings.snapshot(),
            crate::render::Capabilities::UNICODE,
        ));
        settings.app.project_settings.running = false;
        settings.app.project_settings.confirm_raw = true;
        let mut view = settings.snapshot();
        view.pressed = Some(crate::hit::Target::ProjectSettingsCancel);
        view.press_pulse = 1.0;
        observations.push(render_observation(
            &view,
            crate::render::Capabilities::empty(),
        ));
        settings.app.project_settings.confirm_raw = false;
        settings.app.project_settings.failed_action = Some(crate::ProjectSettingsAction {
            path: "/r/p".into(),
            kind: crate::ProjectSettingsActionKind::Retry,
        });
        observations.push(render_observation(
            &settings.snapshot(),
            crate::render::Capabilities::UNICODE,
        ));

        let mut explorer = explorer_state();
        explorer.app.explorer.cwd = std::path::PathBuf::from("/");
        explorer.app.explorer.show_hidden = true;
        explorer.app.explorer.entries.clear();
        observations.push(render_observation(
            &explorer.snapshot(),
            crate::render::Capabilities::UNICODE,
        ));

        let mut overlay = sized(100, 32);
        let cases = [
            Overlay::Results {
                title: "long results".to_owned(),
                lines: (0..80).map(|index| format!("line {index}")).collect(),
                scroll: 5,
                footer: Some("failed with class 2".to_owned()),
                truncated: true,
            },
            Overlay::Results {
                title: "empty results".to_owned(),
                lines: Vec::new(),
                scroll: 0,
                footer: None,
                truncated: false,
            },
            Overlay::Confirm {
                request: DispatchRequest::Rebuild,
                repo: "/r/alpha".to_owned(),
            },
            Overlay::running("rebuild"),
            Overlay::Running {
                title: "setup of alpha".to_owned(),
                lines: vec![
                    " ok  registry       alpha registered".to_owned(),
                    " ..  git hooks".to_owned(),
                ],
            },
            Overlay::Alert(Message::warning("warning", vec!["body".to_owned()])),
            Overlay::Notice(Message::success("success", vec!["body".to_owned()])),
        ];
        for (focus, case) in cases.into_iter().enumerate() {
            overlay.app.open_overlay(case);
            overlay.app.overlay_focus = focus;
            let mut view = overlay.snapshot();
            view.pressed = Some(crate::hit::Target::OverlayClose);
            view.press_pulse = 0.5;
            observations.push(render_observation(
                &view,
                crate::render::Capabilities::UNICODE,
            ));
        }

        let mut command = sized(80, 24);
        command.app.mode = crate::input::Mode::Command;
        command.app.focus = Panel::Command;
        command.app.command_line = "re".to_owned();
        let mut view = command.snapshot();
        view.sweep = 0.35;
        view.suggest_fade = 0.25;
        observations.push(render_observation(
            &view,
            crate::render::Capabilities::UNICODE,
        ));

        let mut editor = sized(120, 40);
        let repository = editor.app.layout.columns[1].units.remove(0);
        editor.app.layout.columns[2].units.insert(1, repository);
        crate::input::enter_panel_editor(&mut editor.app);
        editor.app.editor.cursor = crate::editor::Cursor { col: 2, unit: 1 };
        editor.app.editor.picked = true;
        observations.push(render_observation(
            &editor.snapshot(),
            crate::render::Capabilities::UNICODE,
        ));

        assert!(
            observations.iter().all(|count| *count > 0),
            "every factual state drew visible cells: {observations:?}"
        );
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

        /// Every terminal size yields a renderable layout — no panic, at any size.
        #[test]
        fn every_size_yields_a_renderable_layout(w in 0u16..400, h in 0u16..400) {
            let mut s = sized(w, h);
            let mut f = Frame::new(Size::new(w, h));
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&s.snapshot(), &mut b, &mut f); // must not panic at any size
        }

        /// No arbitrary detail string (model name) ever leaves a control character
        /// in a rendered cell — the sanitize gate holds for any bytes.
        #[test]
        fn no_control_chars_in_any_cell_for_arbitrary_model(model in "(?s).*") {
            let mut s = state_with_detail(ProjectStatus::Idle, &model);
            let mut f = Frame::new(s.size);
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&s.snapshot(), &mut b, &mut f);
            for y in 0..f.height() {
                for x in 0..f.width() {
                    if let Some(cell) = f.get(x, y) {
                        proptest::prop_assert!(!cell.ch.is_control());
                    }
                }
            }
        }
    }
}
