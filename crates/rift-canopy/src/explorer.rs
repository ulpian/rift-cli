//! The destination explorer's interaction state (ADR-0052, PRD-M5.16).
//!
//! Like the profile menu and the colour picker, the explorer is a **mode**, not
//! an `Overlay`: it navigates, selects, and owns a key grammar, so its transient
//! state lives here — a browsed directory, the listed entries, a cursor, a
//! scroll offset, the dot-entry toggle, and any inline error — and its reducer
//! branch runs over synthetic keys like every other mode.
//!
//! Everything in this module is **pure**: `rift-canopy` performs no filesystem
//! I/O on the interactive path, so a listing arrives from outside through the
//! lister seam ([`crate::state::Effects::list_dir`]) as an infallible
//! [`DirListing`] and lands via [`ExplorerState::apply`]. The real walk lives in
//! `rift-canopy-data::dirlist` (ADR-0009); tests inject hand-built listings.
//!
//! Entry names are display forms: a path is rebuilt as `cwd.join(name)`. Names
//! reach us through `to_string_lossy`, so a non-UTF8 filename renders lossily
//! and its rebuilt path may not round-trip on Linux (APFS enforces UTF-8, so the
//! primary platform is unaffected). The escape hatch, if it ever matters, is a
//! per-entry `PathBuf` beside `name` — cheap under [`MAX_ENTRIES`] — deliberately
//! not built today (ADR-0052 Consequences).

use std::path::PathBuf;

/// One listed directory entry, in display form. The full path is rebuilt as
/// `cwd.join(name)` — see the module docs on the non-UTF8 escape hatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerEntry {
    /// The entry's file name (lossy display form).
    pub name: String,
    /// Whether it is a directory — a symlink whose target is one counts
    /// (classification only; the store re-canonicalizes at registration).
    pub is_dir: bool,
}

impl ExplorerEntry {
    /// A dot-entry (hidden unless [`ExplorerState::show_hidden`]).
    #[must_use]
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// The lister seam's reply for one directory (ADR-0052 §3) — **infallible by
/// construction**: a failure is folded into `error`, never thrown, so a
/// permissions problem can never eject the operator from the flow (the
/// `rift_git::list_worktrees` degradation discipline).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirListing {
    /// The directory that was listed.
    pub path: PathBuf,
    /// Its immediate children, sorted by [`sort_entries`], capped at
    /// [`MAX_ENTRIES`]. Dot-entries are present here — filtering is a read-time
    /// concern of the state, never the walker's.
    pub entries: Vec<ExplorerEntry>,
    /// How many entries the cap dropped (`0` when the listing is complete).
    pub omitted: usize,
    /// Why the listing failed, if it did — the entries are then empty and the
    /// state stays put.
    pub error: Option<String>,
}

/// The most entries one listing carries (ADR-0052 §3). One source: the real
/// walker in `rift-canopy-data` imports this constant, so the cap and the
/// truncation disclosure can never disagree.
pub const MAX_ENTRIES: usize = 500;

/// The destination explorer's transient state — only meaningful while
/// `mode == crate::input::Mode::Explorer`; resting (`Default`) otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExplorerState {
    /// The directory currently open — the one the title names and
    /// `[ select destination ]` registers (ADR-0052 §5).
    pub cwd: PathBuf,
    /// The full capped listing of `cwd` (dot-entries included).
    pub entries: Vec<ExplorerEntry>,
    /// The highlighted row, an index into [`ExplorerState::visible`].
    pub cursor: usize,
    /// The list viewport's top row (the first mode-modal to scroll).
    pub scroll: usize,
    /// Whether dot-entries are shown (the `.` toggle).
    pub show_hidden: bool,
    /// How many entries the cap dropped from this listing.
    pub omitted: usize,
    /// The inline error from the last failed listing, cleared by the next good
    /// one.
    pub error: Option<String>,
}

impl ExplorerState {
    /// Reset to the resting state (entering the explorer fresh, or leaving it).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The rows on screen: the listing with the dot-filter applied at **read**
    /// time (the `ProfileMenuState::candidates` compute-never-store pattern), so
    /// the `.` toggle is instant and never loses an entry.
    #[must_use]
    pub fn visible(&self) -> Vec<&ExplorerEntry> {
        self.entries
            .iter()
            .filter(|e| self.show_hidden || !e.is_hidden())
            .collect()
    }

    /// How many rows are on screen (the [`ExplorerState::visible`] length,
    /// without building the vector).
    #[must_use]
    pub fn visible_len(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| self.show_hidden || !e.is_hidden())
            .count()
    }

    /// The visible entry under the cursor, or `None` on an empty list.
    #[must_use]
    pub fn cursor_entry(&self) -> Option<&ExplorerEntry> {
        self.entries
            .iter()
            .filter(|e| self.show_hidden || !e.is_hidden())
            .nth(self.cursor)
    }

    /// The open directory as a display string (lossy — see the module docs).
    #[must_use]
    pub fn display_cwd(&self) -> String {
        self.cwd.to_string_lossy().into_owned()
    }

    /// Land one listing from the seam.
    ///
    /// A **successful** listing swaps the browsed directory whole and resets the
    /// cursor, the scroll, and any inline error. A **failed** one touches only
    /// `error` — cwd, entries, cursor, and scroll survive, so an unreadable
    /// folder leaves the operator exactly where they were (ADR-0052 §3).
    pub fn apply(&mut self, listing: DirListing) {
        if let Some(error) = listing.error {
            self.error = Some(error);
            return;
        }
        self.cwd = listing.path;
        self.entries = listing.entries;
        self.omitted = listing.omitted;
        self.cursor = 0;
        self.scroll = 0;
        self.error = None;
    }
}

