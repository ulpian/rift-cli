//! The savings window's boundary: where the figure in [`crate::savings`]
//! starts counting from.
//!
//! The savings share is `avoided ÷ (avoided + read)` **since the reset**. Until
//! now "the reset" could only mean the moment the projections themselves were
//! emptied, which is a thing a person does once and cannot do again without
//! throwing away the record. An evaluation period needs a boundary that can be
//! moved without destroying anything, so this module adds one:
//!
//! * `<common_dir>/rift-family/savings-window.json` — one small file beside
//!   the family manifest, holding the boundary, who set it and when. Written
//!   temp-file-then-rename, so a reader sees the old boundary or the new one
//!   and never half of either.
//! * absent → the window is the whole record, exactly as before. **Absence is
//!   the wide window, never a narrow one**: a marker that cannot be read leaves
//!   more reads in the denominator, and the direction of any doubt here has to
//!   be the one that lowers the published share rather than the one that
//!   flatters it.
//!
//! It is a *file*, in the directory every member of the family already shares,
//! for the reason every other family cache is: a linked worktree is a member of
//! its repository, and a boundary keyed by a checkout path would give a
//! subagent's temporary worktree a different evaluation period from the one the
//! person set. It is deliberately **not** a row: the canonical tables are
//! append-only records of what happened, and where a reader chooses to start
//! reading them is not something that happened.
//!
//! Nothing here opens a store, and nothing here is reached from a hook: the
//! boundary is read by the three surfaces that publish the share and written by
//! one verb a person types.

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// The family's own directory inside the git common dir — the same one the
/// family manifest and the document sidecars live in.
const FAMILY_DIR: &str = "rift-family";

/// The marker's file name.
const MARKER_FILE: &str = "savings-window.json";

/// The verb that writes the marker, recorded in it so a reader of the file
/// knows what put it there.
pub const RESET_VERB: &str = "rift savings reset";

/// The marker's shape version. Read strictly: a marker this binary does not
/// understand is treated as absent, which widens the window rather than
/// narrowing it.
const SCHEMA_VERSION: u32 = 1;

/// The stored boundary marker.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    /// The marker's shape version.
    pub schema_version: u32,
    /// The boundary itself, in epoch milliseconds: rows recorded before this
    /// instant are outside the window.
    pub boundary_ms: i64,
    /// The same instant spelled for a person reading the file.
    pub boundary: String,
    /// When the marker was written.
    pub set_at_ms: i64,
    /// The same instant, spelled.
    pub set_at: String,
    /// The verb that wrote it ([`RESET_VERB`]).
    pub set_by: String,
}

/// Where a window's boundary came from — the sentence every surface prints
/// beside the figure, as a value rather than as prose each caller invents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// No marker and no override: every recorded row counts, which is what
    /// "since the reset" meant before a marker could exist.
    SinceReset,
    /// The stored marker.
    Marker(Box<Marker>),
    /// A `--since` override, for one call, writing nothing.
    Since(i64),
    /// A **trailing** window (`rift savings --last 7d`, the dashboard's period
    /// buttons) composed with the family's marker, for one call, writing
    /// nothing.
    Trailing {
        /// The cut in force: the later of the trailing window's own start and
        /// the marker's boundary.
        cut_ms: i64,
        /// How the caller spelled the window (`24h`, `7d`, `30d`).
        spelling: &'static str,
        /// `true` when the marker is what [`Self::Trailing::cut_ms`] is —
        /// the window asked for reaches further back than the evaluation
        /// period does, so the period wins and the label says so.
        from_marker: bool,
    },
}

impl Boundary {
    /// The cut, in epoch milliseconds, or `None` for the whole record.
    ///
    /// This is the only value the computation takes: everything else here is
    /// how the cut is explained.
    #[must_use]
    pub fn cut_ms(&self) -> Option<i64> {
        match self {
            Self::SinceReset => None,
            Self::Marker(m) => Some(m.boundary_ms),
            Self::Since(ms) | Self::Trailing { cut_ms: ms, .. } => Some(*ms),
        }
    }

