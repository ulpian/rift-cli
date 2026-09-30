//! The pure status truth table.
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
//! old `.rift/session.phase` reader and its recency proxy are gone.

use crate::{ProjectStatus, StatusError};

/// The readiness of a project's per-repo projection, as classified by the bin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionState {
    /// The projection opened and is queryable.
    Ready,
    /// Initialized but the projection is not up yet.
    Building,
    /// The path is registered but `rift init` has not been run: no `.rift/`
    /// and no projection for the family on this machine.
    Uninitialized,
    /// The repository carries a `.rift/` — committed configuration that
    /// travelled with the clone — but this machine holds no projection for
    /// its family: configured elsewhere, never set up here. `.rift/` alone
    /// used to read as initialized, so a fresh clone of a repository that
    /// shares its configuration looked set up on every machine and the
    /// setup wizard (with its pre-ticked detected agents) never appeared.
    Configured,
    /// The registered path is not inside a git repository, so it has no
    /// family and can never hold a projection; nothing to provision.
    NotRepository,
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
/// from `Ok(None)` so [`derive`](fn@derive) can report
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
    /// Whether this row is a **provisioned member** of a repository family —
    /// `None` for a row that is not a member at all (the family view).
    ///
    /// Three-valued on purpose. The family row itself and any non-repository
    /// path are `None` (the question does not apply: `rift init` provisions the
    /// main work tree and the answer would be a category error, not a fact).
    /// A **linked worktree** answers `Some(true)`/`Some(false)`, and only
    /// `Some(false)` — a member of an initialized family that was never
    /// adopted, so nothing it does is captured — changes the derived status.
    pub provisioned: Option<bool>,
    /// Whether an error-severity integration finding exists: a wired agent's
    /// capture is being lost, or Rift's own session state is wrong.
    ///
    /// A `bool` rather than the reason: the truth table reads only whether
    /// one exists, and the reasons with their remedies travel to Project
    /// Settings in its adapter rows, so a payload here would be a second copy
    /// that could disagree with them. Warnings never set it: nothing is lost
    /// while they hold.
    pub integration_failing: bool,
}

