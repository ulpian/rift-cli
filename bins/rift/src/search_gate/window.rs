//! **What one context window has already been decided for.**
//!
//! One directory of marker files per conversation, and the collection of what
//! ended conversations left behind. Its own module because both levers decide
//! once per window by exactly this mechanism, and because the re-query rate is
//! defined by these file names and by nothing else.

use std::path::{Path, PathBuf};

use super::answer::ANSWERS_DIR;

/// The marker prefix for a name search this window has answered.
#[cfg(feature = "search")]
pub(super) const ANSWER_PREFIX: &str = "g-";

/// The marker prefix for a search this window bounded.
pub(super) const CAP_PREFIX: &str = "c-";

/// The marker prefix for a bounded search asked again in the turn it was
/// bounded in: the re-query rate's own definition, which is `q-` over `c-`.
pub(super) const REQUERY_PREFIX: &str = "q-";

/// The marker prefix for an answered search asked again in the turn it was
/// answered in: the escape hatch taken immediately, counted apart from the
/// bound's re-queries so neither figure is the other's numerator.
#[cfg(feature = "search")]
pub(super) const REPEAT_PREFIX: &str = "r-";

// ----------------------------------------- one context window's decisions ---

/// The directory a window's search decisions are remembered in, under this
/// member's own run state.
const WINDOW_DIR: &str = "w";

/// The marker that says an amendment this window made was displaced, so this
/// window has stopped making them.
const DROPPED_MARKER: &str = "dropped";

/// What one **context window** has already been given a bound for.
///
/// Marker files whose *existence* is the fact, created with `create_new` so
/// that the process which created one is the one that acted: a client may run
/// its hooks in parallel, and a read-modify-write of a shared record would let
/// two hooks both conclude they were first — which here means capping a search
/// twice and taking away the escape hatch that makes the cap safe.
///
/// Per member, under `.rift/state/`, keyed by nothing about the checkout: a
/// window is a conversation, a conversation happens in one checkout, and
/// losing the directory costs one extra cap and can never cost an answer.
pub(super) struct SearchWindow {
    dir: PathBuf,
    /// The window's own identity, which is also what its window journal is
    /// named by: one conversation, one key, wherever this member writes about
    /// it.
    #[cfg(feature = "search")]
    key: String,
}

/// What a window says about a search it is being asked to bound.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Claim {
    /// This window has not bounded this search before, and this call is the
    /// one that claimed it.
    Fresh,
    /// It has, in the turn named — so this call runs unbounded.
    Repeat {
        /// The turn the cap was made in, which is what makes a repeat inside
        /// that turn a re-query rather than ordinary later work.
        capped_in_turn: u32,
    },
}

impl SearchWindow {
    /// The window a hook belongs to, as this member sees it.
    pub(super) fn at(work_tree: &Path, session_id: &str, agent_id: Option<&str>) -> Self {
        let key = crate::window_journal::key(session_id, agent_id);
        Self {
            dir: work_tree
                .join(".rift/state/search")
                .join(WINDOW_DIR)
                .join(&key),
            #[cfg(feature = "search")]
            key,
        }
    }

    /// The window journal this window writes: the same key, in the directory
    /// the search crate names for it.
    #[cfg(feature = "search")]
    pub(super) fn reads(&self, work_tree: &Path) -> PathBuf {
        rift_recall_search::reads_dir(work_tree).join(format!("{}.jsonl", self.key))
    }

    /// Claim the one decision this window may make for `key`.
    ///
    /// `prefix` is which kind of decision — a search bounded, or a name search
    /// answered. The two are one mechanism because they are one rule: a
    /// decision about a search is made once per window and the repeat runs.
    pub(super) fn claim(&self, prefix: &str, key: &str, turn: u32) -> Claim {
        if self.create(&format!("{prefix}{key}"), turn.to_string().as_bytes()) {
            return Claim::Fresh;
        }
        // Some earlier call in this window decided this search. Which turn it
        // did so in is the only thing left to learn, and a body this module
        // did not write reads as a turn nobody is in.
        let capped_in_turn = std::fs::read_to_string(self.dir.join(format!("{prefix}{key}")))
            .ok()
            .and_then(|body| body.trim().parse().ok())
            .unwrap_or(u32::MAX);
        Claim::Repeat { capped_in_turn }
    }

    /// Remember the bound this window asked the client for, so the result of
    /// that call can be checked against it.
    ///
    /// The value and not merely the fact: an arming the executed call never
    /// answered outlives the call that made it, and the only thing that tells
    /// a bound Rift asked for from one the agent chose later is what the
    /// number is.
    pub(super) fn arm(&self, key: &str, head_limit: u32) {
        let _ = self.create(&format!("a-{key}"), head_limit.to_string().as_bytes());
    }

    /// Take the arming for `key`: the bound this window really did ask for on
    /// this search, consumed so that one amendment is answered once.
    ///
    /// A body this module did not write, or one from a build that wrote none,
    /// reads as a bound no policy can resolve to — so it matches nothing and
    /// books nothing.
    pub(super) fn take_armed(&self, key: &str) -> Option<u32> {
        let path = self.dir.join(format!("a-{key}"));
        let body = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok()?;
        Some(
            body.and_then(|body| body.trim().parse().ok())
                .unwrap_or(u32::MAX),
        )
    }

