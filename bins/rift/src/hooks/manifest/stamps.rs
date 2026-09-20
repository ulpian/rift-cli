//! The stamps that let a quiet turn stay silent without opening anything.

use std::path::PathBuf;

use super::baseline::{baseline_dir, baseline_ordinals};

// Most turns have nothing to announce, and discovering that was the single
// largest cost on this path: opening the projection and asking `diff_since` at
// all costs **+205 ms wall against this arm's own 279 ms control**, paid on
// every prompt, including the ones whose honest answer is silence. An
// independent re-run of the same harness on a differently loaded machine got
// +131 ms against a 173 ms control — a different absolute, the same ~0.75x of
// the control, which is why the figure is quoted with the control beside it
// and never alone. Absolute milliseconds here are not machine-portable; the
// ratio is what reproduces.
//
// An earlier version of this comment said +1084 ms, from the 2026-08-06 run
// whose arms were compared against a control from a *different* fixture with a
// different capture history. That number was retracted the same day and is
// wrong by 5-8x. It is named here rather than silently replaced, because it
// reached an owner-facing file and the correction should travel with it.
//
// It is avoidable because the answer is a function of exactly two things — the
// recall projection and this session's baseline — and both are files. If
// neither has moved since the last turn that accounted for them, the delta is
// the delta that turn already absorbed, so the answer is silence and it can be
// given without opening anything.
//
// Failure direction, chosen deliberately: a stamp that cannot be read, or that
// differs for any reason, falls through to the full path. Only an exact match
// skips. So the worst a wrong stamp can do is cost a turn the work it was
// already doing; it can never make Rift *assert* something it did not check.

/// The most bytes a stamp file is allowed to spend of a turn. The stamps this
/// writes are well under a hundred; the cap is there because `.rift/state/`
/// sits on a filesystem the agent can write to, and "read this file" on the
/// critical path must have a bound that does not depend on who wrote it.
const MAX_STAMP_BYTES: u64 = 256;

/// The three levels a recorded stamp carries: the cheap file
/// fingerprint of the store, this session's baseline, and the fingerprint of
/// what this member's scope admits.
///
/// A stamp that does not parse — a stamp from before the scoped level existed,
/// or one an agent wrote into `.rift/state/` by hand — is simply not a match,
/// and the turn falls through to the full path. That is the same failure
/// direction the whole mechanism has: the worst a wrong stamp can do is cost a
/// turn the work it was already doing.
pub(super) struct TurnStamp<'a> {
    pub(super) projection: &'a str,
    pub(super) baseline: &'a str,
    pub(super) scoped: &'a str,
}

impl<'a> TurnStamp<'a> {
    pub(super) fn parse(raw: &'a str) -> Option<Self> {
        let mut parts = raw.split('|');
        let (projection, baseline, scoped) = (parts.next()?, parts.next()?, parts.next()?);
        parts.next().is_none().then_some(Self {
            projection,
            baseline,
            scoped,
        })
    }
}

/// A cheap fingerprint of the recall projection: `recall.db` and its
/// write-ahead log.
///
/// `None` means the projection could not even be located, which is the one
/// state that must never be treated as "unchanged" — the full path would fail
/// here too, and failing to *ask* is different from asking and hearing nothing.
///
/// The `-wal` file is stamped as well as the database. Today `rift-store` opens
/// in the default rollback-journal mode, where every commit moves the main
/// file's length or mtime, so the first component alone would do. But a future
/// `journal_mode=WAL` would land committed writes in the log and leave the main
/// file untouched for a while — silently turning this precondition from
/// conservative into wrong. One extra `stat` buys immunity from that.
pub(in crate::hooks) fn projection_stamp(repo: &std::path::Path) -> Option<String> {
    let db = crate::family::recall_db(repo).ok()?;
    let wal = {
        let mut p = db.clone().into_os_string();
        p.push("-wal");
        PathBuf::from(p)
    };
    Some(format!("{},{}", file_stamp(&db), file_stamp(&wal)))
}