/// **The** listing comparator (ADR-0052 §3): directories first, then
/// case-insensitive by name, with the exact name as the tie-break so the order
/// is total and deterministic. Exported so the real walker sorts with the same
/// rule the state expects — the two sides of the seam cannot drift.
pub fn sort_entries(entries: &mut [ExplorerEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> ExplorerEntry {
        ExplorerEntry {
            name: name.to_owned(),
            is_dir,
        }
    }

    fn listing(path: &str, entries: Vec<ExplorerEntry>) -> DirListing {
        DirListing {
            path: PathBuf::from(path),
            entries,
            omitted: 0,
            error: None,
        }
    }

    #[test]
    fn sort_puts_directories_first_then_case_insensitive_names() {
        let mut v = vec![
            entry("zebra.txt", false),
            entry("Beta", true),
            entry("alpha", true),
            entry("Apple.md", false),
        ];
        sort_entries(&mut v);
        assert_eq!(
            v.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "Beta", "Apple.md", "zebra.txt"],
            "dirs first, then case-insensitive by name"
        );
    }

    #[test]
    fn sort_tie_breaks_on_the_exact_name_deterministically() {
        // Case-folded equals: the exact name orders them, and the result is
        // identical whichever order they arrive in.
        let mut a = vec![entry("README", false), entry("readme", false)];
        let mut b = vec![entry("readme", false), entry("README", false)];
        sort_entries(&mut a);
        sort_entries(&mut b);
        assert_eq!(a, b, "the comparator is deterministic under permutation");
        assert_eq!(a[0].name, "README", "the exact name breaks the tie");
    }

    #[test]
    fn visible_filters_dot_entries_without_dropping_them() {
        let mut st = ExplorerState::default();
        st.apply(listing(
            "/r",
            vec![
                entry(".git", true),
                entry("src", true),
                entry(".env", false),
            ],
        ));
        assert_eq!(st.visible_len(), 1);
        assert_eq!(st.visible().first().map(|e| e.name.as_str()), Some("src"));
        st.show_hidden = true;
        assert_eq!(st.visible_len(), 3, "the toggle reveals, it never re-reads");
        assert_eq!(
            st.entries.len(),
            3,
            "hidden entries were never dropped from state"
        );
    }

    #[test]
    fn cursor_entry_follows_the_visible_list() {
        let mut st = ExplorerState::default();
        st.apply(listing(
            "/r",
            vec![entry(".hidden", true), entry("a", true), entry("b", true)],
        ));
        assert_eq!(st.cursor_entry().map(|e| e.name.as_str()), Some("a"));
        st.cursor = 1;
        assert_eq!(st.cursor_entry().map(|e| e.name.as_str()), Some("b"));
        st.cursor = 9;
        assert!(
            st.cursor_entry().is_none(),
            "past the end is None, not panic"
        );
        // With hidden shown the same index names a different row.
        st.show_hidden = true;
        st.cursor = 0;
        assert_eq!(st.cursor_entry().map(|e| e.name.as_str()), Some(".hidden"));
    }

    #[test]
    fn apply_success_swaps_the_directory_and_resets_the_view() {
        let mut st = ExplorerState::default();
        st.apply(DirListing {
            omitted: 3,
            ..listing("/r", vec![entry("a", true), entry("b", true)])
        });
        st.cursor = 1;
        st.scroll = 1;
        st.error = Some("stale".into());
        st.apply(listing("/r/a", vec![entry("deep", true)]));
        assert_eq!(st.cwd, PathBuf::from("/r/a"));
        assert_eq!(st.entries.len(), 1);
        assert_eq!(st.cursor, 0, "a new directory starts at the top");
        assert_eq!(st.scroll, 0);
        assert_eq!(st.omitted, 0, "the new listing's own count");
        assert!(st.error.is_none(), "a good listing clears the error line");
    }

    #[test]
    fn apply_failure_sets_only_the_error_and_stays_put() {
        let mut st = ExplorerState::default();
        st.apply(DirListing {
            omitted: 2,
            ..listing("/r", vec![entry("a", true), entry("b", true)])
        });
        st.cursor = 1;
        st.scroll = 1;
        let before = st.clone();
        st.apply(DirListing {
            path: PathBuf::from("/nope"),
            entries: vec![],
            omitted: 0,
            error: Some("permission denied".into()),
        });
        assert_eq!(st.error.as_deref(), Some("permission denied"));
        assert_eq!(st.cwd, before.cwd, "the view stays where it was");
        assert_eq!(st.entries, before.entries);
        assert_eq!(st.cursor, before.cursor);
        assert_eq!(st.scroll, before.scroll);
        assert_eq!(st.omitted, before.omitted);
        // …and the next good listing clears it.
        st.apply(listing("/r/a", vec![]));
        assert!(st.error.is_none());
    }

    #[test]
    fn omitted_carries_through_and_reset_restores_default() {
        let mut st = ExplorerState::default();
        st.apply(DirListing {
            omitted: 3,
            ..listing("/r", vec![entry("a", true)])
        });
        assert_eq!(st.omitted, 3);
        st.show_hidden = true;
        st.reset();
        assert_eq!(st, ExplorerState::default(), "reset is the resting state");
    }

    #[test]
    fn display_cwd_renders_the_open_directory() {
        let mut st = ExplorerState::default();
        st.apply(listing("/home/dev/code", vec![]));
        assert_eq!(st.display_cwd(), "/home/dev/code");
    }
}