    /// Record that this search was asked again inside the turn it was decided
    /// in.
    ///
    /// Once per search: the figure is "how many decided searches were asked
    /// again", not "how many times". `prefix` keeps the two levers' figures
    /// apart — a bounded search asked again is the re-query rate, an answered
    /// one asked again is the escape hatch taken immediately, and a reader
    /// dividing one count by the other must not be handed their sum.
    pub(super) fn note_requery(&self, prefix: &str, key: &str) -> bool {
        self.create(&format!("{prefix}{key}"), b"")
    }

    /// Whether this window has stopped amending calls.
    pub(super) fn is_dropped(&self) -> bool {
        self.dir.join(DROPPED_MARKER).exists()
    }

    /// Stop amending, and say whether **this** call is the one that stopped it.
    pub(super) fn mark_dropped(&self) -> bool {
        self.create(DROPPED_MARKER, b"dropped")
    }

    /// Create one marker, and say whether this call created it.
    ///
    /// The names are digests and the bodies are a turn ordinal — nothing
    /// derived from the repository's text or from the agent's own words, which
    /// is why these need no narrower permissions than the run state around
    /// them.
    fn create(&self, name: &str, body: &[u8]) -> bool {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return false;
        }
        let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.dir.join(name))
        else {
            return false;
        };
        use std::io::Write as _;
        let _ = file.write_all(body);
        true
    }
}

// ---------------------------------------------------------- collecting it ---

/// **Collect what ended conversations left behind.**
///
/// Two kinds of thing accumulate under this member's search state, and neither
/// had anything that removed it: one directory per context window, holding one
/// small marker per decision, and one file per answered search holding the
/// paths that answer stood in for. A window is a conversation, conversations
/// end, and the markers of one that ended a month ago decide nothing.
///
/// By age and never wholesale, for the reason the recovery markers are: a
/// marker is what stops a live window being answered twice, and dropping a
/// fresh one costs a second decision in a conversation that is still open —
/// exactly the spend this lever exists to remove. Nothing this module did not
/// write is touched, however it got there: a window directory is named by a
/// digest, and an answer file by an id of the shape this process mints.
pub(crate) fn sweep(work_tree: &Path, now_ms: i64, ttl_ms: i64, dry_run: bool) -> (usize, usize) {
    let root = work_tree.join(".rift/state/search");
    let cutoff = now_ms.saturating_sub(ttl_ms);
    let expired = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|since| i64::try_from(since.as_millis()).ok())
            .is_some_and(|modified_ms| modified_ms < cutoff)
    };
    let mut windows = 0usize;
    if let Ok(entries) = std::fs::read_dir(root.join(WINDOW_DIR)) {
        for entry in entries.flatten() {
            let path = entry.path();
            let named = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| is_hex(name, 64));
            if !named || !path.is_dir() || !expired(&path) {
                continue;
            }
            windows += 1;
            if !dry_run {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
    let mut answers = 0usize;
    if let Ok(entries) = std::fs::read_dir(root.join(ANSWERS_DIR)) {
        for entry in entries.flatten() {
            let path = entry.path();
            let named = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| is_hex(name, 32));
            if !named || !path.is_file() || !expired(&path) {
                continue;
            }
            answers += 1;
            if !dry_run {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    (windows, answers)
}

/// Whether `name` is exactly `len` lowercase hex digits — the one shape this
/// module writes a path component in.
fn is_hex(name: &str, len: usize) -> bool {
    name.len() == len
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(feature = "search")]
#[cfg(test)]
mod tests {
    use crate::search_gate::tests::{gate, glob, hook, indexed, levers, markers};
    use crate::search_gate::SearchAnswer;

    /// **What an ended conversation left behind is collected by its own age,
    /// and a live one keeps what it needs.**
    #[cfg(feature = "search")]
    #[test]
    fn clean_removes_expired_search_windows() {
        let files: Vec<(String, String)> = (0..40)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    "fn f() {}\n".to_owned(),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let root = tmp.path();
        assert!(matches!(
            gate(&hook("PreToolUse", 1, glob("**/*.rs")), root, &levers()).answer,
            Some(SearchAnswer::Listing(_))
        ));
        let answers = root.join(".rift/state/search/answers");
        assert_eq!(markers(root, "g-"), 1);
        assert_eq!(answers.read_dir().expect("the paths priced").count(), 1);

        let now = 1_800_000_000_000_i64;
        let day = 86_400_000_i64;
        // A conversation younger than the window keeps everything: dropping a
        // live marker costs a second answer in a conversation still open.
        assert_eq!(super::sweep(root, now, now, false), (0, 0));
        assert_eq!(markers(root, "g-"), 1);

        // One that ended before the cutoff loses both, and a dry run says so
        // without doing it.
        assert_eq!(super::sweep(root, now, day, true), (1, 1));
        assert_eq!(markers(root, "g-"), 1, "a dry run removes nothing");
        assert_eq!(super::sweep(root, now, day, false), (1, 1));
        assert_eq!(markers(root, "g-"), 0);
        assert_eq!(answers.read_dir().expect("the directory stays").count(), 0);

        // Nothing this module did not write is touched, however it got there.
        let windows = root.join(".rift/state/search/w");
        std::fs::create_dir_all(windows.join("not-a-window")).expect("a directory");
        std::fs::write(answers.join("not-an-answer"), b"x").expect("a file");
        assert_eq!(super::sweep(root, now, day, false), (0, 0));
        assert!(windows.join("not-a-window").is_dir());
        assert!(answers.join("not-an-answer").is_file());
    }
}