/// Derive a project's [`ProjectStatus`] from its read-only inputs. Total and
/// deterministic; the precedence is: a missing repo, then an uninitialized or
/// unreadable projection, then an **unadopted member**, then a corrupt session
/// cache, then an **error-severity integration finding**, then a
/// still-building projection, and finally — for a ready projection — live
/// sessions (`Running`), captured history (`Idle`), or nothing captured yet
/// (`PickingUp`).
///
/// **Why an integration error outranks `Running` and `Idle`:** both would
/// read as healthy while a wired agent's capture is being lost, which is the
/// silent failure the finding exists to name. It stays below every state that
/// makes capture meaningless here, because those states' acts come first.
///
/// **Why `NotAdopted` outranks the session cache and the build state**
/// (the family view places the rule immediately after `Uninitialized`/
/// `Unavailable`): the two facts it would hide are both about capture, and in
/// an unadopted member neither can be acted on. A corrupt cache file there is
/// not the operator's next move — adopting is, after which the cache is
/// rewritten by hooks that now exist. A `Building` family projection is the
/// FAMILY's state, never the member's; it says nothing about whether this
/// checkout is wired, so it must not outrank the answer that it is not.
#[must_use]
pub fn derive(i: &StatusInputs) -> ProjectStatus {
    if !i.repo_present {
        return ProjectStatus::Error(StatusError::RepoMissing);
    }
    match i.projection {
        ProjectionState::Uninitialized => return ProjectStatus::NotInitialized,
        ProjectionState::Configured => return ProjectStatus::NotProvisioned,
        ProjectionState::NotRepository => return ProjectStatus::NotRepository,
        ProjectionState::Unavailable => {
            return ProjectStatus::Error(StatusError::ProjectionUnavailable)
        }
        ProjectionState::Building | ProjectionState::Ready => {}
    }
    // An initialized family whose member was never provisioned: nothing in this
    // checkout is wired to Rift, so every capture-shaped answer below would be
    // a truthful reading of an untruthful premise.
    if i.provisioned == Some(false) {
        return ProjectStatus::NotAdopted;
    }
    if matches!(i.sessions, SessionsRead::Corrupt) {
        return ProjectStatus::Error(StatusError::CorruptSessionCache);
    }
    if i.integration_failing {
        return ProjectStatus::Error(StatusError::Integration);
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
    // Running now means a real live session.)
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

    /// A baseline "ready, quiet, nothing captured, not a member" input; the
    /// original eight cases override one field of it each.
    ///
    /// The cases the family view adds spell **every** field instead: the
    /// rule they pin is about the interaction between two fields, and
    /// `..base()` hides which of the other three the answer depended on.
    fn base() -> StatusInputs {
        StatusInputs {
            repo_present: true,
            projection: ProjectionState::Ready,
            sessions: SessionsRead::Ok(None),
            has_captures: false,
            provisioned: None,
            integration_failing: false,
        }
    }

    /// One live session, freshly updated — the input several cases below need
    /// in order to state what does *not* turn into `Running`.
    fn one_live_session() -> SessionsRead {
        SessionsRead::Ok(Some(LiveSessions {
            active_sessions: 1,
            since_ms: 500,
        }))
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
    fn case9_an_unprovisioned_member_is_not_adopted() {
        // the family view. Every field spelled: the point is that a member of a
        // READY family with CAPTURED history and a LIVE session still reads
        // `NotAdopted`, so none of the three can be blamed for the answer.
        let i = StatusInputs {
            repo_present: true,
            projection: ProjectionState::Ready,
            sessions: one_live_session(),
            has_captures: true,
            provisioned: Some(false),
            integration_failing: false,
        };
        assert_eq!(derive(&i), ProjectStatus::NotAdopted);
    }

    #[test]
    fn case10_a_provisioned_member_derives_exactly_as_a_non_member() {
        // `Some(true)` and `None` must be indistinguishable: the rule fires on
        // a proved negative only, never on the presence of the question.
        for sessions in [
            SessionsRead::Ok(None),
            one_live_session(),
            SessionsRead::Corrupt,
        ] {
            for has_captures in [false, true] {
                let member = StatusInputs {
                    repo_present: true,
                    projection: ProjectionState::Ready,
                    sessions,
                    has_captures,
                    provisioned: Some(true),
                    integration_failing: false,
                };
                let not_a_member = StatusInputs {
                    provisioned: None,
                    ..member
                };
                assert_eq!(derive(&member), derive(&not_a_member));
            }
        }
    }

    #[test]
    fn case11_not_adopted_outranks_a_corrupt_cache_and_a_building_projection() {
        // The rule's PLACE in the chain, pinned from both sides it could slip
        // past. Adopting is the act either way; a cache error inside an
        // unwired checkout is not the operator's next move, and `Building` is
        // the FAMILY projection's state, never this member's.
        let corrupt = StatusInputs {
            repo_present: true,
            projection: ProjectionState::Ready,
            sessions: SessionsRead::Corrupt,
            has_captures: false,
            provisioned: Some(false),
            integration_failing: false,
        };
        assert_eq!(derive(&corrupt), ProjectStatus::NotAdopted);
        let building = StatusInputs {
            repo_present: true,
            projection: ProjectionState::Building,
            sessions: one_live_session(),
            has_captures: false,
            provisioned: Some(false),
            integration_failing: false,
        };
        assert_eq!(derive(&building), ProjectStatus::NotAdopted);
    }

    #[test]
    fn case12_an_unadopted_member_whose_checkout_is_gone_still_reads_repo_missing() {
        // A prunable member: the administrative entry survives, the directory
        // does not. `RepoMissing` outranks `NotAdopted` because adopting a
        // directory that is not there cannot be the advice (the family view's
        // prunable arm).
        let i = StatusInputs {
            repo_present: false,
            projection: ProjectionState::Ready,
            sessions: SessionsRead::Ok(None),
            has_captures: true,
            provisioned: Some(false),
            integration_failing: false,
        };
        assert_eq!(derive(&i), ProjectStatus::Error(StatusError::RepoMissing));
    }

    #[test]
    fn case13_a_building_projection_with_a_live_session_is_picking_up_not_running() {
        // A review's second missing case, and it is not a formality: the
        // family projection can be mid-rebuild while a member's agent is
        // actively typing. `Running` would claim a readable projection behind
        // the count; `PickingUp` is what is true.
        let i = StatusInputs {
            repo_present: true,
            projection: ProjectionState::Building,
            sessions: one_live_session(),
            has_captures: true,
            provisioned: None,
            integration_failing: false,
        };
        assert_eq!(derive(&i), ProjectStatus::PickingUp);
    }

    #[test]
    fn case14_an_error_severity_finding_turns_an_idle_or_running_project_to_an_integration_error() {
        for sessions in [SessionsRead::Ok(None), one_live_session()] {
            let i = StatusInputs {
                repo_present: true,
                projection: ProjectionState::Ready,
                sessions,
                has_captures: true,
                provisioned: None,
                integration_failing: true,
            };
            assert_eq!(derive(&i), ProjectStatus::Error(StatusError::Integration));
        }
    }

    #[test]
    fn case15_a_project_with_no_error_severity_finding_stays_idle_or_running() {
        // Warnings never reach the inputs: the data plane sets the field from
        // error-severity findings only, so a warning-only project reads here
        // exactly like a healthy one.
        let idle = StatusInputs {
            repo_present: true,
            projection: ProjectionState::Ready,
            sessions: SessionsRead::Ok(None),
            has_captures: true,
            provisioned: None,
            integration_failing: false,
        };
        assert_eq!(derive(&idle), ProjectStatus::Idle);
        let running = StatusInputs {
            sessions: one_live_session(),
            ..idle
        };
        assert!(matches!(derive(&running), ProjectStatus::Running { .. }));
    }

    #[test]
    fn case16_every_state_above_an_integration_error_still_wins_over_it() {
        let failing = StatusInputs {
            integration_failing: true,
            has_captures: true,
            ..base()
        };
        let cases = [
            (
                StatusInputs {
                    repo_present: false,
                    ..failing
                },
                ProjectStatus::Error(StatusError::RepoMissing),
            ),
            (
                StatusInputs {
                    projection: ProjectionState::Uninitialized,
                    ..failing
                },
                ProjectStatus::NotInitialized,
            ),
            (
                StatusInputs {
                    projection: ProjectionState::Configured,
                    ..failing
                },
                ProjectStatus::NotProvisioned,
            ),
            (
                StatusInputs {
                    projection: ProjectionState::NotRepository,
                    ..failing
                },
                ProjectStatus::NotRepository,
            ),
            (
                StatusInputs {
                    projection: ProjectionState::Unavailable,
                    ..failing
                },
                ProjectStatus::Error(StatusError::ProjectionUnavailable),
            ),
            (
                StatusInputs {
                    provisioned: Some(false),
                    ..failing
                },
                ProjectStatus::NotAdopted,
            ),
            (
                StatusInputs {
                    sessions: SessionsRead::Corrupt,
                    ..failing
                },
                ProjectStatus::Error(StatusError::CorruptSessionCache),
            ),
        ];
        for (inputs, expected) in cases {
            assert_eq!(derive(&inputs), expected, "{inputs:?}");
        }
    }

    #[test]
    fn case17_an_integration_error_outranks_a_building_projection() {
        let i = StatusInputs {
            projection: ProjectionState::Building,
            integration_failing: true,
            ..base()
        };
        assert_eq!(derive(&i), ProjectStatus::Error(StatusError::Integration));
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
            ProjectStatus::NotProvisioned,
            ProjectStatus::NotRepository,
            ProjectStatus::NotAdopted,
            ProjectStatus::Error(StatusError::RepoMissing),
        ];
        let glyphs: Vec<char> = all.iter().map(|s| s.glyph()).collect();
        let ascii: Vec<&str> = all.iter().map(|s| s.glyph_ascii()).collect();
        assert_eq!(glyphs, vec!['●', '◐', '○', '·', '◌', '×', '◔', '!']);
        assert_eq!(
            ascii,
            vec!["[R]", "[~]", "[ ]", "[-]", "[s]", "[x]", "[a]", "[!]"]
        );
    }

    fn projection_strategy() -> impl Strategy<Value = ProjectionState> {
        prop_oneof![
            Just(ProjectionState::Ready),
            Just(ProjectionState::Building),
            Just(ProjectionState::Uninitialized),
            Just(ProjectionState::Configured),
            Just(ProjectionState::NotRepository),
            Just(ProjectionState::Unavailable),
        ]
    }

    fn provisioned_strategy() -> impl Strategy<Value = Option<bool>> {
        prop_oneof![Just(None), Just(Some(true)), Just(Some(false))]
    }

    /// An **independent** statement of the same truth table, written as one
    /// nested match over the whole input rather than as [`derive`]'s chain of
    /// early returns. Editing either alone makes the two disagree, which is
    /// what review found missing: `derive_is_total` asserted only that
    /// nothing panicked, so a `derive` that returned `Idle` for every input
    /// would have passed it.
    // Two arms genuinely share a body: `Building` and a bare ready projection
    // both read `PickingUp`. Merging them would destroy the property that makes
    // this an oracle — that every row of the table is written out separately.
    #[allow(clippy::match_same_arms)]
    fn expected(i: &StatusInputs) -> ProjectStatus {
        match (
            i.repo_present,
            i.projection,
            i.provisioned,
            i.sessions,
            i.integration_failing,
            i.has_captures,
        ) {
            (false, ..) => ProjectStatus::Error(StatusError::RepoMissing),
            (true, ProjectionState::Uninitialized, ..) => ProjectStatus::NotInitialized,
            (true, ProjectionState::Configured, ..) => ProjectStatus::NotProvisioned,
            (true, ProjectionState::NotRepository, ..) => ProjectStatus::NotRepository,
            (true, ProjectionState::Unavailable, ..) => {
                ProjectStatus::Error(StatusError::ProjectionUnavailable)
            }
            (true, _, Some(false), ..) => ProjectStatus::NotAdopted,
            (true, _, _, SessionsRead::Corrupt, ..) => {
                ProjectStatus::Error(StatusError::CorruptSessionCache)
            }
            (true, _, _, _, true, _) => ProjectStatus::Error(StatusError::Integration),
            (true, ProjectionState::Building, ..) => ProjectStatus::PickingUp,
            (true, ProjectionState::Ready, _, SessionsRead::Ok(Some(s)), false, _)
                if s.active_sessions > 0 =>
            {
                ProjectStatus::Running {
                    since_ms: s.since_ms,
                    active_sessions: s.active_sessions,
                }
            }
            (true, ProjectionState::Ready, _, _, false, true) => ProjectStatus::Idle,
            (true, ProjectionState::Ready, _, _, false, false) => ProjectStatus::PickingUp,
        }
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
        /// `derive` is total **and right**: every combination of inputs yields
        /// the status the independently-written table says it should, and the
        /// glyph mapping never falls through (this used to assert only that
        /// nothing panicked).
        #[test]
        fn derive_matches_the_truth_table_case_for_case(
            repo_present in any::<bool>(),
            projection in projection_strategy(),
            sessions in sessions_strategy(),
            has_captures in any::<bool>(),
            provisioned in provisioned_strategy(),
            integration_failing in any::<bool>(),
        ) {
            let i = StatusInputs {
                repo_present, projection, sessions, has_captures, provisioned, integration_failing,
            };
            let s = derive(&i);
            prop_assert_eq!(s, expected(&i));
            // The glyph mapping is likewise total (never falls through).
            let _ = s.glyph();
            let _ = s.glyph_ascii();
        }

        /// `NotAdopted` is derived from exactly one premise, in both
        /// directions: a present checkout, a projection that is neither
        /// uninitialized nor unreadable, and a member proved unprovisioned.
        /// Nothing else can produce it, and that premise can produce nothing
        /// else.
        #[test]
        fn not_adopted_is_derived_from_exactly_one_premise(
            repo_present in any::<bool>(),
            projection in projection_strategy(),
            sessions in sessions_strategy(),
            has_captures in any::<bool>(),
            provisioned in provisioned_strategy(),
            integration_failing in any::<bool>(),
        ) {
            let i = StatusInputs {
                repo_present, projection, sessions, has_captures, provisioned, integration_failing,
            };
            let premise = repo_present
                && matches!(projection, ProjectionState::Ready | ProjectionState::Building)
                && provisioned == Some(false);
            prop_assert_eq!(derive(&i) == ProjectStatus::NotAdopted, premise);
        }

        /// `Error(Integration)` is derived from exactly one premise, in both
        /// directions: a present, adopted checkout over a readable or building
        /// projection with a readable session cache, and an error-severity
        /// finding. Every state above it in the chain still wins.
        #[test]
        fn an_integration_error_is_derived_from_exactly_one_premise(
            repo_present in any::<bool>(),
            projection in projection_strategy(),
            sessions in sessions_strategy(),
            has_captures in any::<bool>(),
            provisioned in provisioned_strategy(),
            integration_failing in any::<bool>(),
        ) {
            let i = StatusInputs {
                repo_present, projection, sessions, has_captures, provisioned, integration_failing,
            };
            let premise = repo_present
                && matches!(projection, ProjectionState::Ready | ProjectionState::Building)
                && provisioned != Some(false)
                && sessions != SessionsRead::Corrupt
                && integration_failing;
            prop_assert_eq!(
                derive(&i) == ProjectStatus::Error(StatusError::Integration),
                premise
            );
        }

        /// `Running` never reports a count the session read did not contain,
        /// and never survives an unready projection or an unadopted member —
        /// the badge on screen is the cache's own number or nothing.
        #[test]
        fn running_only_ever_repeats_the_session_reads_own_numbers(
            projection in projection_strategy(),
            sessions in sessions_strategy(),
            has_captures in any::<bool>(),
            provisioned in provisioned_strategy(),
            integration_failing in any::<bool>(),
        ) {
            let i = StatusInputs {
                repo_present: true,
                projection,
                sessions,
                has_captures,
                provisioned,
                integration_failing,
            };
            if let ProjectStatus::Running { since_ms, active_sessions } = derive(&i) {
                prop_assert_eq!(projection, ProjectionState::Ready);
                prop_assert_ne!(provisioned, Some(false));
                prop_assert!(!integration_failing);
                prop_assert_eq!(
                    sessions,
                    SessionsRead::Ok(Some(LiveSessions { active_sessions, since_ms }))
                );
                prop_assert!(active_sessions > 0);
            }
        }
    }
}
