//! The fixtures the column-3 test modules share: a rendered stack, one panel
//! read back as text, and the view-models the recall and token tests seed.
//!
//! One module so a fixture is built one way and a test in any panel's module
//! reads the same frame the goldens do.

use crate::render::frame::{Frame, Size};
use crate::render::{AsciiBackend, Capabilities};

use super::{
    draw, draw_one, BreakdownData, BreakdownView, Col3, FamilyShareRow, RecallView, SavingsData,
    SectionId, ShareData, TokensData, TokensView, WorktreeAxis, WorktreeSlice,
};

/// Render the panels into a `w`×`h` area (no outer border), unscrolled.
pub(super) fn draw_to(col3: &Col3, w: u16, h: u16) -> Frame {
    draw_at(col3, w, h, 0)
}

/// Render the panels into a `w`×`h` area, scrolled to `offset`.
pub(super) fn draw_at(col3: &Col3, w: u16, h: u16, offset: usize) -> Frame {
    let mut f = Frame::new(Size::new(w, h));
    let mut b = AsciiBackend::new(Capabilities::empty());
    draw(
        col3,
        &SectionId::ALL,
        false,
        false,
        f.rect(),
        offset,
        &mut b,
        &mut f,
    );
    f
}

pub(super) fn text_of(f: &Frame) -> String {
    (0..f.height())
        .map(|y| {
            (0..f.width())
                .map(|x| f.get(x, y).map_or(' ', |c| c.ch))
                .collect::<String>()
                + "\n"
        })
        .collect()
}

/// Draw ONE panel into a `w`×`h` frame and read it back as text. The
/// workflow panels take no backend and no shader tier, so this is the whole
/// render path for them.
pub(super) fn panel_text(col3: &Col3, id: SectionId, w: u16, h: u16, collapsed: bool) -> String {
    let mut f = Frame::new(Size::new(w, h));
    let mut b = AsciiBackend::new(Capabilities::empty());
    draw_one(col3, id, false, false, collapsed, f.rect(), &mut b, &mut f);
    text_of(&f)
}

/// A Col3 whose savings carry the USD estimate (the priced savings estimate fixtures).
pub(super) fn usd_col3(label: Option<&str>) -> Col3 {
    Col3 {
        savings: RecallView::Data(SavingsData {
            target_pct: Some(90),
            usd_saved_micros: label.map(|_| 6_250_000),
            usd_label: label.map(str::to_owned),
            usd_rows: vec![],
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: WorktreeAxis::default(),
            share: Box::new(ShareData {
                overall: Some(42.0),
                avoided_tokens: 6_000,
                read_tokens: 8_285,
                ..ShareData::default()
            }),
        }),
        ..Col3::default()
    }
}

/// A recall panel whose window holds both a read side and an avoided side.
pub(super) fn barred_col3(read: u64, avoided: u64) -> Col3 {
    Col3 {
        savings: RecallView::Data(SavingsData {
            target_pct: Some(90),
            usd_saved_micros: None,
            usd_label: None,
            usd_rows: vec![],
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: WorktreeAxis::default(),
            share: Box::new(ShareData {
                overall: Some(3.0),
                avoided_tokens: avoided,
                read_tokens: read,
                ..ShareData::default()
            }),
        }),
        ..Col3::default()
    }
}

/// A token panel with a total, at a width column 3 actually has.
pub(super) fn tokens_col3(total: f64) -> Col3 {
    Col3 {
        tokens: TokensView::Data(TokensData {
            rate: vec![1.0, 2.0],
            total,
            by_worktree: WorktreeAxis {
                rows: vec![WorktreeSlice {
                    name: Some("main".to_owned()),
                    tokens: 7_500,
                }],
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
        ..Col3::default()
    }
}

pub(super) fn share_fixture(overall: Option<f64>) -> SavingsData {
    SavingsData {
        target_pct: Some(90),
        usd_saved_micros: None,
        usd_label: None,
        usd_rows: Vec::new(),
        usd_read_micros: None,
        usd_read_label: None,
        by_worktree: WorktreeAxis::default(),
        share: Box::new(ShareData {
            window: None,
            overall,
            honesty: 9_803,
            avoided_tokens: 10_000,
            read_tokens: 9_000,
            sessions_with_reads: 3,
            reads_unmeasured: 0,
            by_family: Some(10.0),
            excluding: Some(5.3),
            excluded: vec!["codex".to_owned()],
            families: vec![
                FamilyShareRow {
                    family: "claude-code".to_owned(),
                    share: Some(5.3),
                    honesty: 340,
                    sessions_with_reads: 3,
                    decided_without_reads: false,
                },
                FamilyShareRow {
                    family: "codex".to_owned(),
                    share: Some(100.0),
                    honesty: 500,
                    sessions_with_reads: 0,
                    decided_without_reads: true,
                },
            ],
        }),
    }
}
