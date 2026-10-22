//! Dashboard navigation and geometry shared by drawing, keys, and hit-testing.

use crate::layout::{chrome_split, Rect};
use crate::render::Size;

/// The six project views. Commands keep their own independent input focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DashboardTab {
    /// Repository and Recall summaries.
    #[default]
    Home,
    /// Observed model usage; read-only.
    Models,
    /// Capture activity.
    Activity,
    /// Recorded token usage.
    Tokens,
    /// Read-only checkpoint and transcript browser.
    Recall,
    /// Project policy and integrations.
    Settings,
}

impl DashboardTab {
    /// Navigation order, independent of legacy saved column layouts.
    pub const ALL: [Self; 6] = [
        Self::Home,
        Self::Models,
        Self::Activity,
        Self::Tokens,
        Self::Recall,
        Self::Settings,
    ];

    /// Visible title, shortened only on narrow terminals.
    #[must_use]
    pub const fn label(self, compact: bool) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Models => "Models",
            Self::Activity => "Activity",
            Self::Tokens if compact => "Tokens",
            Self::Tokens => "Token usage",
            Self::Recall if compact => "Recall",
            Self::Recall => "Recall viewer",
            Self::Settings => "Settings",
        }
    }

    /// Move one view, wrapping at the ends.
    #[must_use]
    pub fn next(self, forward: bool) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        let step = if forward { 1 } else { Self::ALL.len() - 1 };
        Self::ALL
            .get((index + step) % Self::ALL.len())
            .copied()
            .unwrap_or_default()
    }
}

/// The full-width main area; two navigation rows sit below the global header.
#[must_use]
pub fn geometry(size: Size) -> (Rect, Rect, Rect) {
    chrome_split(Rect {
        x: 0,
        y: 4,
        w: size.w,
        h: size.h.saturating_sub(4),
    })
}

/// The two Home boxes share exact rectangles between drawing and hit-testing.
#[must_use]
pub fn home_rects(size: Size) -> (Rect, Rect) {
    let (body, _, _) = geometry(size);
    let h = body.h.min(10).min(body.h / 2);
    (
        Rect { h, ..body },
        Rect {
            y: body.y + h,
            h: body.h.saturating_sub(h),
            ..body
        },
    )
}

/// The Recall status line precedes its existing measured-usage card.
#[must_use]
pub fn home_recall_content(size: Size) -> Rect {
    let (_, area) = home_rects(size);
    Rect {
        y: area.y.saturating_add(1),
        h: area.h.saturating_sub(1),
        ..area
    }
}

/// Clickable tab spans, also consumed by the renderer.
#[must_use]
pub fn tab_rects(size: Size, active: DashboardTab) -> Vec<(DashboardTab, Rect)> {
    let compact = size.w < 82;
    let limit = size.w.saturating_sub(4);
    let width = |tab: DashboardTab| u16::try_from(tab.label(compact).len()).unwrap_or(0) + 3;
    let active_index = DashboardTab::ALL
        .iter()
        .position(|tab| *tab == active)
        .unwrap_or(0);
    let mut start = 0;
    while start < active_index
        && 5 + DashboardTab::ALL[start..=active_index]
            .iter()
            .map(|tab| width(*tab))
            .sum::<u16>()
            > limit
    {
        start += 1;
    }
    let mut x = 5u16;
    let mut tabs = Vec::new();
    for tab in DashboardTab::ALL.iter().skip(start).copied() {
        let w = width(tab).saturating_sub(1);
        if x.saturating_add(w) > limit {
            break;
        }
        tabs.push((tab, Rect { x, y: 1, w, h: 1 }));
        x = x.saturating_add(w + 1);
    }
    tabs
}

/// Previous and next tab buttons remain visible on narrow terminals.
#[must_use]
pub fn tab_steps(size: Size, active: DashboardTab) -> [(DashboardTab, Rect); 2] {
    [
        (
            active.next(false),
            Rect {
                x: size.w.saturating_sub(4),
                y: 1,
                w: 2,
                h: 1,
            },
        ),
        (
            active.next(true),
            Rect {
                x: size.w.saturating_sub(2),
                y: 1,
                w: 2,
                h: 1,
            },
        ),
    ]
}

/// The temporary project drawer never reduces the main content's layout width.
#[must_use]
pub fn drawer_rect(size: Size) -> Rect {
    let (body, _, _) = geometry(size);
    Rect {
        w: body.w.min(46),
        ..body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_wraps_and_never_includes_retired_views() {
        assert_eq!(DashboardTab::Home.next(false), DashboardTab::Settings);
        assert_eq!(DashboardTab::Settings.next(true), DashboardTab::Home);
        assert_eq!(DashboardTab::ALL.len(), 6);
    }

    #[test]
    fn navigation_and_commands_never_overlap_the_content() {
        for size in [Size::new(80, 24), Size::new(120, 40)] {
            let (body, hint, command) = geometry(size);
            assert_eq!(body.y, 4);
            assert!(body.y + body.h <= hint.y);
            assert!(hint.y + hint.h <= command.y);
            for (_, rect) in tab_rects(size, DashboardTab::Home) {
                assert!(rect.x + rect.w <= size.w);
                assert!(rect.y < body.y);
            }
        }
    }
    #[test]
    fn narrow_navigation_keeps_the_active_tab_and_both_step_buttons_visible() {
        for w in [40, 60, 80, 120] {
            for tab in DashboardTab::ALL {
                let size = Size::new(w, 24);
                let tabs = tab_rects(size, tab);
                assert!(tabs.iter().any(|(shown, _)| *shown == tab));
                assert!(tabs.iter().all(|(_, r)| r.x + r.w <= w - 4));
                assert_eq!(tab_steps(size, tab)[1].0, tab.next(true));
            }
        }
    }
}