    /// The sentence a surface prints to say which boundary it used.
    ///
    /// It reads as a `since …` clause because that is how every surface uses
    /// it — "the share the plane avoided, `<label>`" — and a boundary that is
    /// only ever printed one way cannot be printed inconsistently.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::SinceReset => {
                "since the projection reset — every recorded row counts (no boundary marker)"
                    .to_owned()
            }
            Self::Marker(m) => format!(
                "since the boundary marker at {}, set {} by `{}`",
                m.boundary, m.set_at, m.set_by
            ),
            Self::Since(ms) => format!(
                "since {} — `--since`, this call only; nothing was written",
                format_rfc3339_ms(*ms)
            ),
            Self::Trailing {
                cut_ms,
                spelling,
                from_marker,
            } => {
                if *from_marker {
                    format!(
                        "since {} — this family's boundary marker, which starts later \
                         than the trailing {spelling} window asked for",
                        format_rfc3339_ms(*cut_ms)
                    )
                } else {
                    format!(
                        "since {} — the trailing {spelling} window, this call only; \
                         nothing was written",
                        format_rfc3339_ms(*cut_ms)
                    )
                }
            }
        }
    }

    /// The same clause, short enough for a fixed-width panel.
    ///
    /// Same facts, fewer of them: which instant, and whether it was set or
    /// asked for. A surface that cannot fit [`Self::label`] prints this and
    /// never an abbreviation of its own.
    #[must_use]
    pub fn short_label(&self) -> String {
        match self {
            Self::SinceReset => "since the projection was reset".to_owned(),
            Self::Marker(m) => format!("since the window was reset {}", m.boundary),
            Self::Since(ms) => format!("since {} (--since)", format_rfc3339_ms(*ms)),
            Self::Trailing {
                cut_ms,
                spelling,
                from_marker,
            } => {
                if *from_marker {
                    format!(
                        "since the window was reset {} (inside {spelling})",
                        format_rfc3339_ms(*cut_ms)
                    )
                } else {
                    format!("since {} ({spelling})", format_rfc3339_ms(*cut_ms))
                }
            }
        }
    }

    /// Whether this boundary narrows the record at all.
    #[must_use]
    pub fn is_cut(&self) -> bool {
        self.cut_ms().is_some()
    }
}

/// Where this family keeps its marker.
#[must_use]
pub fn marker_path(common_dir: &Path) -> PathBuf {
    common_dir.join(FAMILY_DIR).join(MARKER_FILE)
}

/// The family's marker, or `None` when there is none this binary can read.
///
/// Unreadable, unparsable and unrecognised all answer `None` — the wide
/// window. A boundary is a claim about what to leave *out* of the denominator,
/// and a claim nobody can read is not one this tool will act on.
#[must_use]
pub fn read_marker(common_dir: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(marker_path(common_dir)).ok()?;
    let marker: Marker = serde_json::from_str(&text).ok()?;
    (marker.schema_version == SCHEMA_VERSION && marker.boundary_ms >= 0).then_some(marker)
}

/// The boundary in force for one call: an explicit `--since` first, then the
/// family's marker, then the whole record.
///
/// The override never writes and never persists — a person asking "what would
/// the figure be from Tuesday?" has not changed the evaluation period.
#[must_use]
pub fn resolve(common_dir: &Path, since_ms: Option<i64>) -> Boundary {
    if let Some(ms) = since_ms {
        return Boundary::Since(ms);
    }
    read_marker(common_dir).map_or(Boundary::SinceReset, |m| Boundary::Marker(Box::new(m)))
}

/// The boundary for a **trailing** window — the dashboard's `[24h] [7d] [30d]`
/// buttons and `rift savings --last` — composed with the family's marker.
///
/// `trailing_cut_ms` is `None` for "no window of its own" (the `all` button,
/// and `--last` not passed), which is exactly [`resolve`] with no override.
///
/// Otherwise the two are **composed rather than one replacing the other**, and
/// the composition is the *later* of the two instants. Both are exclusions —
/// the marker says which rows are outside the evaluation period, the window
/// says which are too old to be interesting — and composing two exclusions
/// gives the tighter one. The alternative, letting a button reach back past the
/// boundary somebody set, would publish rows on the card that `rift savings`
/// refuses to count, which is the same figure disagreeing with itself on two
/// surfaces.
///
/// `--since` is deliberately NOT this: it is a stated instant, for a person
/// asking what the figure was from a moment they name, and it answers exactly
/// that. A trailing window names no instant — it names a length — so there is
/// nothing in it for a marker to contradict.
#[must_use]
pub fn resolve_trailing(
    common_dir: &Path,
    trailing_cut_ms: Option<i64>,
    spelling: &'static str,
) -> Boundary {
    let Some(trailing) = trailing_cut_ms else {
        return resolve(common_dir, None);
    };
    let marker = read_marker(common_dir).map(|m| m.boundary_ms);
    let from_marker = marker.is_some_and(|m| m > trailing);
    Boundary::Trailing {
        cut_ms: marker.map_or(trailing, |m| m.max(trailing)),
        spelling,
        from_marker,
    }
}

