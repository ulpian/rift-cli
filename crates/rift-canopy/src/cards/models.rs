//! The models card: a repository-scoped model-call treemap with an exact,
//! provenance-aware key.

use crate::col2::sanitize_for_terminal;
use crate::layout::Rect;
use crate::render::frame::{Color, Frame};
use crate::render::{CellShaderKind, RenderBackend};
use crate::theme::{self, Section, Theme};
use crate::ProjectStatus;

use super::{as_f64, card_frame, line, CardCtx};

pub(super) fn draw_models(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    collapsed: bool,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let models = &ctx.detail.models;
    let total = models
        .iter()
        .fold(0u64, |sum, model| sum.saturating_add(model.calls));
    let title = format!("models · Σ {total} calls");
    let inner = card_frame(theme, Section::Models, &title, rect, frame);
    if models.is_empty() {
        if let Some(placeholder) = model_projection_placeholder(ctx.detail.status) {
            line(frame, inner, 0, placeholder, theme.dim());
            return;
        }
    }
    if collapsed {
        line(
            frame,
            inner,
            0,
            &format!("{} models · {total} calls", models.len()),
            theme.body(),
        );
        return;
    }

    if models.is_empty() {
        line(frame, inner, 0, "(none yet)", theme.dim());
        return;
    }

    let desired_key_rows = u16::try_from(models.len()).unwrap_or(u16::MAX);
    let key_rows = desired_key_rows
        .min(inner.h.saturating_sub(5))
        .max(1)
        .min(inner.h);
    let chart_rows = inner.h.saturating_sub(key_rows);
    if inner.w < 24 || chart_rows < 5 {
        // At intermediate geometry, exact ranked rows are more legible than a
        // treemap too small to distinguish its leaves.
        draw_model_key(ctx, theme, inner, frame);
        return;
    }
    let chart_area = Rect {
        h: chart_rows,
        ..inner
    };
    draw_model_treemap(ctx, theme, chart_area, backend, frame);
    draw_model_key(
        ctx,
        theme,
        Rect {
            y: inner.y + chart_rows,
            h: key_rows,
            ..inner
        },
        frame,
    );
}

/// The treemap over `area`: its outline, one Braille-shaded leaf per model
/// sized by calls, and each leaf's compact symbol — the exact names and counts
/// belong to the key below it.
fn draw_model_treemap(
    ctx: &CardCtx<'_>,
    theme: Theme,
    area: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let models = &ctx.detail.models;
    let plot_rect = crate::render::treemap_outline(frame, area, theme.dim(), ctx.unicode);
    let values: Vec<f64> = models.iter().map(|model| as_f64(model.calls)).collect();
    let colors: Vec<Color> = models
        .iter()
        .map(|model| model_usage_color(&model.model))
        .collect();
    let leaves = crate::charts::treemap_layout(&values);
    let samples = crate::charts::treemap_samples(&leaves, plot_rect, &colors);
    backend.shade_cells(&samples, plot_rect, CellShaderKind::Braille, frame);
    for leaf in leaves {
        let Some(model) = models.get(leaf.index) else {
            continue;
        };
        let label_rect = crate::charts::treemap_cell_rect(leaf, plot_rect);
        if label_rect.w == 0 || label_rect.h == 0 {
            continue;
        }
        let style = theme.tile_value(model_usage_color(&model.model));
        frame.set(
            label_rect.x,
            label_rect.y,
            model_leaf_symbol(leaf.index, ctx.unicode),
            style,
        );
    }
}

