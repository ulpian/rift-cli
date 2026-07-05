//! The pure status truth table (P4 S2, reworked by M4-06).
//!
//! A project's live [`ProjectStatus`] is a **deterministic function** of read-only
//! inputs the data plane gathers off-thread ([`StatusInputs`]): whether the repo
//! path still resolves, the projection's readiness, the live-session cache read,
//! and whether anything has ever been captured. Keeping the decision here (pure,
//! total, and clock-free — staleness is judged by the *reader*, which is
//! clock-injected) means every transition is unit-testable without a store or a
//! terminal — the data plane only supplies the facts.
//!
//! `Running` derives from the **real per-session cache** the capture hooks
//! maintain (`<git_dir>/rift-sessions/*.json`), not from recent file noise: the
//! old `.rift/session.phase` reader and its recency proxy are gone (PRD-M4 user
//! story 10).

use crate::{ProjectStatus, StatusError};

/// The readiness of a project's per-repo projection, as classified by the bin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionState {
    /// The projection opened and is queryable.
    Ready,
    /// Initialized (`.rift/` present) but the projection is not up yet.
    Building,
    /// The path is registered but `rift init` has not been run (no `.rift/`).
    Uninitialized,
    /// The projection exists but could not be opened/read (locked/corrupt).
    Unavailable,
}

/// The live sessions observed for a project, already staleness-filtered by the
/// reader (the data plane's session-cache query).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveSessions {
    /// How many distinct sessions are currently active.
    pub active_sessions: u16,
    /// When the current activity began (epoch-ms; the earliest live session's
    /// last update).
    pub since_ms: i64,
}

/// The outcome of reading a project's session cache. `Corrupt` is distinct
/// from `Ok(None)` so [`derive`] can report
/// [`StatusError::CorruptSessionCache`] rather than silently treating a
/// garbled cache as "no session".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionsRead {
    /// The cache was read (or is absent → `None`).
    Ok(Option<LiveSessions>),
    /// A cache file exists but could not be parsed.
    Corrupt,
}

/// The read-only inputs the status decision is a pure function of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusInputs {
    /// Whether the registered repository path still resolves.
    pub repo_present: bool,
    /// The projection's readiness.
    pub projection: ProjectionState,
    /// The live-session cache read.
    pub sessions: SessionsRead,
    /// Whether the projection holds any captured event at all (an empty-but-
    /// ready projection is still warming up).
    pub has_captures: bool,
}