/// The fingerprint of this session's baseline: the newest ordinal and that
/// file's own stamp, or `-` when the session has no baseline yet.
///
/// The ordinal is included because it is the part that cannot be forged by
/// accident: [`write_baseline`](super::baseline::write_baseline) always creates `max + 1`, so every legitimate
/// baseline write moves this string. A hostile actor who deleted the baseline
/// and recreated it with the same ordinal, length and mtime would buy exactly
/// one missed manifest — a saving not taken, never a claim not checked.
pub(super) fn baseline_stamp(repo: &std::path::Path, session_key: &str) -> String {
    baseline_ordinals(&baseline_dir(repo, session_key))
        .pop()
        .map_or_else(
            || "-".to_owned(),
            |(ord, path)| format!("{ord}:{}", file_stamp(&path)),
        )
}

/// The stamp this session last accounted for, bounded by [`MAX_STAMP_BYTES`].
pub(super) fn read_stamp(repo: &std::path::Path, session_key: &str) -> Option<String> {
    use std::io::Read as _;
    let mut s = String::new();
    std::fs::File::open(stamp_path(repo, session_key))
        .ok()?
        .take(MAX_STAMP_BYTES)
        .read_to_string(&mut s)
        .ok()?;
    Some(s)
}

/// Record that this session has accounted for `projection` — everything the
/// projection said at that stamp is either in the baseline or was announced.
///
/// The two halves are captured at different moments, and that asymmetry is the
/// whole safety argument. `projection` is read **before** the query, so any
/// write that lands while the query runs leaves the stored stamp stale and the
/// next turn re-checks: a delta can be re-examined, never swallowed. The
/// baseline half is read **here**, after this turn's own write, because this
/// function is the only thing that writes it — there is no race to lose.
pub(super) fn record_stamp(
    repo: &std::path::Path,
    session_key: &str,
    projection: Option<&str>,
    scoped: Option<&str>,
) {
    let (Some(projection), Some(scoped)) = (projection, scoped) else {
        return;
    };
    let stamp = format!(
        "{projection}|{}|{scoped}",
        baseline_stamp(repo, session_key)
    );
    let _ = std::fs::write(stamp_path(repo, session_key), stamp);
}

/// Where one session's projection stamp lives — beside its baselines, because
/// it is the same kind of thing: local run state whose loss costs one extra
/// turn of work and no correctness at all.
///
/// The name deliberately has no `.json` suffix, so [`baseline_ordinals`] (which
/// admits only `<ordinal>.json`) can never mistake it for a baseline.
pub(super) fn stamp_path(repo: &std::path::Path, session_key: &str) -> PathBuf {
    baseline_dir(repo, session_key).join("projection.stamp")
}

