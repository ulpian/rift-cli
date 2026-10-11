//! The Recall card's window: the period button composed with the family's
//! boundary marker, resolved ONCE and handed to every read on the card.
//!
//! A separate module because the composition is the card's one arithmetic:
//! the headline, the two bars, the money and the serve count all take the cut
//! this module returns, and none of them may resolve a period of its own.

use std::path::Path;

use rift_canopy::col3::{RecallWindow, WindowCut};
use rift_store::Value;

use super::project_family;

/// The Recall card's window, resolved **once** for every figure on it.
///
/// One resolution is the whole point. The card draws a headline, two bars, a
/// dollar estimate, a serve count and a modal, and each of them used to reach
/// for the window in its own way: the audit sum took the button's cut, the
/// share took the family's boundary marker, the money took neither and the
/// serve count took nothing at all. On a repository carrying a marker the card
/// therefore showed one period's tokens beside another period's dollars. This
/// type exists so that cannot be spelled — it is resolved at the top of
/// [`super::compute_recall`] and handed to every read under it.
pub(super) enum CardWindow {
    /// The cut every query takes (`None` = the family's whole record), and the
    /// sentence the card prints for it.
    Resolved {
        /// The composed boundary, in epoch milliseconds.
        cut_ms: Option<i64>,
        /// `Boundary::short_label`, when the window narrows the record.
        label: Option<String>,
    },
    /// A **trailing** window this machine's clock could not place.
    ///
    /// The bus reads the wall clock and answers `0` when the platform refuses
    /// it, and `now − 24h` clamped at the epoch is the whole record — so the
    /// card would draw every row it holds under a button labelled `24h`. An
    /// unplaceable window is drawn as the empty window it is, and the modal
    /// says why.
    Unplaceable,
}

impl CardWindow {
    /// The cut to hand a query. An unplaceable window cuts everything out:
    /// absence is the honest answer, never the whole record under a narrow
    /// label.
    pub(super) fn cut_ms(&self) -> Option<i64> {
        match self {
            Self::Resolved { cut_ms, .. } => *cut_ms,
            Self::Unplaceable => Some(i64::MAX),
        }
    }

    /// The sentence the card and its modal print beside the figures.
    ///
    /// An unplaceable window has no sentence because it has no figures: every
    /// read under it answers empty, and the card draws its empty-window state
    /// rather than a period it cannot name.
    pub(super) fn label(&self) -> Option<String> {
        match self {
            Self::Resolved { label, .. } => label.clone(),
            Self::Unplaceable => None,
        }
    }
}

/// Resolve the card's window: the button's trailing cut composed with the
/// family's boundary marker, through the same resolver `rift savings --last`
/// calls rather than around it.
///
/// Composition, not replacement, and the composed cut is the **later** of the
/// two. A marker says which rows are outside the evaluation period; a trailing
/// window says which are too old to be interesting. Both are exclusions, and
/// two exclusions compose to the tighter one — so no button can reach back
/// past a boundary somebody set, and `all` still means exactly what the marker
/// has always meant.
pub(super) fn resolve_card_window(path: &str, now_ms: i64, window: RecallWindow) -> CardWindow {
    let trailing = match window.cut(now_ms) {
        WindowCut::NoClock => return CardWindow::Unplaceable,
        WindowCut::Marker => None,
        WindowCut::Since(ms) => Some(ms),
    };
    let Some(family) = project_family(Path::new(path)) else {
        // No family, no marker to compose with — and no stores to read under
        // one either, so this card is about to resolve to the placeholder.
        return CardWindow::Resolved {
            cut_ms: trailing,
            label: None,
        };
    };
    let boundary = rift_recall_ledger::savings_window::resolve_trailing(
        &family.common_dir,
        trailing,
        window.label(),
    );
    CardWindow::Resolved {
        cut_ms: boundary.cut_ms(),
        label: boundary.is_cut().then(|| boundary.short_label()),
    }
}
/// The window clause for one capture-projection column, and its parameter —
/// the shape `savings::window` uses on the serving side, spelled once here for
/// the two model-mix queries so the money is cut exactly where the tokens it
/// prices were. `None` is deliberately no clause at all, never `>= 0`.
///
/// The column name is this module's own constant; nothing a viewer types
/// reaches the SQL.
pub(super) fn mix_window(column: &str, cut_ms: Option<i64>) -> (String, Vec<Value>) {
    cut_ms.map_or_else(
        || (String::new(), Vec::new()),
        |ms| (format!(" WHERE {column} >= ?1"), vec![Value::Int(ms)]),
    )
}