/// Derive a project's [`ProjectStatus`] from its read-only inputs. Total and
/// deterministic; the precedence is: a missing repo, then an uninitialized or
/// unreadable projection, then a corrupt session cache, then a still-building
/// projection, and finally — for a ready projection — live sessions
/// (`Running`), captured history (`Idle`), or nothing captured yet
/// (`PickingUp`).
#[must_use]
pub fn derive(i: &StatusInputs) -> ProjectStatus {
    if !i.repo_present {
        return ProjectStatus::Error(StatusError::RepoMissing);
    }
    match i.projection {
        ProjectionState::Uninitialized => return ProjectStatus::NotInitialized,
        ProjectionState::Unavailable => {
            return ProjectStatus::Error(StatusError::ProjectionUnavailable)
        }
        ProjectionState::Building | ProjectionState::Ready => {}
    }
    if matches!(i.sessions, SessionsRead::Corrupt) {
        return ProjectStatus::Error(StatusError::CorruptSessionCache);
    }
    if matches!(i.projection, ProjectionState::Building) {
        return ProjectStatus::PickingUp;
    }
    // Ready + a readable cache: a live session is unambiguously Running.
    if let SessionsRead::Ok(Some(s)) = i.sessions {
        if s.active_sessions > 0 {
            return ProjectStatus::Running {
                since_ms: s.since_ms,
                active_sessions: s.active_sessions,
            };
        }
    }
    // No live session: captured history reads Idle; a bare projection is
    // still picking up. (The old "recent activity is Running" proxy is gone —
    // Running now means a real live session, PRD-M4 user story 10.)
    if i.has_captures {
        ProjectStatus::Idle
    } else {
        ProjectStatus::PickingUp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A baseline "ready, quiet, nothing captured" input; tests override fields.
    fn base() -> StatusInputs {
        StatusInputs {
            repo_present: true,
            projection: ProjectionState::Ready,
            sessions: SessionsRead::Ok(None),
            has_captures: false,
        }
    }

    #[test]
    fn case1_missing_repo() {
        let i = StatusInputs {
            repo_present: false,
            ..base()
        };
        assert_eq!(derive(&i), ProjectStatus::Error(StatusError::RepoMissing));
    }

    #[test]
    fn case2_uninitialized() {
        let i = StatusInputs {
            projection: ProjectionState::Uninitialized,
            ..base()
        };
        assert_eq!(derive(&i), ProjectStatus::NotInitialized);
    }

    #[test]
    fn case3_projection_unavailable() {
        let i = StatusInputs {
            projection: ProjectionState::Unavailable,
            ..base()
        };
        assert_eq!(
            derive(&i),
            ProjectStatus::Error(StatusError::ProjectionUnavailable)
        );
    }

    #[test]
    fn case4_corrupt_session_cache() {
        let i = StatusInputs {
            sessions: SessionsRead::Corrupt,
            ..base()
        };
        assert_eq!(
            derive(&i),
            ProjectStatus::Error(StatusError::CorruptSessionCache)
        );
    }

    #[test]
    fn case5_building_is_picking_up() {
        let i = StatusInputs {
            projection: ProjectionState::Building,
            ..base()
        };
        assert_eq!(derive(&i), ProjectStatus::PickingUp);
    }

    #[test]
    fn case6_live_session_is_running() {
        let i = StatusInputs {
            sessions: SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 2,
                since_ms: 500,
            })),
            ..base()
        };
        assert_eq!(
            derive(&i),
            ProjectStatus::Running {
                since_ms: 500,
                active_sessions: 2
            }
        );
    }

    #[test]
    fn case7_history_without_sessions_is_idle_never_running() {
        // The recency proxy is gone: captured history alone is Idle no matter
        // how fresh it is (Running requires a live session).
        let i = StatusInputs {
            has_captures: true,
            ..base()
        };
        assert_eq!(derive(&i), ProjectStatus::Idle);
    }

    #[test]
    fn case8_ready_but_nothing_captured_is_picking_up() {
        assert_eq!(derive(&base()), ProjectStatus::PickingUp);
    }

    #[test]
    fn zero_active_sessions_reads_as_quiet() {
        // A readable cache whose sessions all ended: not Running.
        let i = StatusInputs {
            sessions: SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 0,
                since_ms: 500,
            })),
            has_captures: true,
            ..base()
        };
        assert_eq!(derive(&i), ProjectStatus::Idle);
    }

    #[test]
    fn corrupt_cache_outranks_building_and_ready() {
        // A corrupt cache surfaces as an error whether building or ready.
        for projection in [ProjectionState::Building, ProjectionState::Ready] {
            let i = StatusInputs {
                projection,
                sessions: SessionsRead::Corrupt,
                ..base()
            };
            assert_eq!(
                derive(&i),
                ProjectStatus::Error(StatusError::CorruptSessionCache)
            );
        }
    }

    #[test]
    fn glyph_and_ascii_fallback_mapping() {
        // Every status has a distinct unicode glyph and an ascii fallback token.
        let all = [
            ProjectStatus::Running {
                since_ms: 0,
                active_sessions: 1,
            },
            ProjectStatus::PickingUp,
            ProjectStatus::Idle,
            ProjectStatus::NotInitialized,
            ProjectStatus::Error(StatusError::RepoMissing),
        ];
        let glyphs: Vec<char> = all.iter().map(|s| s.glyph()).collect();
        let ascii: Vec<&str> = all.iter().map(|s| s.glyph_ascii()).collect();
        assert_eq!(glyphs, vec!['●', '◐', '○', '·', '!']);
        assert_eq!(ascii, vec!["[R]", "[~]", "[ ]", "[-]", "[!]"]);
    }

    fn projection_strategy() -> impl Strategy<Value = ProjectionState> {
        prop_oneof![
            Just(ProjectionState::Ready),
            Just(ProjectionState::Building),
            Just(ProjectionState::Uninitialized),
            Just(ProjectionState::Unavailable),
        ]
    }

    fn sessions_strategy() -> impl Strategy<Value = SessionsRead> {
        prop_oneof![
            Just(SessionsRead::Ok(None)),
            Just(SessionsRead::Corrupt),
            (0u16..4, 0i64..2_000_000).prop_map(|(active_sessions, since_ms)| SessionsRead::Ok(
                Some(LiveSessions {
                    active_sessions,
                    since_ms,
                })
            )),
        ]
    }

    proptest! {
        /// `derive` is total: it never panics and always yields a status for any
        /// combination of inputs (the "status truth table is complete" guarantee).
        #[test]
        fn derive_is_total(
            repo_present in any::<bool>(),
            projection in projection_strategy(),
            sessions in sessions_strategy(),
            has_captures in any::<bool>(),
        ) {
            let i = StatusInputs { repo_present, projection, sessions, has_captures };
            let s = derive(&i);
            // The glyph mapping is likewise total (never falls through).
            let _ = s.glyph();
            let _ = s.glyph_ascii();
        }
    }
}
