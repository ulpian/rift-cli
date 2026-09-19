//! The Activity card: per-calendar-day counts of events, sessions and
//! checkpoints over the trailing thirty days, plus the remainder the strips
//! could not place.

use std::path::Path;

use rift_canopy::col3::{ActivityData, ActivityView, DayBar, OffStrip};
use rift_canopy::{FeedPayload, FeedUpdate};
use rift_store::window::CHECKPOINT_TIME_CTE;
use rift_store::{
    projection_db_for_family_under, CanonicalStore, SqliteStore, Value, CREATED_AT_UNKNOWN,
};

use super::project_family;
use crate::store::runtime;

/// One calendar day in milliseconds (UTC day boundaries, chosen deliberately:
/// deterministic and free of any timezone dependency).
const DAY_MS: i64 = 86_400_000;
/// The trailing window the activity card shows.
const ACTIVITY_DAYS: usize = 30;

/// Short weekday label for an epoch-day index (day 0 = 1970-01-01 = Thursday).
fn weekday_label(epoch_day: i64) -> &'static str {
    const LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    // Monday-indexed: epoch-day 0 (Thu) → 3.
    let idx = (epoch_day.rem_euclid(7) + 3).rem_euclid(7);
    LABELS
        .get(usize::try_from(idx).unwrap_or(0))
        .copied()
        .unwrap_or("?")
}

/// Compute the Activity feed: per-calendar-day counts of events, sessions, and
/// checkpoints over the trailing thirty days, from the project's own projection.
/// `now_ms` (the injected clock) defines "today" and the UTC day boundaries.
/// A **missing/unreadable projection** yields the placeholder; a readable one
/// with quiet days yields thirty zero-count days (a quiet month is a fact).
/// A readable projection additionally yields the [`OffStrip`] remainder — see
/// `read_day_counts`.
#[must_use]
pub fn compute_activity(root: &Path, path: &str, now_ms: i64) -> FeedUpdate {
    let payload = match read_day_counts(root, path, now_ms) {
        Some((days, off_strip)) => {
            FeedPayload::Activity(ActivityView::Days(ActivityData { days, off_strip }))
        }
        None => FeedPayload::Activity(ActivityView::NoProjection),
    };
    FeedUpdate {
        project: path.to_string(),
        payload,
    }
}

/// Read the trailing thirty days' capture counts **and the rows they do not
/// account for**, scoped to the projection. `None` when the projection is
/// absent/unreadable (the placeholder case).
///
/// # The strips are a filter, so they owe a remainder
///
/// Each strip is one grouped query with a `WHERE <time> >= window_start`
/// predicate, and a predicate drops what it does not match without saying so.
/// `checkpoint.created_at` carries an explicit unknown-time sentinel, which no
/// window predicate can ever match, so on the projection this repository
/// actually has, that query returns **nothing at all** for 103 checkpoints —
/// thirty zeros rendered as if the month had been quiet.
///
/// The remainder is therefore computed the only way that needs no knowledge of
/// how a time can be unreadable: `total rows − rows this strip placed`, per
/// category, from the same store. It is an identity, not a second reading of
/// the time column, so it stays true whatever the time column turns out to
/// hold — and it is why this function does **not** re-derive the sentinel rule
/// that `rift dispatch` owns. What it cannot do is say *why* a row is off the
/// strip; that needs the shared derivation named in the crate docs.
fn read_day_counts(root: &Path, path: &str, now_ms: i64) -> Option<(Vec<DayBar>, OffStrip)> {
    let db = projection_db_for_family_under(root, &project_family(Path::new(path))?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;
    let strip = StripRead {
        store: &store,
        rt: &rt,
        window: DayWindow::ending_at(now_ms),
    };

    let mut bars = strip.window.empty_bars();
    strip.place_by_time_column(&mut bars, "captured_event", "occurred_at_ms", &|b| {
        &mut b.events
    });
    strip.place_by_time_column(&mut bars, "session", "started_at", &|b| &mut b.sessions);
    strip.place_checkpoints(&mut bars);
    let off_strip = strip.off_strip(&bars);
    Some((bars, off_strip))
}

/// The thirty-day span the card draws, in whole UTC days.
struct DayWindow {
    /// The newest day on the strip: the day `now_ms` falls in.
    today: i64,
    /// The oldest day on the strip, inclusive.
    start_day: i64,
}

impl DayWindow {
    fn ending_at(now_ms: i64) -> Self {
        let today = now_ms.div_euclid(DAY_MS);
        let days_span = i64::try_from(ACTIVITY_DAYS).unwrap_or(7) - 1;
        Self {
            today,
            start_day: today - days_span,
        }
    }

    /// One zero-count bar per day, oldest first — the shape every strip writes
    /// into, so a category that places nothing still draws its quiet month.
    fn empty_bars(&self) -> Vec<DayBar> {
        (self.start_day..=self.today)
            .map(|day| DayBar {
                weekday: weekday_label(day),
                events: 0,
                sessions: 0,
                checkpoints: 0,
            })
            .collect()
    }

    /// The instant the oldest bar opens at.
    fn start_ms(&self) -> i64 {
        self.start_day.saturating_mul(DAY_MS)
    }

    /// Write one day's count into its bar. A day off the strip — older than
    /// the window, or in the future — is dropped here and accounted for by the
    /// remainder rather than folded into an edge bar, which would invent a
    /// spike.
    fn place(
        &self,
        bars: &mut [DayBar],
        day: i64,
        count: i64,
        pick: &dyn Fn(&mut DayBar) -> &mut u64,
    ) {
        if day < self.start_day || self.today < day {
            return;
        }
        let Some(bar) = usize::try_from(day - self.start_day)
            .ok()
            .and_then(|i| bars.get_mut(i))
        else {
            return;
        };
        *pick(bar) = u64::try_from(count).unwrap_or(0);
    }
}

/// One projection, read once for every strip on the card.
struct StripRead<'a> {
    store: &'a SqliteStore,
    rt: &'a tokio::runtime::Runtime,
    window: DayWindow,
}