#[cfg(test)]
mod tests {
    use rift_canopy::col3::RecallView;
    use rift_canopy::FeedPayload;

    use super::*;
    use crate::feeds::compute_recall;
    use crate::feeds::fixtures::{make_project, seed_one_read, seed_recall_db, USD_NOW};

    /// The chosen period is a **cut on the audit relation**, and the same cut
    /// on both halves of the card.
    ///
    /// The rows are placed either side of a 24-hour boundary from `USD_NOW`,
    /// so each window's headline is arithmetic anyone can check by hand rather
    /// than whatever the fixture happened to contain.
    #[test]
    fn a_narrower_window_sums_only_the_rows_inside_it() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "windowed");
        let day = 24 * 60 * 60 * 1000;
        seed_recall_db(
            root.path(),
            &proj,
            // The fixture's odd events are pulls, outside the share: each
            // one here books nothing, so the headline is the even events'.
            &[
                (USD_NOW - 40 * day, 5_000), // outside every narrow window
                (USD_NOW - 40 * day, 0),
                (USD_NOW - 10 * day, 700), // inside 30d only
                (USD_NOW - 10 * day, 0),
                (USD_NOW - 3 * day, 200), // inside 30d and 7d
                (USD_NOW - 3 * day, 0),
                (USD_NOW - 60_000, 90), // inside all three
            ],
        );
        seed_one_read(root.path(), &proj, "claude-code", 9_000);
        let avoided = |window: RecallWindow| -> u64 {
            let FeedPayload::Recall { savings, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            let RecallView::Data(sd) = savings else {
                panic!("expected savings data for {window:?}");
            };
            // The headline, the axis and the priced rows are all this figure:
            // one window resolution reaches every one of them.
            assert_eq!(sd.by_worktree.family_only, sd.share.avoided_tokens);
            sd.share.avoided_tokens
        };
        assert_eq!(avoided(RecallWindow::Today), 90);
        assert_eq!(avoided(RecallWindow::Last7Days), 290);
        assert_eq!(avoided(RecallWindow::Last30Days), 990);
        assert_eq!(
            avoided(RecallWindow::AllTime),
            5_990,
            "all-time keeps the pre-window rows the boundary marker never cut"
        );
    }

    /// A window that excludes every served row is **quiet**, not absent.
    ///
    /// Absent is the enable-prompt — "recall off — open Project Settings" —
    /// and a repository that served yesterday has already been through that
    /// door. Naming it again because the viewer asked about the last hour
    /// would be the card telling an operator to fix something that is working.
    #[test]
    fn an_empty_window_over_a_live_ledger_is_quiet_not_disabled() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "stale");
        let day = 24 * 60 * 60 * 1000;
        seed_recall_db(root.path(), &proj, &[(USD_NOW - 40 * day, 4_000)]);
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::Today).payload
        else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Quiet);
        // …and the same ledger over the whole record still carries its data,
        // so "quiet" is a fact about the window and not about the store.
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        assert!(matches!(savings, RecallView::Data(_)));
    }

    /// The card's window sentence is the **resolved boundary's own**, naming
    /// the instant it cut at and the button that asked for it.
    ///
    /// One resolution, one sentence: the card used to print a phrase of its
    /// own ("since 24 hours ago") that could not say which instant it meant
    /// and could not say when a boundary marker had moved the cut somewhere
    /// else. No flag was typed here, so nothing may claim one was.
    #[test]
    fn the_share_block_names_the_boundary_it_actually_cut_at() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "labelled");
        seed_recall_db(root.path(), &proj, &[(USD_NOW - 60_000, 90)]);
        for (window, spelling) in [
            (RecallWindow::Today, Some("(24h)")),
            (RecallWindow::Last7Days, Some("(7d)")),
            (RecallWindow::Last30Days, Some("(30d)")),
            // No marker in this fixture, so all-time is the whole record and
            // says nothing — exactly what it said before the buttons existed.
            (RecallWindow::AllTime, None),
        ] {
            let FeedPayload::Recall { savings, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            let RecallView::Data(sd) = savings else {
                panic!("expected savings data for {window:?}");
            };
            let sentence = sd.share.window.clone();
            match spelling {
                Some(s) => {
                    let sentence = sentence.unwrap_or_default();
                    assert!(sentence.ends_with(s), "{window:?}: {sentence}");
                    assert!(sentence.starts_with("since 20"), "{window:?}: {sentence}");
                    assert!(
                        !sentence.contains("--since"),
                        "no flag was typed: {sentence}"
                    );
                }
                None => assert_eq!(sentence, None, "{window:?}"),
            }
        }
    }

    /// A period button never reaches back past the family's boundary marker.
    ///
    /// The two are exclusions and they compose to the tighter one, so `[30d]`
    /// on a repository whose window was reset an hour ago covers the hour —
    /// and says the marker is what cut it. Anything else would draw rows on
    /// the card that `rift savings` refuses to count.
    #[test]
    fn a_period_button_is_composed_with_the_boundary_marker() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "marked");
        let day = 24 * 60 * 60 * 1000;
        seed_recall_db(
            root.path(),
            &proj,
            // The middle event is a pull booking nothing: the fixture's odd
            // events are outside the share.
            &[
                (USD_NOW - 10 * day, 5_000), // before the marker
                (USD_NOW - 10 * day, 0),
                (USD_NOW - 60_000, 90), // after it
            ],
        );
        seed_one_read(root.path(), &proj, "claude-code", 9_000);
        let family = project_family(Path::new(&proj)).expect("family");
        rift_recall_ledger::savings_window::write_marker(
            &family.common_dir,
            USD_NOW - 2 * day,
            USD_NOW,
        )
        .expect("marker");

        let card = |window: RecallWindow| {
            let FeedPayload::Recall { savings, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            match savings {
                RecallView::Data(sd) => sd,
                other => panic!("expected savings data for {window:?}: {other:?}"),
            }
        };
        // 30d asked for more than the marker allows: the marker wins, and the
        // sentence says which of the two did.
        let wide = card(RecallWindow::Last30Days);
        assert_eq!(wide.share.avoided_tokens, 90);
        assert!(
            wide.share
                .window
                .as_deref()
                .unwrap_or_default()
                .contains("was reset"),
            "{:?}",
            wide.share.window
        );
        // 24h is narrower than the marker: its own cut stands.
        let narrow = card(RecallWindow::Today);
        assert_eq!(narrow.share.avoided_tokens, 90);
        assert!(narrow
            .share
            .window
            .as_deref()
            .unwrap_or_default()
            .ends_with("(24h)"));
        // `all` is the marker, as it always was.
        assert_eq!(card(RecallWindow::AllTime).share.avoided_tokens, 90);
    }

    /// The serve count beside the headline covers the card's period, and the
    /// chart draws nothing from outside it.
    #[test]
    fn the_serve_count_follows_the_card_window() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "counted");
        let day = 24 * 60 * 60 * 1000;
        seed_recall_db(
            root.path(),
            &proj,
            &[
                (USD_NOW - 10 * day, 100),
                (USD_NOW - 3 * day, 100),
                (USD_NOW - 60_000, 100),
            ],
        );
        let total = |window: RecallWindow| -> u64 {
            let FeedPayload::Recall { calls, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            match calls {
                RecallView::Data(cd) => cd.total,
                other => panic!("expected call data for {window:?}: {other:?}"),
            }
        };
        assert_eq!(total(RecallWindow::Today), 1);
        assert_eq!(total(RecallWindow::Last7Days), 2);
        assert_eq!(total(RecallWindow::AllTime), 3);
    }

    /// A trailing window a broken clock cannot place is an **empty window**,
    /// never the whole record under a narrow label.
    ///
    /// The bus answers `0` when the platform refuses the wall clock; the old
    /// arithmetic clamped `0 − 24h` to the epoch and the card drew every row
    /// it had under `[24h]`.
    #[test]
    fn an_unplaceable_window_draws_nothing_rather_than_everything() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "clockless");
        seed_recall_db(root.path(), &proj, &[(1_000, 5_000)]);
        seed_one_read(root.path(), &proj, "claude-code", 9_000);
        let FeedPayload::Recall { savings, calls } =
            compute_recall(root.path(), &proj, 0, RecallWindow::Today).payload
        else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Quiet, "no figures under a dead clock");
        match calls {
            RecallView::Data(cd) => assert_eq!(cd.total, 0),
            other => panic!("expected an empty call window: {other:?}"),
        }
        // The same clock with `all` selected still answers: that window needs
        // no clock at all.
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, 0, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        assert!(matches!(savings, RecallView::Data(_)));
    }
}