fn model_projection_placeholder(status: ProjectStatus) -> Option<&'static str> {
    match status {
        ProjectStatus::PickingUp => Some("(collecting model usage...)"),
        ProjectStatus::NotInitialized
        | ProjectStatus::NotProvisioned
        | ProjectStatus::SetupIncomplete
        | ProjectStatus::Error(crate::StatusError::ProjectionUnavailable) => {
            Some("(no projection yet)")
        }
        ProjectStatus::NotRepository => Some("(not a git repository)"),
        ProjectStatus::Error(crate::StatusError::RepoMissing) => Some("(repository unavailable)"),
        // `NotAdopted` says nothing about the projection: an unadopted member
        // reads its FAMILY's store, which is present and may be full. Claiming
        // "(no projection yet)" here would be the one placeholder on this card
        // that is not a fact.
        ProjectStatus::Running { .. }
        | ProjectStatus::Idle
        | ProjectStatus::NotAdopted
        | ProjectStatus::Error(
            crate::StatusError::CorruptSessionCache | crate::StatusError::Integration,
        ) => None,
    }
}

fn draw_model_key(ctx: &CardCtx<'_>, theme: Theme, rect: Rect, frame: &mut Frame) {
    if rect.h == 0 || rect.w == 0 {
        return;
    }
    let models = &ctx.detail.models;
    if models.is_empty() {
        line(frame, rect, 0, "(none yet)", theme.dim());
        return;
    }
    let visible = usize::from(rect.h);
    let show = models
        .len()
        .min(visible.saturating_sub(usize::from(models.len() > visible)));
    for (index, model) in models.iter().take(show).enumerate() {
        let Ok(row) = u16::try_from(index) else {
            break;
        };
        draw_model_key_row(ctx, theme, rect, row, index, model, frame);
    }
    let more = models.len().saturating_sub(show);
    if more > 0 {
        let tail_calls = models
            .iter()
            .skip(show)
            .fold(0u64, |sum, model| sum.saturating_add(model.calls));
        if let Ok(row) = u16::try_from(show) {
            line(
                frame,
                rect,
                row,
                &format!("+{more} more · {tail_calls} calls"),
                theme.dim(),
            );
        }
    }
}

fn draw_model_key_row(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    row: u16,
    index: usize,
    model: &crate::col2::ModelUsage,
    frame: &mut Frame,
) {
    if row >= rect.h || rect.w == 0 {
        return;
    }
    let color = model_usage_color(&model.model);
    frame.set(
        rect.x,
        rect.y + row,
        model_leaf_symbol(index, ctx.unicode),
        theme.tile_value(color),
    );
    let provenance = match model.source {
        crate::col2::ModelProvenance::Runtime => "runtime",
        crate::col2::ModelProvenance::Configured => "configured",
    };
    let pill = format!("×{} · {provenance}", model.calls);
    let pill_width = pill.chars().count();
    let name = sanitize_for_terminal(&model.model);
    let name = if name.is_empty() { "(unknown)" } else { &name };
    let name_width = usize::from(rect.w).saturating_sub(pill_width + 3);
    let clipped: String = name.chars().take(name_width).collect();
    if rect.w >= 2 {
        frame.put_str(rect.x + 2, rect.y + row, &clipped, theme.body());
    }
    if pill_width < usize::from(rect.w) {
        let x = rect.x + rect.w - u16::try_from(pill_width).unwrap_or(rect.w);
        frame.put_str(x, rect.y + row, &pill, theme.tile_value(color));
    }
}

fn model_leaf_symbol(index: usize, unicode: bool) -> char {
    u8::try_from(index)
        .ok()
        .filter(|index| *index < 26)
        .map_or(if unicode { '•' } else { '*' }, |index| {
            char::from(b'A' + index)
        })
}

