//! The pure status truth table (P4 S2).
//!
//! A project's live [`ProjectStatus`] is a **deterministic function** of read-only
//! inputs the bin gathers off-thread ([`StatusInputs`]): whether the repo path
//! still resolves, the projection's readiness, the session phase file, and the age
//! of the most recent captured event. Keeping the decision here (pure, total, and
//! clock-injected via `now_ms`) means every transition is unit-testable without a
//! store or a terminal — the bin only supplies the facts.

use crate::{ProjectStatus, StatusError};

/// Activity within this window (ms) still counts as `Running` even with no live
/// session — so a just-finished burst doesn't flicker straight to `Idle`.
pub const RECENT_WINDOW_MS: i64 = 5 * 60 * 1000;

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

/// A live session's phase, as read from a project's session phase file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionPhase {
    /// How many agent sessions are currently active.
    pub active_sessions: u16,
    /// When the current activity began (epoch-ms).
    pub since_ms: i64,
}

/// The outcome of reading a project's session phase file. `Corrupt` is distinct
/// from `Ok(None)` so [`derive`] can report [`StatusError::CorruptPhaseFile`]
/// rather than silently treating a garbled file as "no session".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseRead {
    /// The phase file was read (or is absent → `None`).
    Ok(Option<SessionPhase>),
    /// The phase file exists but could not be parsed.
    Corrupt,
}

/// The read-only inputs the status decision is a pure function of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusInputs {
    /// Whether the registered repository path still resolves.
    pub repo_present: bool,
    /// The projection's readiness.
    pub projection: ProjectionState,
    /// The session phase read.
    pub phase: PhaseRead,
    /// Age (ms) of the most recent captured event, or `None` if none captured.
    pub last_event_age_ms: Option<i64>,
    /// The current time (epoch-ms), injected so the decision is deterministic.
    pub now_ms: i64,
}

/// Derive a project's [`ProjectStatus`] from its read-only inputs. Total and
/// deterministic; the precedence is: a missing repo, then an uninitialized or
/// unreadable projection, then a corrupt phase file, then a still-building
/// projection, and finally — for a ready projection — a live session or recent
/// activity (`Running`), older activity (`Idle`), or nothing captured yet
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
    if matches!(i.phase, PhaseRead::Corrupt) {
        return ProjectStatus::Error(StatusError::CorruptPhaseFile);
    }
    if matches!(i.projection, ProjectionState::Building) {
        return ProjectStatus::PickingUp;
    }
    // Ready + a readable phase: a live session is unambiguously Running.
    if let PhaseRead::Ok(Some(p)) = i.phase {
        if p.active_sessions > 0 {
            return ProjectStatus::Running {
                since_ms: p.since_ms,
                active_sessions: p.active_sessions,
            };
        }
    }
    // No live session: recent capture activity still reads as Running.
    match i.last_event_age_ms {
        Some(age) if age < RECENT_WINDOW_MS => ProjectStatus::Running {
            since_ms: i.now_ms.saturating_sub(age),
            active_sessions: 0,
        },
        Some(_) => ProjectStatus::Idle,
        None => ProjectStatus::PickingUp,
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
            phase: PhaseRead::Ok(None),
            last_event_age_ms: None,
            now_ms: 1_000_000,
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
    fn case4_corrupt_phase() {
        let i = StatusInputs {
            phase: PhaseRead::Corrupt,
            ..base()
        };
        assert_eq!(
            derive(&i),
            ProjectStatus::Error(StatusError::CorruptPhaseFile)
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
            phase: PhaseRead::Ok(Some(SessionPhase {
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
    fn case7_recent_activity_is_running() {
        let i = StatusInputs {
            last_event_age_ms: Some(RECENT_WINDOW_MS - 1),
            ..base()
        };
        assert_eq!(
            derive(&i),
            ProjectStatus::Running {
                since_ms: i.now_ms - (RECENT_WINDOW_MS - 1),
                active_sessions: 0
            }
        );
    }

    #[test]
    fn case8_old_activity_is_idle() {
        let i = StatusInputs {
            last_event_age_ms: Some(RECENT_WINDOW_MS),
            ..base()
        };
        assert_eq!(derive(&i), ProjectStatus::Idle);
    }

    #[test]
    fn case9_ready_but_nothing_captured_is_picking_up() {
        assert_eq!(derive(&base()), ProjectStatus::PickingUp);
    }

    #[test]
    fn corrupt_phase_outranks_building_and_ready() {
        // A corrupt phase surfaces as an error whether building or ready.
        for projection in [ProjectionState::Building, ProjectionState::Ready] {
            let i = StatusInputs {
                projection,
                phase: PhaseRead::Corrupt,
                ..base()
            };
            assert_eq!(
                derive(&i),
                ProjectStatus::Error(StatusError::CorruptPhaseFile)
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

    fn phase_strategy() -> impl Strategy<Value = PhaseRead> {
        prop_oneof![
            Just(PhaseRead::Ok(None)),
            Just(PhaseRead::Corrupt),
            (0u16..4, 0i64..2_000_000).prop_map(|(active_sessions, since_ms)| PhaseRead::Ok(Some(
                SessionPhase {
                    active_sessions,
                    since_ms,
                }
            ))),
        ]
    }

    proptest! {
        /// `derive` is total: it never panics and always yields a status for any
        /// combination of inputs (the "status truth table is complete" guarantee).
        #[test]
        fn derive_is_total(
            repo_present in any::<bool>(),
            projection in projection_strategy(),
            phase in phase_strategy(),
            age in proptest::option::of(0i64..10_000_000),
            now_ms in 0i64..10_000_000,
        ) {
            let i = StatusInputs { repo_present, projection, phase, last_event_age_ms: age, now_ms };
            let s = derive(&i);
            // The glyph mapping is likewise total (never falls through).
            let _ = s.glyph();
            let _ = s.glyph_ascii();
        }
    }
}
