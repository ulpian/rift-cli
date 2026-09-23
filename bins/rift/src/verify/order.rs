//! The 1:1 check between the checkpoint branch and `refs/rift/chain/v1`.
//!
//! Separate because it reads git alone — no projection, no shard artifacts —
//! and it is what says whether the recorded order is complete before anything
//! is compared against it.

use std::path::Path;

use super::verdict::Verdict;

/// **The 1:1 check.** Every checkpoint on the branch has exactly one position in
/// `refs/rift/chain/v1`, and every position names a checkpoint on the branch.
///
/// Three findings, each with its own diagnostic, because they have three
/// different causes and three different answers:
///
/// 1. **the orders disagree** — both sources have the checkpoint and put it in
///    different places. One of two append-only histories was rewritten, so the
///    diagnostic says which position each source claims. Tamper.
/// 2. **an orphan entry** — a position names a checkpoint the branch does not
///    carry. The branch was rewritten, or this chain is another repository's.
///    Nothing can heal it. Tamper.
/// 3. **an un-chained checkpoint** — a shard with no position. The order has
///    not caught up: nothing was rewritten, and the next archive heals it (or
///    `rift chain init`, on a repository that never had a chain). Structure —
///    incomplete, not falsified.
///
/// Checked in that order, because a disagreement makes the other two counts
/// meaningless.
pub(super) fn chain_completeness(work_tree: &Path) -> Verdict {
    use rift_git::chain;
    use rift_shadow::backend::NATIVE_CHECKPOINT_REF as CHECKPOINT_BRANCH;

    let Ok(repo) = rift_git::discover(work_tree) else {
        return Verdict::Ok; // not a repository: there is no order to check
    };
    let git_dir = &repo.git_dir;
    let branch = match chain::branch_order(git_dir, CHECKPOINT_BRANCH) {
        Ok(b) => b,
        // A non-linear checkpoint branch has no single write order to check
        // against. Reported, never linearized around.
        Err(e) => return Verdict::Structure(format!("the checkpoint branch: {e}")),
    };
    let order = match chain::read_order(git_dir) {
        Ok(o) => o,
        Err(e) => return Verdict::Structure(format!("the chain ref: {e}")),
    };

    // Multiplicity before membership: `orphan_entries`/`missing_entries` are
    // set filters and the zip below compares positions, so a checkpoint
    // recorded at TWO chain positions is invisible to all three when every id
    // is also present once on the branch — an adversarial review planted
    // exactly that (branch of 1, ref of 2, verify said OK). The product cannot write the state
    // (`record_archived` no-ops on a recorded id; `init_from_branch` refuses
    // an existing ref), but verify is the tamper gate and a duplicate entry
    // is the mirror of the fork this ref exists to prevent.
    {
        let mut seen = std::collections::HashSet::new();
        for id in &order {
            if !seen.insert(id) {
                return Verdict::Tamper(format!(
                    "{id}: the chain records this checkpoint at more than one position"
                ));
            }
        }
    }

    if let Some(d) = crate::chain::first_disagreement(&order, &branch) {
        return Verdict::Tamper(format!(
            "{}: the chain and the checkpoint branch disagree — {d}",
            d.branch
        ));
    }
    match chain::orphan_entries(git_dir, &branch) {
        Ok(orphans) if !orphans.is_empty() => {
            return Verdict::Tamper(format!(
                "{}: {} chain entr{} name a checkpoint that is not on the branch ({})",
                orphans.first().map_or("", String::as_str),
                orphans.len(),
                if orphans.len() == 1 { "y" } else { "ies" },
                name_some(&orphans)
            ))
        }
        Ok(_) => {}
        Err(e) => return Verdict::Structure(format!("the chain ref: {e}")),
    }
    match chain::missing_entries(git_dir, &branch) {
        Ok(missing) if !missing.is_empty() => Verdict::Structure(format!(
            "{}: {} checkpoint(s) on the branch have no chain entry ({}) — run `rift chain init` \
             if this repository has never recorded its order",
            missing.first().map_or("", String::as_str),
            missing.len(),
            name_some(&missing)
        )),
        Ok(_) => Verdict::Ok,
        Err(e) => Verdict::Structure(format!("the chain ref: {e}")),
    }
}
/// Name at most three ids out of `ids`, with the count.
fn name_some(ids: &[String]) -> String {
    let shown: Vec<&str> = ids.iter().take(3).map(String::as_str).collect();
    if ids.len() > shown.len() {
        format!("{} (+{} more)", shown.join(", "), ids.len() - shown.len())
    } else {
        shown.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the 1:1 check, over real git ---------------------------------------

    /// Write order A, B; content-hash order A < B, so a sort agrees here and
    /// the tests below turn on presence rather than order — except the third,
    /// which uses MID (sorting between them) to make the disagreement concrete.
    const CK_A: &str = "4c5c0a053e73b924663009e1602882b6073d3fdd10ad33aa92c988a643e040f3";

    const CK_B: &str = "b970a014c1a3dd6508c5382679051a37f7cc1cd238b66543ecb1472a746d23c7";

    const CK_MID: &str = "93ae336492a162b429cf27a5952d8d8f0a7a878eca243585d5bc33893d7d0d9a";

    /// A checked git call with a fixed identity, answering its trimmed
    /// stdout. Every git the fixtures run goes through here: an unchecked
    /// call that fails transiently (a lock held by a parallel test, a
    /// machine with no git identity) leaves the fixture half-built and the
    /// test asserting about a state it never made.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_AUTHOR_DATE", "@1700000001 +0000")
            .env("GIT_COMMITTER_DATE", "@1700000001 +0000")
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// One shard, one commit, on the checkpoint branch.
    fn archive_shard(dir: &Path, id: &str) {
        let (hi, rest) = id.split_at(2);
        let shard = dir.join(hi).join(rest);
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join("metadata.json"), r#"{"strategy":"x"}"#).unwrap();
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-qm", id]);
    }

    fn checkpoint_repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git_in(d.path(), &["init", "-q", "-b", "rift/checkpoints/v1"]);
        d
    }

    #[test]
    fn a_one_to_one_chain_is_clean() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        for id in [CK_A, CK_B] {
            archive_shard(d.path(), id);
            rift_git::chain::append(&git_dir, id, 1_700_000_000).unwrap();
        }
        assert_eq!(chain_completeness(d.path()), Verdict::Ok);
    }

    /// Direction one: a checkpoint on the branch with no entry. Incomplete, not
    /// falsified — and it names the checkpoint and the count.
    #[test]
    fn an_unchained_checkpoint_is_reported_by_id_and_count() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        archive_shard(d.path(), CK_A);
        rift_git::chain::append(&git_dir, CK_A, 1_700_000_000).unwrap();
        archive_shard(d.path(), CK_B); // its entry never landed

        let Verdict::Structure(m) = chain_completeness(d.path()) else {
            panic!("an un-chained checkpoint is incomplete, not tamper")
        };
        assert!(m.contains(CK_B), "names the checkpoint: {m}");
        assert!(m.contains("1 checkpoint(s)"), "and the count: {m}");
        assert!(m.contains("no chain entry"), "{m}");
    }

    /// An adversarial review's plant, now a permanent guard: a checkpoint recorded at TWO
    /// chain positions while the branch carries it once. The set filters and
    /// the position zip are all blind to multiplicity when every id is
    /// present — this reddened nothing until the duplicate screen landed, and
    /// verify said OK over a duplicated history. Calibrated: with the screen
    /// reverted, this test panics at its Tamper match.
    #[test]
    fn a_checkpoint_recorded_at_two_positions_is_tamper() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        archive_shard(d.path(), CK_A);
        rift_git::chain::append(&git_dir, CK_A, 1_700_000_000).unwrap();
        // The product cannot write this state (`record_archived` no-ops on a
        // recorded id; `init_from_branch` refuses an existing ref) — build it
        // the way an adversary would, with git itself.
        // Checked calls, with an identity: unchecked, a transient failure left
        // the duplicate unwritten and the test reported "not detected".
        let head = git_in(d.path(), &["rev-parse", "refs/rift/chain/v1"]);
        let dup = git_in(
            d.path(),
            &[
                "commit-tree",
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
                "-p",
                &head,
                "-m",
                &format!("rift: chain\n\nRift-Chain-Checkpoint: {CK_A}"),
            ],
        );
        git_in(d.path(), &["update-ref", "refs/rift/chain/v1", &dup]);

        let Verdict::Tamper(m) = chain_completeness(d.path()) else {
            panic!("a duplicated chain entry is a tamper finding")
        };
        assert!(m.contains(CK_A), "names the duplicated checkpoint: {m}");
        assert!(m.contains("more than one position"), "{m}");
    }

    /// Direction two, which the first cannot see: an entry naming a checkpoint
    /// the branch does not carry. Its own function, its own message, and a
    /// tamper verdict — the branch was rewritten or the chain is not this
    /// repository's.
    #[test]
    fn an_orphan_entry_gets_its_own_diagnostic() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        archive_shard(d.path(), CK_A);
        rift_git::chain::append(&git_dir, CK_A, 1_700_000_000).unwrap();
        rift_git::chain::append(&git_dir, CK_B, 1_700_000_000).unwrap(); // never archived

        let Verdict::Tamper(m) = chain_completeness(d.path()) else {
            panic!("an entry naming nothing on the branch is a tamper finding")
        };
        assert!(m.contains(CK_B), "names the entry's checkpoint: {m}");
        assert!(m.contains("not on the branch"), "{m}");
        assert!(
            !m.contains("no chain entry"),
            "not the other diagnostic: {m}"
        );
    }

    /// Both present, positions disagree: say which position EACH source claims.
    #[test]
    fn orders_that_disagree_say_what_each_source_claims_where() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        for id in [CK_A, CK_B, CK_MID] {
            archive_shard(d.path(), id);
        }
        // The chain records A, MID, B — the order a content-hash sort produces.
        for id in [CK_A, CK_MID, CK_B] {
            rift_git::chain::append(&git_dir, id, 1_700_000_000).unwrap();
        }
        let Verdict::Tamper(m) = chain_completeness(d.path()) else {
            panic!("two histories disagreeing about a position is tamper")
        };
        assert!(m.contains("position 2"), "the position: {m}");
        assert!(m.contains(CK_MID), "what the chain claims there: {m}");
        assert!(m.contains(CK_B), "what the branch claims there: {m}");
    }
}