fn model_usage_color(model: &str) -> Color {
    // FNV-1a is a tiny stable checksum here, not a security identity. It keeps
    // a model's key/leaf hue stable across sort-order and usage changes.
    let hash = model
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    let palette_len = u64::try_from(theme::MODEL_USAGE.len()).unwrap_or(1);
    let index = usize::try_from(hash % palette_len).unwrap_or(0);
    theme::MODEL_USAGE
        .get(index)
        .copied()
        .unwrap_or(Section::Models.accent())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::fixtures::{fixture_ctx, fixture_detail, fixture_rect};
    use crate::col2::{ModelProvenance, ModelUsage};
    use crate::col3::ActivityView;
    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities, RenderError, ShadeSample};

    #[derive(Default)]
    struct ShaderProbe {
        shader: Option<CellShaderKind>,
        samples: usize,
    }

    impl RenderBackend for ShaderProbe {
        fn name(&self) -> &'static str {
            "shader-probe"
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities::UNICODE | Capabilities::SUBCELL
        }

        fn present(&mut self, _frame: &Frame) -> Result<(), RenderError> {
            Ok(())
        }

        fn shade_cells(
            &mut self,
            samples: &[ShadeSample],
            _area: Rect,
            shader: CellShaderKind,
            _frame: &mut Frame,
        ) {
            self.shader = Some(shader);
            self.samples = samples.len();
        }
    }

    #[test]
    fn models_more_is_honest() {
        let mut d = fixture_detail(ProjectStatus::Idle);
        d.models = (0..10u64)
            .map(|i| ModelUsage {
                model: format!("m{i}"),
                calls: 10 - i,
                source: ModelProvenance::Runtime,
            })
            .collect();
        let act = ActivityView::NoProjection;
        // A short models card can't show all ten → an honest `+N more`.
        let models_rect = fixture_rect(0, 0, 40, 9);
        let mut f = Frame::new(Size::new(40, 9));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_models(
            &fixture_ctx(&d, &act),
            Theme::rift_dark(),
            models_rect,
            false,
            &mut b,
            &mut f,
        );
        let text: String = (0..f.height())
            .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
            .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(text.contains("more"), "the tail is counted: {text}");
        assert!(
            text.contains("45 calls"),
            "the hidden tail's calls remain exact: {text}"
        );
    }

    #[test]
    fn model_treemap_requests_braille_and_keeps_exact_key() {
        let mut d = fixture_detail(ProjectStatus::Idle);
        d.models.push(ModelUsage {
            model: "configured-only".into(),
            calls: 0,
            source: ModelProvenance::Configured,
        });
        let act = ActivityView::NoProjection;
        let mut frame = Frame::new(Size::new(48, 13));
        let mut backend = ShaderProbe::default();
        let mut model_ctx = fixture_ctx(&d, &act);
        model_ctx.unicode = true;
        draw_models(
            &model_ctx,
            Theme::rift_dark(),
            fixture_rect(0, 0, 48, 13),
            false,
            &mut backend,
            &mut frame,
        );
        assert_eq!(backend.shader, Some(CellShaderKind::Braille));
        assert!(
            backend.samples > 0,
            "positive calls produce treemap samples"
        );
        let text: String = (0..frame.height())
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(
            text.contains("Σ 15 calls"),
            "the title states scope: {text}"
        );
        assert!(text.contains("×12 · runtime"), "exact runtime key: {text}");
        assert!(
            text.contains("×0 · configured"),
            "zero-call observations remain in the exact key: {text}"
        );
        let chart_text: String = (1..9)
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(chart_text.contains('A') && chart_text.contains('B'));
        assert!(
            !chart_text.contains("claude") && !chart_text.contains('×'),
            "the plot keeps only compact symbols; exact text belongs in the key: {chart_text}"
        );
    }

    #[test]
    fn model_treemap_zero_total_has_empty_boundary_and_zero_key() {
        let mut d = fixture_detail(ProjectStatus::Idle);
        d.models = vec![ModelUsage {
            model: "configured-only".into(),
            calls: 0,
            source: ModelProvenance::Configured,
        }];
        let act = ActivityView::NoProjection;
        let mut frame = Frame::new(Size::new(40, 10));
        let mut backend = ShaderProbe::default();
        let model_ctx = fixture_ctx(&d, &act);
        draw_models(
            &model_ctx,
            Theme::rift_dark(),
            fixture_rect(0, 0, 40, 10),
            false,
            &mut backend,
            &mut frame,
        );
        assert_eq!(backend.shader, Some(CellShaderKind::Braille));
        assert_eq!(backend.samples, 0, "zero calls receive no area inflation");
        let text: String = (0..frame.height())
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(text.contains("+------------------------------------+"));
        assert!(text.contains("×0 · configured"));
    }

    #[test]
    fn model_card_degrades_to_ranked_rows_then_collapsed_total() {
        let d = fixture_detail(ProjectStatus::Idle);
        let act = ActivityView::NoProjection;
        let model_ctx = fixture_ctx(&d, &act);
        let mut compact = Frame::new(Size::new(22, 7));
        let mut compact_probe = ShaderProbe::default();
        draw_models(
            &model_ctx,
            Theme::rift_dark(),
            fixture_rect(0, 0, 22, 7),
            false,
            &mut compact_probe,
            &mut compact,
        );
        let compact_text: String = (0..compact.height())
            .flat_map(|y| (0..compact.width()).map(move |x| (x, y)))
            .map(|(x, y)| compact.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(compact_text.contains("×12"));
        assert_eq!(
            compact_probe.shader, None,
            "compact geometry uses exact rows"
        );

        let mut collapsed = Frame::new(Size::new(32, 3));
        let mut ascii = AsciiBackend::new(Capabilities::empty());
        draw_models(
            &model_ctx,
            Theme::rift_dark(),
            fixture_rect(0, 0, 32, 3),
            true,
            &mut ascii,
            &mut collapsed,
        );
        let collapsed_text: String = (0..collapsed.height())
            .flat_map(|y| (0..collapsed.width()).map(move |x| (x, y)))
            .map(|(x, y)| collapsed.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(collapsed_text.contains("2 models · 15 calls"));
    }

    #[test]
    fn empty_model_state_distinguishes_projection_availability() {
        let act = ActivityView::NoProjection;
        for (status, expected) in [
            (ProjectStatus::Idle, "(none yet)"),
            (ProjectStatus::PickingUp, "(collecting model usage...)"),
            (
                ProjectStatus::Error(crate::StatusError::ProjectionUnavailable),
                "(no projection yet)",
            ),
        ] {
            let mut d = fixture_detail(status);
            d.models.clear();
            let mut frame = Frame::new(Size::new(40, 9));
            let mut ascii = AsciiBackend::new(Capabilities::empty());
            draw_models(
                &fixture_ctx(&d, &act),
                Theme::rift_dark(),
                fixture_rect(0, 0, 40, 9),
                false,
                &mut ascii,
                &mut frame,
            );
            let text: String = (0..frame.height())
                .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
                .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
                .collect();
            assert!(text.contains(expected), "status {status:?}: {text}");
        }
    }

    /// Strengthened after review: the old "reordered" arm
    /// found `claude-opus` in an array and mapped the same single name — it
    /// never exercised a second order. The mapping is a pure FNV over the
    /// name, so order-independence is inherent; this now DEMONSTRATES it by
    /// mapping the same model set in both directions and comparing per-model,
    /// and pins every assignment inside the palette.
    #[test]
    fn model_palette_mapping_is_stable_and_order_independent() {
        let models = ["claude-opus", "claude-haiku", "gpt-5.6-sol", "gemini"];
        let forward: Vec<_> = models.iter().map(|m| model_usage_color(m)).collect();
        let backward: Vec<_> = models.iter().rev().map(|m| model_usage_color(m)).collect();
        for (i, m) in models.iter().enumerate() {
            assert_eq!(
                forward[i],
                backward[models.len() - 1 - i],
                "{m} maps differently depending on visit order"
            );
            assert_eq!(forward[i], model_usage_color(m), "{m} is unstable");
            assert!(
                theme::MODEL_USAGE.contains(&forward[i]),
                "{m} left the palette"
            );
        }
    }

    #[test]
    fn model_leaf_symbols_are_compact_and_have_an_overflow_fallback() {
        assert_eq!(model_leaf_symbol(0, true), 'A');
        assert_eq!(model_leaf_symbol(25, true), 'Z');
        assert_eq!(model_leaf_symbol(26, true), '•');
        assert_eq!(model_leaf_symbol(26, false), '*');
    }
}
