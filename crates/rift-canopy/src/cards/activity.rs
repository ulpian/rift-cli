//! The activity card: three one-dimensional thirty-day heat strips for
//! events, sessions and checkpoints over a relative time scale.

use crate::col3::ActivityView;
use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Style};
use crate::render::{CellShaderKind, RenderBackend};
use crate::theme::{Icon, Section, Theme};

use super::{as_f64, card_frame, line, CardCtx};

/// One activity card's strip geometry and drawing choices, solved once and
/// shared by its three strips so they line up column for column.
struct StripPass {
    theme: Theme,
    inner: Rect,
    shader: CellShaderKind,
    /// The row-label gutter: a three-cell tag, or one letter on a narrow card.
    label_w: u16,
    /// The right-edge gutter for each strip's maximum; 0 on a narrow card.
    max_w: u16,
    plot_w: u16,
    strip_h: u16,
}

/// The three activity categories' colours (events, sessions, checkpoints),
/// sourced from the theme so the row labels and heat density match.
fn activity_cats() -> [(Icon, &'static str, Color); 3] {
    [
        (Icon::Events, "events", Section::Capture.accent()),
        (Icon::Sessions, "sess", Section::Repository.accent()),
        (Icon::Checkpoints, "checks", Section::Activity.accent()),
    ]
}

/// The activity card's title, carrying the off-strip remainder when there is
/// one.
///
/// **Byte-identical to the bare title when nothing was left off**, so a card on
/// a fully-placed month reads exactly as it always did and every existing
/// golden stays green — the change is visible only where something is actually
/// missing from the strips.
///
/// The title is the carrier rather than an extra row because the strips already
/// spend every row they have (three heat rows plus the time scale), and a fact
/// that only appears when the card is tall enough is a fact the reader cannot
/// rely on. It is drawn before the collapsed check for the same reason: a
/// collapsed card must not be the one that hides it.
fn activity_title(off: crate::col3::OffStrip) -> String {
    if !off.any() {
        return "activity · 30d".to_owned();
    }
    let parts: Vec<String> = [
        (off.events, "ev"),
        (off.sessions, "se"),
        (off.checkpoints, "ck"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, tag)| format!("{n} {tag}"))
    .collect();
    format!("activity · 30d · {} off-strip", parts.join(" "))
}

pub(super) fn draw_activity(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    collapsed: bool,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let data = match ctx.activity {
        ActivityView::NoProjection => None,
        ActivityView::Days(d) => Some(d),
    };
    let title = activity_title(data.map_or_else(Default::default, |d| d.off_strip));
    let inner = card_frame(theme, Section::Activity, &title, rect, frame);
    let Some(days) = data.map(|d| &d.days) else {
        line(frame, inner, 0, "(no projection yet)", theme.dim());
        return;
    };
    if collapsed {
        let today = days.last().cloned().unwrap_or_default();
        line(
            frame,
            inner,
            0,
            &format!(
                "today {} ev · {} se · {} ck",
                today.events, today.sessions, today.checkpoints
            ),
            theme.body(),
        );
        return;
    }
    if inner.h < 4 {
        return;
    }
    draw_activity_strips(ctx, theme, inner, days, backend, frame);
}

fn draw_activity_strips(
    ctx: &CardCtx<'_>,
    theme: Theme,
    inner: Rect,
    days: &[crate::col3::DayBar],
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let series: [Vec<u64>; 3] = [
        days.iter().map(|day| day.events).collect(),
        days.iter().map(|day| day.sessions).collect(),
        days.iter().map(|day| day.checkpoints).collect(),
    ];
    let Some(pass) = strip_pass(ctx, theme, inner, &series) else {
        return;
    };
    for (index, (_, name, color)) in activity_cats().into_iter().enumerate() {
        let values = series.get(index).map_or(&[][..], Vec::as_slice);
        draw_activity_strip(&pass, index, (name, color), values, backend, frame);
    }
    draw_activity_time_scale(
        frame,
        Rect {
            x: inner.x + pass.label_w,
            y: inner.y + inner.h - 1,
            w: pass.plot_w,
            h: 1,
        },
        ctx.unicode,
        theme.dim(),
    );
}

/// Split `inner` into the label gutter, the plot and the maximum-label
/// gutter, and pick the strip height — `None` when the card cannot hold three
/// strips over the time scale, so it draws no partial chart.
fn strip_pass(
    ctx: &CardCtx<'_>,
    theme: Theme,
    inner: Rect,
    series: &[Vec<u64>; 3],
) -> Option<StripPass> {
    let max_labels: Vec<String> = series
        .iter()
        .map(|values| crate::col3::fmt_km(as_f64(values.iter().copied().max().unwrap_or(0))))
        .collect();
    let label_w = if inner.w >= 20 { 3 } else { 1 };
    let max_w = if inner.w >= 20 {
        u16::try_from(max_labels.iter().map(String::len).max().unwrap_or(1))
            .unwrap_or(1)
            .min(6)
    } else {
        0
    };
    let plot_w = inner.w.saturating_sub(label_w).saturating_sub(max_w);
    if plot_w == 0 {
        return None;
    }
    let chart_rows = inner.h - 1;
    let strip_h = if chart_rows >= 6 { 2 } else { 1 };
    if chart_rows < strip_h * 3 {
        return None;
    }
    let shader = if ctx.unicode {
        CellShaderKind::Braille
    } else {
        CellShaderKind::LuminanceRamp
    };
    Some(StripPass {
        theme,
        inner,
        shader,
        label_w,
        max_w,
        plot_w,
        strip_h,
    })
}

/// Strip `index`: its row label, its heat cells scaled to its own maximum,
/// and that maximum at the right edge when the card is wide enough.
fn draw_activity_strip(
    pass: &StripPass,
    index: usize,
    (name, color): (&str, Color),
    values: &[u64],
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let inner = pass.inner;
    let y = inner.y + u16::try_from(index).unwrap_or(0) * pass.strip_h;
    let plot_rect = Rect {
        x: inner.x + pass.label_w,
        y,
        w: pass.plot_w,
        h: pass.strip_h,
    };
    let short = if pass.label_w >= 3 {
        match index {
            0 => "ev ",
            1 => "se ",
            _ => "ck ",
        }
    } else {
        name.get(..1).unwrap_or_default()
    };
    frame.put_str(inner.x, y + pass.strip_h / 2, short, Style::fg(color));
    let strip = crate::charts::heat_strip(values, plot_rect, color);
    backend.shade_cells(&strip.samples, plot_rect, pass.shader, frame);
    if pass.max_w > 0 {
        let label = crate::col3::fmt_km(as_f64(strip.max));
        let width = u16::try_from(label.len())
            .unwrap_or(pass.max_w)
            .min(pass.max_w);
        frame.put_str(
            inner.x + inner.w.saturating_sub(width),
            y + pass.strip_h / 2,
            &label,
            pass.theme.dim(),
        );
    }
}

fn draw_activity_time_scale(frame: &mut Frame, rect: Rect, unicode: bool, style: Style) {
    if rect.w < 5 || rect.h == 0 {
        return;
    }
    let oldest = if unicode { "−29d" } else { "-29d" };
    frame.put_str(rect.x, rect.y, oldest, style);
    if rect.w >= 18 {
        let middle = if unicode { "−15d" } else { "-15d" };
        let middle_w = u16::try_from(middle.chars().count()).unwrap_or(4);
        frame.put_str(
            rect.x + rect.w.saturating_sub(middle_w) / 2,
            rect.y,
            middle,
            style,
        );
    }
    let today = "today";
    let today_w = u16::try_from(today.len()).unwrap_or(5);
    if rect.w >= today_w.saturating_add(6) {
        frame.put_str(rect.x + rect.w - today_w, rect.y, today, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::fixtures::{fixture_ctx, fixture_detail, fixture_rect};
    use crate::col3::{ActivityData, DayBar};
    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};
    use crate::ProjectStatus;

    #[test]
    fn activity_card_renders_heat_strips_scales_and_placeholder() {
        let d = fixture_detail(ProjectStatus::Idle);
        // Missing projection → the honest placeholder.
        let placeholder = ActivityView::NoProjection;
        let mut f = Frame::new(Size::new(44, 12));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &fixture_ctx(&d, &placeholder),
            Theme::rift_dark(),
            fixture_rect(0, 0, 44, 12),
            false,
            &mut b,
            &mut f,
        );
        let text: String = (0..f.height())
            .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
            .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(text.contains("no projection"));

        // A dense month draws three independently-scaled heat strips.
        let days = ActivityView::Days(ActivityData {
            days: (0..30u64)
                .map(|i| DayBar {
                    weekday: "Mon",
                    events: i * 3,
                    sessions: i,
                    checkpoints: i / 2,
                })
                .collect(),
            ..ActivityData::default()
        });
        let mut f2 = Frame::new(Size::new(44, 12));
        let mut b2 = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &fixture_ctx(&d, &days),
            Theme::rift_dark(),
            fixture_rect(0, 0, 44, 12),
            false,
            &mut b2,
            &mut f2,
        );
        let mut density_cells = 0;
        for y in 0..f2.height() {
            for x in 0..f2.width() {
                if let Some(c) = f2.get(x, y) {
                    if ".:-=+*#%@".contains(c.ch) {
                        density_cells += 1;
                    }
                }
            }
        }
        assert!(density_cells > 0, "the heat strips draw density cells");
        let text2: String = (0..f2.height())
            .flat_map(|y| (0..f2.width()).map(move |x| (x, y)))
            .map(|(x, y)| f2.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(
            text2.contains("87"),
            "the events strip shows its own maximum: {text2}"
        );
        assert!(text2.contains("ev "), "events row is named: {text2}");
        assert!(text2.contains("se "), "sessions row is named: {text2}");
        assert!(text2.contains("ck "), "checkpoints row is named: {text2}");
        assert!(
            text2.contains("-29d"),
            "oldest relative tick renders: {text2}"
        );
        assert!(
            text2.contains("-15d"),
            "midpoint relative tick renders: {text2}"
        );
        assert!(text2.contains("today"), "today tick renders: {text2}");
    }

    /// The title is the only place the off-strip remainder can appear, so it
    /// is pinned on both sides: unchanged when nothing is missing (every
    /// existing golden depends on that), and naming what is missing when
    /// something is.
    #[test]
    fn the_activity_title_is_unchanged_until_something_is_off_the_strip() {
        use crate::col3::OffStrip;
        assert_eq!(activity_title(OffStrip::default()), "activity · 30d");
        assert_eq!(
            activity_title(OffStrip {
                events: 0,
                sessions: 0,
                checkpoints: 103,
            }),
            "activity · 30d · 103 ck off-strip",
            "the measured case: a hundred checkpoints no strip can place"
        );
        assert_eq!(
            activity_title(OffStrip {
                events: 2,
                sessions: 1,
                checkpoints: 4,
            }),
            "activity · 30d · 2 ev 1 se 4 ck off-strip",
            "categories appear in strip order and zeros are never listed"
        );
    }

    /// A quiet month and a month whose rows are all unplaceable render the same
    /// zero strips — the title is what tells them apart, and a *collapsed* card
    /// must not be the one that hides it.
    #[test]
    fn an_unplaceable_month_says_so_even_when_the_card_is_collapsed() {
        let d = fixture_detail(ProjectStatus::Idle);
        let blind = ActivityView::Days(ActivityData {
            days: (0..30u64)
                .map(|_| DayBar {
                    weekday: "Tue",
                    events: 0,
                    sessions: 0,
                    checkpoints: 0,
                })
                .collect(),
            off_strip: crate::col3::OffStrip {
                checkpoints: 103,
                ..crate::col3::OffStrip::default()
            },
        });
        for collapsed in [false, true] {
            let mut f = Frame::new(Size::new(44, 12));
            let mut b = AsciiBackend::new(Capabilities::empty());
            draw_activity(
                &fixture_ctx(&d, &blind),
                Theme::rift_dark(),
                fixture_rect(0, 0, 44, 12),
                collapsed,
                &mut b,
                &mut f,
            );
            let text: String = (0..f.height())
                .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
                .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
                .collect();
            assert!(
                text.contains("103 ck off-strip"),
                "collapsed={collapsed}: the remainder must reach the card: {text}"
            );
        }
    }

    #[test]
    fn a_quiet_month_still_draws_zero_scales_and_time_range() {
        // Thirty zero-count days are a real quiet month, distinct from the
        // missing-projection placeholder.
        let d = fixture_detail(ProjectStatus::Idle);
        let quiet = ActivityView::Days(ActivityData {
            days: (0..30u64)
                .map(|_| DayBar {
                    weekday: "Tue",
                    events: 0,
                    sessions: 0,
                    checkpoints: 0,
                })
                .collect(),
            ..ActivityData::default()
        });
        let mut f = Frame::new(Size::new(44, 12));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &fixture_ctx(&d, &quiet),
            Theme::rift_dark(),
            fixture_rect(0, 0, 44, 12),
            false,
            &mut b,
            &mut f,
        );
        let text: String = (0..f.height())
            .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
            .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(text.contains('0'), "each quiet strip shows a zero maximum");
        assert!(
            text.contains("today"),
            "the fixed time range still frames the quiet month"
        );
    }

    #[test]
    fn activity_compacts_to_one_row_strips_then_the_today_summary() {
        let d = fixture_detail(ProjectStatus::Idle);
        let days = ActivityView::Days(ActivityData {
            days: (0..30u64)
                .map(|i| DayBar {
                    weekday: "Wed",
                    events: i,
                    sessions: i / 2,
                    checkpoints: i / 3,
                })
                .collect(),
            ..ActivityData::default()
        });
        let mut compact = Frame::new(Size::new(44, 6));
        let mut backend = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &fixture_ctx(&d, &days),
            Theme::rift_dark(),
            compact.rect(),
            false,
            &mut backend,
            &mut compact,
        );
        let compact_text: String = (0..compact.height())
            .flat_map(|y| (0..compact.width()).map(move |x| (x, y)))
            .map(|(x, y)| compact.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(compact_text.contains("ev "));
        assert!(compact_text.contains("se "));
        assert!(compact_text.contains("ck "));
        assert!(compact_text.contains("today"));

        let mut collapsed = Frame::new(Size::new(44, 3));
        draw_activity(
            &fixture_ctx(&d, &days),
            Theme::rift_dark(),
            collapsed.rect(),
            true,
            &mut backend,
            &mut collapsed,
        );
        let collapsed_text: String = (0..collapsed.height())
            .flat_map(|y| (0..collapsed.width()).map(move |x| (x, y)))
            .map(|(x, y)| collapsed.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(collapsed_text.contains("today 29 ev · 14 se · 9 ck"));
    }
}