impl StripRead<'_> {
    /// Place one table's rows by a plain epoch-millisecond column.
    ///
    /// The column name is this module's own constant; nothing a viewer types
    /// reaches the SQL.
    fn place_by_time_column(
        &self,
        bars: &mut [DayBar],
        table: &str,
        col: &str,
        pick: &dyn Fn(&mut DayBar) -> &mut u64,
    ) {
        let window_start = self.window.start_ms();
        let sql = format!(
            "SELECT {col} / {DAY_MS} AS day, COUNT(*) FROM {table} \
             WHERE {col} >= {window_start} GROUP BY day"
        );
        let Ok(rows) = self.rt.block_on(self.store.query(&sql, &[])) else {
            return;
        };
        for r in rows {
            if let (Ok(day), Ok(n)) = (r.get_i64(0), r.get_i64(1)) {
                self.window.place(bars, day, n, pick);
            }
        }
    }

    /// **The checkpoint bars do NOT use [`Self::place_by_time_column`], and
    /// that is the point.**
    ///
    /// `checkpoint.created_at` carries `CREATED_AT_UNKNOWN` (-1) whenever a
    /// shard records no time of its own — which is EVERY native shard.
    /// Bucketing that column directly, as this strip did until it moved onto
    /// the shared derivation, places -1 / 86_400_000 in the far past and
    /// therefore places nothing at all: on a repository of native checkpoints
    /// the strip's checkpoint row was structurally zero and the whole
    /// population fell into `OffStrip`, which reads as "older than the window"
    /// and was really "undated". It is the same defect that once made `rift
    /// dispatch` answer `checkpoints: 0` against 103 real checkpoints.
    ///
    /// The fix is not a second sentinel rule here. It is the ONE derivation,
    /// linked: [`rift_store::window::CHECKPOINT_TIME_CTE`] resolves each
    /// checkpoint to a recorded time, else to `MAX(session.started_at)` over
    /// the sessions it archives — the tightest lower bound stored facts
    /// support — else to NULL. A NULL is genuinely unplaceable and still
    /// becomes `OffStrip`, so the strip's silence now means what it says.
    fn place_checkpoints(&self, bars: &mut [DayBar]) {
        let window_start = self.window.start_ms();
        let sql = format!(
            "{CHECKPOINT_TIME_CTE}\
             SELECT ct.effective_at / {DAY_MS} AS day, COUNT(*) FROM ct \
             WHERE ct.effective_at IS NOT NULL AND ct.effective_at >= {window_start} \
             GROUP BY day"
        );
        let query = self.store.query(&sql, &[Value::Int(CREATED_AT_UNKNOWN)]);
        let Ok(rows) = self.rt.block_on(query) else {
            return;
        };
        for r in rows {
            if let (Ok(day), Ok(n)) = (r.get_i64(0), r.get_i64(1)) {
                self.window.place(bars, day, n, &|b| &mut b.checkpoints);
            }
        }
    }

    /// `total − placed`, per category.
    ///
    /// `count` is the table's own row count, so a category whose every row
    /// landed on a day yields exactly zero and the card is unchanged;
    /// saturation means a table that grew between the two reads reports
    /// "nothing missing" rather than an underflowed number.
    fn off_strip(&self, bars: &[DayBar]) -> OffStrip {
        let total = |table: &str| -> u64 {
            u64::try_from(self.rt.block_on(self.store.count(table)).unwrap_or(0)).unwrap_or(0)
        };
        let placed = |pick: &dyn Fn(&DayBar) -> u64| -> u64 {
            bars.iter().map(pick).fold(0u64, u64::saturating_add)
        };
        OffStrip {
            events: total("captured_event").saturating_sub(placed(&|b| b.events)),
            sessions: total("session").saturating_sub(placed(&|b| b.sessions)),
            checkpoints: total("checkpoint").saturating_sub(placed(&|b| b.checkpoints)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::fixtures::{make_project, open_proj, seed_event_for};

    /// Seed a `session` row started at `t`.
    fn seed_session(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, t: i64) {
        rt.block_on(store.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("claude-code"),
                Value::text("m"),
                Value::text("r"),
                Value::Int(t),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `checkpoint` created at `t`.
    fn seed_checkpoint(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, t: i64) {
        rt.block_on(store.insert(
            "checkpoint",
            &[
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "prev_hash",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("manual"),
                Value::Int(t),
                Value::text("root"),
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `captured_event` occurring at `t`.
    fn seed_event(store: &SqliteStore, rt: &tokio::runtime::Runtime, key: &str, t: i64) {
        seed_event_for(store, rt, key, "s", t);
    }
    /// `now` fixed at day 20_000 (2024-10-04), the 8th hour, so "today" is stable.
    const NOW: i64 = 20_000 * DAY_MS + 8 * 3_600_000;

    #[test]
    fn activity_missing_projection_is_the_placeholder_never_thirty_zeros() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("plain");
        std::fs::create_dir_all(&proj).unwrap();
        let update = compute_activity(root.path(), &proj.to_string_lossy(), NOW);
        assert_eq!(
            update.payload,
            FeedPayload::Activity(ActivityView::NoProjection),
            "no projection is the placeholder, not a zero month"
        );
    }

    #[test]
    fn activity_empty_projection_is_thirty_zero_days() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "quiet");
        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("a readable projection yields Days, even when quiet");
        };
        assert_eq!(d.days.len(), 30);
        assert!(d
            .days
            .iter()
            .all(|b| b.events == 0 && b.sessions == 0 && b.checkpoints == 0));
    }

    #[test]
    fn activity_buckets_by_calendar_day_within_the_window() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // Today (day 20_000): 2 events, 1 session, 1 checkpoint.
        seed_event(&store, &rt, "e1", 20_000 * DAY_MS + 1_000);
        seed_event(&store, &rt, "e2", 20_000 * DAY_MS + 2_000);
        seed_session(&store, &rt, "s-today", 20_000 * DAY_MS + 500);
        seed_checkpoint(&store, &rt, "k-today", 20_000 * DAY_MS + 3_000);
        // Yesterday (day 19_999): 1 event.
        seed_event(&store, &rt, "e3", 19_999 * DAY_MS + 100);
        // The oldest included day (29 days ago) remains visible.
        seed_event(&store, &rt, "e-start", 19_971 * DAY_MS + 100);
        // Thirty days ago is outside the fixed window.
        seed_event(&store, &rt, "e-old", 19_970 * DAY_MS + 100);
        // Future facts are never pulled into today's final bucket.
        seed_event(&store, &rt, "e-future", 20_001 * DAY_MS + 100);

        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("expected Days");
        };
        assert_eq!(d.days.len(), 30);
        let today = &d.days[29]; // oldest→newest, today is last
        assert_eq!((today.events, today.sessions, today.checkpoints), (2, 1, 1));
        let yesterday = &d.days[28];
        assert_eq!(yesterday.events, 1);
        assert_eq!(d.days[0].events, 1, "the inclusive oldest day is retained");
        // Outside/future events are dropped → total is exactly four.
        let total_events: u64 = d.days.iter().map(|b| b.events).sum();
        assert_eq!(total_events, 4, "outside and future events are dropped");
        // …and "dropped" is now said out loud: the two that fell outside the
        // window are the remainder, not a silence.
        assert_eq!(
            d.off_strip.events, 2,
            "e-old and e-future are accounted for"
        );
        assert_eq!(d.off_strip.sessions, 0);
        assert_eq!(d.off_strip.checkpoints, 0);
        // Weekday labels: day 20_000 is a Friday.
        assert_eq!(today.weekday, "Fri");
    }

    /// **The sentinel guard, and it now pins the RESIDUAL case.**
    ///
    /// `checkpoint.created_at` carries an explicit unknown-time sentinel.
    /// Measured on the projection for this repository (`4d9d222…`, 2026-08-18):
    /// 0 of 103 checkpoints placed beside 15,887 placed events, rendered as a
    /// quiet month.
    ///
    /// **These three checkpoints archive no session**, which is why they are
    /// still unplaceable after the strip moved onto
    /// `rift_store::window::CHECKPOINT_TIME_CTE`: the derivation resolves a
    /// sentinel row through `session_in_checkpoint`, and there is nothing here
    /// to resolve it to. That is the honest floor — a row no stored fact can
    /// place. The population the fix actually rescues is the one with sessions,
    /// pinned by
    /// `a_shard_that_records_no_time_is_placed_by_the_sessions_it_archives`
    /// below; keeping BOTH is what stops "the strip got busier" from hiding
    /// "the strip now guesses".
    ///
    /// What the strip may never do is stay silent about what it could not
    /// place, and this is the assertion that reddens if it does.
    #[test]
    fn checkpoints_whose_time_is_the_unknown_sentinel_are_counted_not_silently_dropped() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // The shape of the real projection: events and sessions carry real
        // times inside the window; every checkpoint carries the sentinel —
        // read from the crate that declares it, now that this crate can link
        // to it. It could not when this test was written, and the comment that
        // said so has been removed rather than left to age.
        const SENTINEL: i64 = CREATED_AT_UNKNOWN;
        seed_event(&store, &rt, "e1", 20_000 * DAY_MS + 1_000);
        seed_session(&store, &rt, "s1", 20_000 * DAY_MS + 500);
        for i in 0..3 {
            seed_checkpoint(&store, &rt, &format!("k{i}"), SENTINEL);
        }

        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("expected Days");
        };
        assert!(
            d.days.iter().all(|b| b.checkpoints == 0),
            "an undated checkpoint has no day to sit on"
        );
        assert_eq!(
            d.off_strip.checkpoints, 3,
            "…and every one of them is counted where the reader cannot miss it"
        );
        // The other two categories were placed, so the remainder is specific to
        // the column that lied and not a blanket "everything is missing".
        assert_eq!(d.days[29].events, 1);
        assert_eq!(d.days[29].sessions, 1);
        assert_eq!((d.off_strip.events, d.off_strip.sessions), (0, 0));
    }

    /// **The defect this crate carried until 2026-08-19, pinned from the side
    /// that can see it.**
    ///
    /// A native shard records no time of its own, so `created_at` is the
    /// sentinel — and the strip used to bucket that column directly, putting
    /// `-1 / 86_400_000` in 1969 and therefore placing NOTHING. On a repository
    /// of native checkpoints the checkpoint row was structurally zero and the
    /// legacy population fell into `OffStrip`, which a reader takes to mean
    /// "older than the window" when it really meant "undated".
    ///
    /// The strip now prefixes `rift_store::window::CHECKPOINT_TIME_CTE`, the
    /// same derivation `rift dispatch` uses, so a shard is placed at
    /// `MAX(session.started_at)` over the sessions it archives — the tightest
    /// lower bound stored facts support, and never a guess: the session cannot
    /// postdate the shard that covers it.
    ///
    /// Written as a SEPARATE test rather than by editing the one above, because
    /// the two cases are different facts: with a session it is placed, without
    /// one it is counted off-strip, and a single test asserting only the happy
    /// half would let the fix regress into placing everything at day zero.
    #[test]
    fn a_shard_that_records_no_time_is_placed_by_the_sessions_it_archives() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();

        // Two undated shards. `k-linked` archives a session that started
        // yesterday; `k-orphan` archives nothing at all.
        seed_session(&store, &rt, "s-yesterday", 19_999 * DAY_MS + 500);
        seed_checkpoint(&store, &rt, "k-linked", CREATED_AT_UNKNOWN);
        seed_checkpoint(&store, &rt, "k-orphan", CREATED_AT_UNKNOWN);
        rt.block_on(store.insert(
            "session_in_checkpoint",
            &["checkpoint_id", "session_id", "ordinal", "schema_version"],
            &[
                Value::text("k-linked"),
                Value::text("s-yesterday"),
                Value::Int(0),
                Value::Int(1),
            ],
        ))
        .unwrap();

        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("expected Days");
        };
        assert_eq!(
            d.days[28].checkpoints, 1,
            "the linked shard sits on its session's day, not on no day at all"
        );
        assert_eq!(d.days[29].checkpoints, 0, "and not on today");
        assert_eq!(
            d.off_strip.checkpoints, 1,
            "the orphan is still unplaceable, and is still counted rather than hidden"
        );
    }

    #[test]
    fn a_fully_placed_month_leaves_nothing_off_the_strip() {
        // The other half of the guard: the remainder is zero when the strips
        // account for everything, so the card is unchanged where nothing is
        // wrong. Without this, "always report 3 missing" would pass the test
        // above.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_event(&store, &rt, "e1", 20_000 * DAY_MS + 1_000);
        seed_session(&store, &rt, "s1", 19_990 * DAY_MS + 500);
        seed_checkpoint(&store, &rt, "k1", 19_971 * DAY_MS + 3_000);

        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("expected Days");
        };
        assert_eq!(d.off_strip, OffStrip::default());
        assert!(!d.off_strip.any());
    }

    #[test]
    fn activity_bucketing_is_stable_around_the_day_boundary() {
        // A nudge within the same day never re-buckets (day-bucketing is total
        // and stable under intra-day clock movement).
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_event(&store, &rt, "e1", 20_000 * DAY_MS + 12 * 3_600_000);
        let a = compute_activity(root.path(), &proj, 20_000 * DAY_MS + 1);
        let b = compute_activity(root.path(), &proj, 20_000 * DAY_MS + DAY_MS - 1);
        assert_eq!(a, b, "any 'now' within the same day yields the same window");
        // Crossing into the next day shifts the window by one.
        let c = compute_activity(root.path(), &proj, 20_001 * DAY_MS + 1);
        assert_ne!(a, c, "a new calendar day shifts the trailing window");
    }

    #[test]
    fn weekday_labels_cover_the_week() {
        // Day 0 (1970-01-01) is a Thursday.
        assert_eq!(weekday_label(0), "Thu");
        assert_eq!(weekday_label(3), "Sun"); // 1970-01-04
        assert_eq!(weekday_label(20_000), "Fri");
        // Negative epoch-days (pre-1970) never panic.
        assert!(!weekday_label(-5).is_empty());
    }
    /// Weekday labels are Monday-indexed off an epoch-day count whose day 0 is
    /// a Thursday, and negative days (pre-1970) wrap the same way rather than
    /// falling off the table.
    #[test]
    fn weekday_labels_are_monday_indexed_from_a_thursday_epoch() {
        assert_eq!(weekday_label(0), "Thu");
        for (day, expected) in [
            (1, "Fri"),
            (2, "Sat"),
            (3, "Sun"),
            (4, "Mon"),
            (5, "Tue"),
            (6, "Wed"),
            (7, "Thu"),
        ] {
            assert_eq!(weekday_label(day), expected, "day {day}");
        }
        assert_eq!(weekday_label(-1), "Wed");
        assert_eq!(weekday_label(-7), "Thu");
    }
}