/// Move the family's boundary to `boundary_ms`, recording `now_ms` as when.
///
/// Idempotent in the way a marker can be: writing it again replaces it, and a
/// second reset with no argument moves the boundary forward to that moment.
/// Returns the marker written and the boundary it replaced.
///
/// # Errors
/// [`RiftError::Validity`] for a boundary in the future — a window that starts
/// after now would exclude the very sessions it was opened to measure, and
/// silently report `n/a` for as long as the clock took to catch up.
/// [`RiftError::Io`] if the family directory cannot be written.
pub fn write_marker(
    common_dir: &Path,
    boundary_ms: i64,
    now_ms: i64,
) -> Result<(Marker, Option<i64>)> {
    if boundary_ms > now_ms {
        return Err(RiftError::Validity(format!(
            "a savings window cannot start in the future: {} is after {}",
            format_rfc3339_ms(boundary_ms),
            format_rfc3339_ms(now_ms)
        )));
    }
    if boundary_ms < 0 {
        return Err(RiftError::Validity(format!(
            "a savings window cannot start before the epoch: {boundary_ms} ms"
        )));
    }
    let previous = read_marker(common_dir).map(|m| m.boundary_ms);
    let marker = Marker {
        schema_version: SCHEMA_VERSION,
        boundary_ms,
        boundary: format_rfc3339_ms(boundary_ms),
        set_at_ms: now_ms,
        set_at: format_rfc3339_ms(now_ms),
        set_by: RESET_VERB.to_owned(),
    };
    let path = marker_path(common_dir);
    let dir = common_dir.join(FAMILY_DIR);
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(&marker)
        .map_err(|e| RiftError::Validity(format!("the marker could not be serialized: {e}")))?;
    // Named for the writing process, so two resets at the same instant cannot
    // tear each other's temp file; the rename is what a reader ever sees.
    let tmp = dir.join(format!("{MARKER_FILE}.tmp{}", std::process::id()));
    std::fs::write(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok((marker, previous))
}

/// Parse an instant a person typed: strict UTC RFC3339
/// (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) or plain epoch milliseconds.
///
/// Nothing else — no local time, no "yesterday", no offset arithmetic. A
/// boundary that a reader cannot spell back exactly is not one this tool will
/// take a published figure from.
#[must_use]
pub fn parse_instant(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Some(ms) = rift_recall_substrate::parse_rfc3339_ms(text) {
        return Some(ms);
    }
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        return text.parse::<i64>().ok();
    }
    None
}

