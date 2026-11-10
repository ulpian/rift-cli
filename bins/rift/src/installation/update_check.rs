//! The update offer made when `rift` or `rift init` starts.
//!
//! At most once a day, an installer-managed `rift` run interactively reads
//! the signed release feed (three seconds at most, and nothing else on the
//! way to the dashboard waits on it). When a newer release exists it asks
//! once whether to update now. Declining is remembered for that release, so
//! the question returns only when a newer one appears. Nothing is asked in CI,
//! without a terminal, with `RIFT_NONINTERACTIVE=1`, or with
//! `RIFT_NO_UPDATE_CHECK=1`; development builds are never offered updates,
//! because only an installer-managed executable can be replaced.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{alpha_key, release_feed, Installation};
use crate::cli::UpdateArgs;

/// How long a feed reading stays fresh.
const RECHECK_SECS: u64 = 24 * 60 * 60;
/// The feed read's time bound on the way into the dashboard.
const FEED_TIMEOUT: Duration = Duration::from_secs(3);
const STATE_HEADER: &str = "rift-update-check-v1";

/// What the caller does after the offer.
pub(crate) enum Offer {
    /// Carry on with the command that was run.
    Continue,
    /// Rift was replaced; the running process is the old version, so the
    /// caller stops and the person runs the command again.
    Updated,
}

/// The remembered outcome of the last feed reading.
#[derive(Debug, Default, PartialEq, Eq)]
struct State {
    /// Unix seconds of the last reading.
    checked: u64,
    /// The newest release the feed named, if the reading succeeded.
    latest: Option<String>,
    /// A release the person declined; it is not offered again.
    declined: Option<String>,
}

impl State {
    fn parse(text: &str) -> Self {
        let mut lines = text.lines();
        if lines.next() != Some(STATE_HEADER) {
            return Self::default();
        }
        let checked = lines.next().and_then(|l| l.parse().ok()).unwrap_or(0);
        let tag = |line: Option<&str>| line.filter(|v| alpha_key(v).is_some()).map(str::to_owned);
        Self {
            checked,
            latest: tag(lines.next()),
            declined: tag(lines.next()),
        }
    }

    fn render(&self) -> String {
        format!(
            "{STATE_HEADER}\n{}\n{}\n{}\n",
            self.checked,
            self.latest.as_deref().unwrap_or("-"),
            self.declined.as_deref().unwrap_or("-")
        )
    }

    /// Whether the feed should be read again at `now`. A clock that moved
    /// backwards counts as due rather than silencing checks until it returns.
    fn due(&self, now: u64) -> bool {
        now < self.checked || now - self.checked >= RECHECK_SECS
    }

    /// The release to offer someone running `current`, if any.
    fn offer_for(&self, current: &str) -> Option<&str> {
        let latest = self.latest.as_deref()?;
        let newer = alpha_key(latest)? > alpha_key(current)?;
        (newer && self.declined.as_deref() != Some(latest)).then_some(latest)
    }
}

/// Offer a newer release before `rift` or `rift init` goes on.
pub(crate) fn offer() -> Offer {
    if !interactive() {
        return Offer::Continue;
    }
    let Ok(install) = Installation::current() else {
        return Offer::Continue;
    };
    let path = state_path();
    let mut state =
        std::fs::read_to_string(&path).map_or_else(|_| State::default(), |t| State::parse(&t));
    let now = unix_now();
    if state.due(now) {
        state.checked = now;
        if let Ok(signed) = release_feed::fetch_signed(&release_feed::latest_link(), FEED_TIMEOUT) {
            state.latest = Some(signed.version);
        }
        save(&path, &state);
    }
    let Some(latest) = state.offer_for(&install.version).map(str::to_owned) else {
        return Offer::Continue;
    };
    if ask(&format!(
        "Rift {latest} is available (you have {}). Update now? [Y/n] ",
        install.version
    )) {
        return apply(&latest);
    }
    state.declined = Some(latest);
    save(&path, &state);
    eprintln!("Skipped. Run `rift update` whenever you like.");
    Offer::Continue
}

fn apply(latest: &str) -> Offer {
    match super::update(&UpdateArgs { from: None }) {
        Ok(outcome) => {
            eprintln!("{}", outcome.human);
            eprintln!("Run the command again to use Rift {latest}.");
            Offer::Updated
        }
        Err(error) => {
            eprintln!("rift: the update did not complete ({error}); continuing with this version.");
            Offer::Continue
        }
    }
}

fn interactive() -> bool {
    let set = |key: &str| std::env::var_os(key).is_some_and(|v| !v.is_empty() && v != "0");
    !set("RIFT_NO_UPDATE_CHECK")
        && !set("RIFT_NONINTERACTIVE")
        && std::env::var_os("CI").is_none()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
}

/// Ask `question` on stderr; an empty answer or "y"/"yes" is yes.
fn ask(question: &str) -> bool {
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    )
}

fn state_path() -> PathBuf {
    crate::family::cache_root()
        .join("rift")
        .join("update-check")
}

/// Best effort: a state that cannot be saved only means the feed is read
/// again next time.
fn save(path: &Path, state: &State) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, state.render());
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(checked: u64, latest: Option<&str>, declined: Option<&str>) -> State {
        State {
            checked,
            latest: latest.map(str::to_owned),
            declined: declined.map(str::to_owned),
        }
    }

    #[test]
    fn the_feed_is_read_at_most_once_a_day_and_again_after_a_clock_jump_back() {
        let s = state(1_000_000, None, None);
        assert!(!s.due(1_000_000 + RECHECK_SECS - 1));
        assert!(s.due(1_000_000 + RECHECK_SECS));
        assert!(s.due(999_999));
        assert!(
            State::default().due(1_000_000),
            "a state never written is due"
        );
    }

    #[test]
    fn only_a_newer_release_that_was_not_declined_is_offered() {
        let s = state(0, Some("v1.2.0-alpha"), None);
        assert_eq!(s.offer_for("v1.1.0-alpha"), Some("v1.2.0-alpha"));
        assert_eq!(s.offer_for("v1.2.0-alpha"), None);
        assert_eq!(s.offer_for("v1.3.0-alpha"), None);
        let declined = state(0, Some("v1.2.0-alpha"), Some("v1.2.0-alpha"));
        assert_eq!(declined.offer_for("v1.1.0-alpha"), None);
        let newer_than_declined = state(0, Some("v1.2.1-alpha"), Some("v1.2.0-alpha"));
        assert_eq!(
            newer_than_declined.offer_for("v1.1.0-alpha"),
            Some("v1.2.1-alpha")
        );
    }

    #[test]
    fn the_state_round_trips_and_a_damaged_file_starts_over() {
        let s = state(42, Some("v1.2.0-alpha"), Some("v1.1.0-alpha"));
        assert_eq!(State::parse(&s.render()), s);
        let empty = state(7, None, None);
        assert_eq!(State::parse(&empty.render()), empty);
        assert_eq!(State::parse("garbage"), State::default());
        assert_eq!(
            State::parse("rift-update-check-v1\n9\nv1.0.0-alphaevil\n-\n"),
            state(9, None, None)
        );
    }
}