/// `<len>:<mtime-nanos>` for one file, or `-` when it is not there.
fn file_stamp(path: &std::path::Path) -> String {
    let Ok(md) = std::fs::metadata(path) else {
        return "-".to_owned();
    };
    let nanos = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{}:{nanos}", md.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::agent::{drop_projection_store, AsRoot};
    use crate::hooks::audit::append_memd_audit;
    use crate::hooks::manifest::baseline::write_baseline;
    use crate::hooks::manifest::fixtures::{
        arm_turn_stamp, fixture_repo_with_recall_on, seed_sibling_checkpoint,
        seed_two_read_revisions, stamp_as_it_stands, FIXTURE_HEAD,
    };
    use crate::hooks::manifest::turn_manifest;

    /// The silence precondition, stated where it can actually fail.
    ///
    /// A timing assertion would be a flake; this is the *behavioural* form of
    /// the same claim. The fixture is rigged so that consulting the projection
    /// and not consulting it give **different** answers — an empty baseline
    /// against a seeded projection is a delta that injects. If the turn is
    /// silent, the projection was not consulted; and the converse run, with
    /// only the stamp removed, proves the fixture really would have injected.
    #[test]
    fn an_unmoved_projection_is_answered_without_opening_it() {
        let repo = fixture_repo_with_recall_on();
        let db = seed_two_read_revisions(repo.path());
        let key = crate::window_journal::key("s-skip", None);

        // An empty baseline: everything in the projection is a delta.
        write_baseline(repo.path(), &key, &[]);
        // …and a stamp saying this exact state was already accounted for,
        // which is what a previous silent turn leaves behind.
        arm_turn_stamp(repo.path(), &key);

        assert_eq!(
            turn_manifest(repo.path(), "s-skip", None, None, &Vec::new, &|_| {}),
            None,
            "an unmoved projection must be answered with silence, not read"
        );

        // The converse, on the same fixture: remove only the stamp.
        std::fs::remove_file(stamp_path(repo.path(), &key)).unwrap();
        assert!(
            turn_manifest(repo.path(), "s-skip", None, None, &Vec::new, &|_| {}).is_some(),
            "without the stamp the very same state injects — so the silence \
             above was the precondition, not an empty delta"
        );

        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop_projection_store(repo.as_root());
    }

    /// The precondition is over *both* inputs. A baseline this turn did not
    /// write is a changed question, even against a projection that has not
    /// moved, so it can never be skipped over.
    #[test]
    fn a_baseline_the_turn_did_not_write_is_never_skipped_over() {
        let repo = fixture_repo_with_recall_on();
        let db = seed_two_read_revisions(repo.path());
        let key = crate::window_journal::key("s-baseline", None);

        write_baseline(repo.path(), &key, &[]);
        arm_turn_stamp(repo.path(), &key);
        assert_eq!(
            turn_manifest(repo.path(), "s-baseline", None, None, &Vec::new, &|_| {}),
            None,
            "armed"
        );

        // Same projection, a new baseline file: the question changed.
        write_baseline(repo.path(), &key, &[]);
        assert!(
            turn_manifest(repo.path(), "s-baseline", None, None, &Vec::new, &|_| {}).is_some(),
            "a moved baseline re-opens the projection"
        );

        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop_projection_store(repo.as_root());
    }

    /// …and a projection that moved for a reason of its own (here: someone
    /// else's audit row) retires the cheap level and re-opens the store. This
    /// is the direction that keeps the precondition conservative — the file
    /// stamp is a claim about bytes, and any byte that moved retires it.
    ///
    /// What the reopened turn then *says* is the second level's business
    /// and here it says nothing, correctly: an audit row is not
    /// a checkpoint, so this member's scope did not move. The evidence that
    /// level 1 really was retired is that the recorded stamp comes back
    /// re-armed against the projection's NEW bytes — which the cheap path,
    /// having returned before opening anything, could not have written.
    #[test]
    fn a_moved_projection_is_never_skipped_over() {
        let repo = fixture_repo_with_recall_on();
        let db = seed_two_read_revisions(repo.path());
        let key = crate::window_journal::key("s-moved", None);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        write_baseline(repo.path(), &key, &[]);
        arm_turn_stamp(repo.path(), &key);
        assert_eq!(
            turn_manifest(repo.path(), "s-moved", None, None, &Vec::new, &|_| {}),
            None,
            "armed"
        );

        {
            let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
            rt.block_on(append_memd_audit(
                &store,
                rift_recall_serve::injection_audit_envelope(7, "elsewhere").as_bytes(),
                1,
            ))
            .unwrap();
        }
        let armed_at = read_stamp(repo.path(), &key).unwrap();
        assert_eq!(
            turn_manifest(repo.path(), "s-moved", None, None, &Vec::new, &|_| {}),
            None
        );
        let rearmed = read_stamp(repo.path(), &key).unwrap();
        assert_ne!(
            rearmed, armed_at,
            "a projection whose bytes moved must be read again"
        );
        assert_eq!(rearmed, stamp_as_it_stands(repo.path(), &key));

        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop_projection_store(repo.as_root());
    }

    /// Silence is what the precondition has to *record* to be worth anything:
    /// the first turn that finds nothing pays for the discovery, and every
    /// turn after it on the same state does not.
    #[test]
    fn a_silent_turn_records_the_stamp_that_makes_the_next_one_free() {
        let repo = fixture_repo_with_recall_on();
        let db = seed_two_read_revisions(repo.path());
        let key = crate::window_journal::key("s-silent", None);

        // A baseline that already covers both seeded revisions: a real delta
        // query that comes back empty, which is the Silent branch.
        let covered = vec![format!("{:064x}", 0), format!("{:064x}", 1)];
        write_baseline(repo.path(), &key, &covered);
        assert!(
            !stamp_path(repo.path(), &key).exists(),
            "nothing recorded yet"
        );

        assert_eq!(
            turn_manifest(repo.path(), "s-silent", None, None, &Vec::new, &|_| {}),
            None,
            "no delta"
        );
        let expected = stamp_as_it_stands(repo.path(), &key);
        assert_eq!(
            read_stamp(repo.path(), &key).as_deref(),
            Some(expected.as_str()),
            "the silent turn must record what it accounted for"
        );

        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop_projection_store(repo.as_root());
    }

    /// The price the docstring names, made falsifiable.
    ///
    /// `turn_manifest` reads the projection stamp BEFORE the query, so an
    /// injecting turn — whose own audit row moves `recall.db` afterwards —
    /// necessarily records a stamp that no longer matches. The comment at that
    /// call site says an injecting turn costs one extra full walk after it;
    /// this is the test that would fail if that stopped being true, in either
    /// direction: stale after an inject, matching after the silent turn that
    /// follows.
    #[test]
    fn an_injecting_turn_leaves_a_stamp_that_makes_the_next_turn_walk() {
        let repo = fixture_repo_with_recall_on();
        let db = seed_two_read_revisions(repo.path());
        let key = crate::window_journal::key("s-stale", None);
        let current = |k: &str| stamp_as_it_stands(repo.path(), k);

        write_baseline(repo.path(), &key, &[]);
        assert!(
            turn_manifest(repo.path(), "s-stale", None, None, &Vec::new, &|_| {}).is_some(),
            "the fixture must inject"
        );
        assert_ne!(
            read_stamp(repo.path(), &key).as_deref(),
            Some(current(&key).as_str()),
            "an injecting turn's own audit row moves the projection under it"
        );

        // …so the next turn does the walk, finds the delta absorbed, and
        // records the stamp that finally matches.
        assert_eq!(
            turn_manifest(repo.path(), "s-stale", None, None, &Vec::new, &|_| {}),
            None
        );
        assert_eq!(
            read_stamp(repo.path(), &key).as_deref(),
            Some(current(&key).as_str()),
            "the turn after an inject is what establishes the steady state"
        );

        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop_projection_store(repo.as_root());
    }

    /// The double-billing hazard, closed.
    ///
    /// One family, one store. A sibling member working on a divergent branch
    /// lands checkpoints all day; this member is quiet. The file stamp moves
    /// every time — so the cheap level retires every time — and if that were
    /// the only level, every one of those writes would re-open the walk and
    /// spend this member's context on another member's work.
    ///
    /// Rigged the way its level-1 sibling above is, and for the same reason:
    /// the baseline is empty, so a turn that *did* consult the projection would
    /// inject. If the turn is silent, the scoped stamp decided it; and the
    /// converse run, with only the stamp removed, proves the fixture really
    /// would have injected.
    #[test]
    fn a_quiet_member_is_not_reinjected_by_a_divergent_siblings_write() {
        let repo = fixture_repo_with_recall_on();
        let db = seed_two_read_revisions(repo.path());
        let key = crate::window_journal::key("s-quiet", None);

        write_baseline(repo.path(), &key, &[]);
        arm_turn_stamp(repo.path(), &key);
        assert_eq!(
            turn_manifest(repo.path(), "s-quiet", None, None, &Vec::new, &|_| {}),
            None,
            "armed"
        );
        let armed = read_stamp(repo.path(), &key).unwrap();

        // The sibling lands a checkpoint anchored on a DIVERGENT commit.
        let divergent = "2222222222222222222222222222222222222222";
        seed_sibling_checkpoint(repo.path(), "sib", 9, divergent, &format!("{:064x}", 42));

        // The cheap level really is retired — the file genuinely moved …
        assert_ne!(
            projection_stamp(repo.path()).unwrap(),
            TurnStamp::parse(&armed).unwrap().projection,
            "the fixture must actually move the store, or it proves nothing"
        );
        // … and the answer is still silence, because nothing moved for THIS
        // member.
        assert_eq!(
            turn_manifest(repo.path(), "s-quiet", None, None, &Vec::new, &|_| {}),
            None
        );
        assert_eq!(
            read_stamp(repo.path(), &key).unwrap(),
            stamp_as_it_stands(repo.path(), &key),
            "and level 1 is re-armed against the bytes as they now stand"
        );

        // The converse, on the same fixture: remove only the stamp.
        std::fs::remove_file(stamp_path(repo.path(), &key)).unwrap();
        assert!(
            turn_manifest(repo.path(), "s-quiet", None, None, &Vec::new, &|_| {}).is_some(),
            "without the stamp the very same state injects — so the silence \
             above was the scoped level, not an empty delta"
        );

        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop_projection_store(repo.as_root());
    }

    /// The ancestry twin, and the reason the pair exists: containment is not a
    /// mute button. A sibling landing facts on THIS member's line moves the
    /// scoped stamp, the walk runs, and the delta is announced — which is the
    /// compounding a shared store is for. The same rigged fixture, one commit
    /// different.
    #[test]
    fn a_siblings_write_on_the_members_ancestry_does_reinject() {
        let repo = fixture_repo_with_recall_on();
        let db = seed_two_read_revisions(repo.path());
        let key = crate::window_journal::key("s-compound", None);

        write_baseline(repo.path(), &key, &[]);
        arm_turn_stamp(repo.path(), &key);
        assert_eq!(
            turn_manifest(repo.path(), "s-compound", None, None, &Vec::new, &|_| {}),
            None,
            "armed"
        );

        // The sibling's checkpoint is anchored at this member's own HEAD.
        seed_sibling_checkpoint(repo.path(), "sib", 9, FIXTURE_HEAD, &format!("{:064x}", 42));

        let text = turn_manifest(repo.path(), "s-compound", None, None, &Vec::new, &|_| {})
            .expect("a sibling on this member's ancestry is this member's delta");
        assert!(text.contains("crates/sib/src/lib.rs"), "{text}");

        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop_projection_store(repo.as_root());
    }

    /// A stamp file is untrusted local state on a path the agent can write to.
    /// It gets a byte bound, and anything that is not an exact match falls
    /// through to the full walk — the only direction that cannot invent an
    /// answer.
    #[test]
    fn a_hostile_stamp_file_costs_work_never_correctness() {
        let repo = fixture_repo_with_recall_on();
        let db = seed_two_read_revisions(repo.path());
        let key = crate::window_journal::key("s-hostile", None);
        write_baseline(repo.path(), &key, &[]);

        let real = format!(
            "{}|{}",
            projection_stamp(repo.path()).unwrap(),
            baseline_stamp(repo.path(), &key)
        );
        // The real stamp padded past the cap: the bounded read cannot return
        // it, so it cannot match, so the turn does its work.
        let padded = format!("{real}{}", "x".repeat(4096));
        std::fs::write(stamp_path(repo.path(), &key), &padded).unwrap();
        let read_back = read_stamp(repo.path(), &key).unwrap();
        assert!(
            read_back.len() <= usize::try_from(MAX_STAMP_BYTES).unwrap(),
            "the read is bounded: {} bytes",
            read_back.len()
        );
        assert!(
            turn_manifest(repo.path(), "s-hostile", None, None, &Vec::new, &|_| {}).is_some(),
            "an unmatchable stamp falls through to the real answer"
        );

        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop_projection_store(repo.as_root());
    }
}