/// Render a non-negative epoch-ms instant as `YYYY-MM-DDTHH:MM:SS.mmmZ` — the
/// exact subset [`parse_instant`] reads back, so `parse(format(ms)) == ms`.
///
/// Civil-from-days is Howard Hinnant's algorithm over the proleptic Gregorian
/// calendar, the inverse of the parser's days-from-civil. It is spelled here
/// rather than borrowed because the crate that owns the capture-side spelling
/// sits above this one in the graph, and a boundary label must be renderable by
/// the crate that owns the boundary.
#[must_use]
pub fn format_rfc3339_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-07T18:00:00.000Z.
    const T: i64 = 1_788_804_000_000;

    #[test]
    fn an_absent_marker_is_the_whole_record() {
        let tmp = tempfile::tempdir().unwrap();
        let b = resolve(tmp.path(), None);
        assert_eq!(b, Boundary::SinceReset);
        assert_eq!(b.cut_ms(), None);
        assert!(!b.is_cut());
        assert!(b.label().contains("no boundary marker"));
        assert_eq!(b.short_label(), "since the projection was reset");
    }

    /// A trailing window and a marker COMPOSE, and the composition is the
    /// later instant — the tighter of two exclusions.
    #[test]
    fn a_trailing_window_is_composed_with_the_marker_and_never_reaches_past_it() {
        let tmp = tempfile::tempdir().unwrap();
        // No marker: the window is its own cut, and says which window it is.
        let plain = resolve_trailing(tmp.path(), Some(T), "24h");
        assert_eq!(plain.cut_ms(), Some(T));
        assert!(
            plain.short_label().contains("24h"),
            "{}",
            plain.short_label()
        );
        // `all` is the marker's business, exactly as before.
        assert_eq!(
            resolve_trailing(tmp.path(), None, "all"),
            Boundary::SinceReset
        );

        write_marker(tmp.path(), T, T + 5).unwrap();
        // A window that starts BEFORE the boundary is cut at the boundary: the
        // card can never show a row `rift savings` refuses to count.
        let older = resolve_trailing(tmp.path(), Some(T - 90_000_000), "30d");
        assert_eq!(older.cut_ms(), Some(T));
        assert!(
            older.label().contains("boundary marker"),
            "the label says which of the two won: {}",
            older.label()
        );
        // A window that starts AFTER it keeps its own, narrower cut.
        let newer = resolve_trailing(tmp.path(), Some(T + 60_000), "24h");
        assert_eq!(newer.cut_ms(), Some(T + 60_000));
        assert!(newer.label().contains("trailing 24h"), "{}", newer.label());
        // `--since` is unchanged: a stated instant answers exactly itself.
        assert_eq!(resolve(tmp.path(), Some(T - 1)).cut_ms(), Some(T - 1));
    }

    #[test]
    fn a_marker_is_written_atomically_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let (written, previous) = write_marker(tmp.path(), T, T + 5).unwrap();
        assert_eq!(previous, None);
        assert_eq!(written.boundary_ms, T);
        assert_eq!(written.set_by, RESET_VERB);
        assert_eq!(read_marker(tmp.path()), Some(written.clone()));
        assert_eq!(resolve(tmp.path(), None).cut_ms(), Some(T));
        // Nothing is left behind by the write but the marker itself.
        let names: Vec<String> = std::fs::read_dir(tmp.path().join(FAMILY_DIR))
            .unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(names, vec![MARKER_FILE.to_owned()]);
    }

    #[test]
    fn a_second_reset_moves_the_boundary_and_names_the_one_it_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        write_marker(tmp.path(), T, T).unwrap();
        let (moved, previous) = write_marker(tmp.path(), T + 60_000, T + 60_000).unwrap();
        assert_eq!(previous, Some(T));
        assert_eq!(moved.boundary_ms, T + 60_000);
        assert_eq!(resolve(tmp.path(), None).cut_ms(), Some(T + 60_000));
    }

    #[test]
    fn a_future_boundary_is_refused_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let err = write_marker(tmp.path(), T + 1, T).unwrap_err();
        assert!(
            format!("{err}").contains("cannot start in the future"),
            "{err}"
        );
        assert!(!marker_path(tmp.path()).exists());
        assert_eq!(resolve(tmp.path(), None), Boundary::SinceReset);
    }

    #[test]
    fn an_override_takes_precedence_and_persists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        write_marker(tmp.path(), T, T).unwrap();
        let b = resolve(tmp.path(), Some(T + 1_000));
        assert_eq!(b.cut_ms(), Some(T + 1_000));
        assert!(b.label().contains("nothing was written"));
        assert!(
            b.short_label().starts_with("since 2026-"),
            "{}",
            b.short_label()
        );
        // The marker is untouched by a call that only overrode it.
        assert_eq!(read_marker(tmp.path()).map(|m| m.boundary_ms), Some(T));
    }

    /// A marker this binary cannot read widens the window rather than
    /// narrowing it: the conservative direction for a published share is the
    /// bigger denominator.
    #[test]
    fn an_unreadable_marker_is_the_whole_record() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(FAMILY_DIR)).unwrap();
        std::fs::write(marker_path(tmp.path()), "{not json").unwrap();
        assert_eq!(resolve(tmp.path(), None), Boundary::SinceReset);
        std::fs::write(
            marker_path(tmp.path()),
            serde_json::json!({
                "schema_version": 99, "boundary_ms": T, "boundary": "x",
                "set_at_ms": T, "set_at": "x", "set_by": RESET_VERB,
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(resolve(tmp.path(), None), Boundary::SinceReset);
    }

    #[test]
    fn an_instant_is_rfc3339_or_epoch_milliseconds_and_nothing_else() {
        assert_eq!(parse_instant("2026-09-07T18:00:00.000Z"), Some(T));
        assert_eq!(parse_instant("2026-09-07T18:00:00Z"), Some(T));
        assert_eq!(parse_instant(" 1788804000000 "), Some(T));
        assert_eq!(parse_instant("yesterday"), None);
        assert_eq!(parse_instant("2026-09-07"), None);
        assert_eq!(parse_instant("2026-09-07T18:00:00+01:00"), None);
        assert_eq!(parse_instant("-1"), None);
        assert_eq!(parse_instant(""), None);
    }

    #[test]
    fn the_rendered_instant_parses_back_to_itself() {
        for ms in [0, 1, T, T + 999, 1_000_000_000_000, 4_102_444_800_000] {
            let text = format_rfc3339_ms(ms);
            assert_eq!(parse_instant(&text), Some(ms), "{text}");
        }
        assert_eq!(format_rfc3339_ms(T), "2026-09-07T18:00:00.000Z");
    }
}
