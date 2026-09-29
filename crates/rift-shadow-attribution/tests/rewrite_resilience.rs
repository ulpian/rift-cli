//! The rewrite fixtures, and what git actually does to a
//! `Rift-Checkpoint` trailer when a commit's sha moves.
//!
//! This file establishes the ground truth the resolution logic is written
//! against. Every claim here is a measurement of real `git` output, not a
//! restatement of an assumption:
//!
//! * the fixtures are reproducible (same seed ⇒ same oids, twice, in two
//!   different temp roots) — without that, nothing else here is about git;
//! * `rebase`, `cherry-pick` and `amend` change the sha and leave the trailer
//!   byte-identical (the legacy backend's "this trailer survives git amend and rebase"
//!   claim, verified rather than inherited);
//! * a squash arrives in **two different shapes**, and one of them indents its
//!   trailers — the fact that decides whether a line-anchored parser can see
//!   them at all.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rift_shadow_attribution::provenance::{
    parse_checkpoint_claims, select_for_file, CheckpointCandidate, SelectionBasis,
};
use support::{
    all, amend, cherry_pick, rebase, rebase_interactive_squash, squash_forge, squash_merge,
    squash_merge_message_replaced, Builder, GitFixture,
};

/// Count occurrences of a checkpoint id in a message.
fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn every_scenario_is_reproducible_from_its_seed() {
    let first = all(0x5EED_B004);
    let second = all(0x5EED_B004);
    assert_eq!(first.len(), second.len());
    for ((_, a), (_, b)) in first.iter().zip(second.iter()) {
        assert_eq!(a.name, b.name);
        // The oid is the strong claim: it folds tree bytes, parent, identity and
        // both timestamps. Equal oids across two temp roots means the hermetic
        // environment really did isolate the build from this machine.
        assert_eq!(
            a.head, b.head,
            "{}: head oid must not depend on where or when the fixture was built",
            a.name
        );
        assert_eq!(a.message, b.message, "{}: message drifted", a.name);
        assert_eq!(a.expected, b.expected, "{}: checkpoint ids drifted", a.name);
        assert_eq!(
            a.before.iter().map(|c| &c.oid).collect::<Vec<_>>(),
            b.before.iter().map(|c| &c.oid).collect::<Vec<_>>(),
            "{}: pre-rewrite oids drifted",
            a.name
        );
    }
}

#[test]
fn different_seeds_produce_different_repositories() {
    // Guards the reproducibility test above from being vacuous: if the generator
    // ignored its seed, "same seed ⇒ same oids" would hold trivially.
    let a = all(0x1111_1111);
    let b = all(0x2222_2222);
    for ((_, x), (_, y)) in a.iter().zip(b.iter()) {
        assert_ne!(x.head, y.head, "{}: seed must reach the content", x.name);
    }
}

#[test]
fn a_rewrite_moves_the_sha_and_keeps_the_trailer() {
    for (build, name) in [
        (rebase as Builder, "rebase"),
        (cherry_pick, "cherry_pick"),
        (amend, "amend"),
    ] {
        let mut fx = GitFixture::new(0xC0FF_EE01);
        let rw = build(&mut fx);
        assert_eq!(rw.name, name);
        // The tip of what was rewritten becomes the new HEAD.
        let tip = &rw.before[rw.before.len() - 1];
        assert_ne!(
            rw.head, tip.oid,
            "{name}: the rewrite must actually move the sha, or this proves nothing",
        );
        assert_eq!(
            occurrences(&rw.message, &tip.checkpoint),
            1,
            "{name}: HEAD carries the original checkpoint id exactly once:\n{}",
            rw.message
        );
        // Byte-identical, not merely present: the id is content-addressed over
        // the session's redacted capture and its read facts — never over the
        // anchoring commit — so a moved sha cannot perturb it.
        assert_eq!(
            fx.message_of(&rw.head),
            rw.message,
            "{name}: message read back differs from the one recorded",
        );
    }
}

#[test]
fn a_rebase_rewrites_every_commit_on_the_branch() {
    let mut fx = GitFixture::new(0xC0FF_EE02);
    let rw = rebase(&mut fx);
    // Two commits were rebased; both shas moved, and both trailers are still
    // reachable — one on HEAD, one on HEAD~1.
    let head_msg = fx.message_of("HEAD");
    let parent_msg = fx.message_of("HEAD~1");
    assert_eq!(occurrences(&head_msg, &rw.expected[1]), 1);
    assert_eq!(occurrences(&parent_msg, &rw.expected[0]), 1);
    let log = fx.git_read(&["log", "--format=%H", "-n", "2"]);
    for line in log.lines() {
        assert!(
            !rw.before.iter().any(|c| c.oid == line),
            "rebase must have rewritten every commit it replayed",
        );
    }
}

#[test]
fn a_squash_merge_indents_its_constituent_trailers() {
    let mut fx = GitFixture::new(0xC0FF_EE03);
    let rw = squash_merge(&mut fx);
    assert_eq!(rw.expected.len(), 3);
    for id in &rw.expected {
        assert_eq!(
            occurrences(&rw.message, id),
            1,
            "squash_merge: every constituent checkpoint survives into the message",
        );
    }
    // The load-bearing observation. `git merge --squash` writes SQUASH_MSG as an
    // indented transcript of the squashed commits, so each trailer line begins
    // with whitespace — a parser that anchors `Rift-Checkpoint` at column 0 sees
    // NONE of them.
    let indented = rw
        .message
        .lines()
        .filter(|l| l.trim_start().starts_with("Rift-Checkpoint:"))
        .filter(|l| l.starts_with(char::is_whitespace))
        .count();
    assert_eq!(
        indented, 3,
        "expected three indented trailer lines, message was:\n{}",
        rw.message
    );
    assert!(
        !rw.message
            .lines()
            .any(|l| l.starts_with("Rift-Checkpoint:")),
        "squash_merge carries no column-0 trailer at all",
    );
}

#[test]
fn a_forge_squash_stacks_unindented_trailers() {
    let mut fx = GitFixture::new(0xC0FF_EE04);
    let rw = squash_forge(&mut fx);
    assert_eq!(rw.expected.len(), 2);
    let flush = rw
        .message
        .lines()
        .filter(|l| l.starts_with("Rift-Checkpoint:"))
        .count();
    assert_eq!(
        flush, 2,
        "a forge squash stacks several column-0 trailers in one message:\n{}",
        rw.message
    );
    for id in &rw.expected {
        assert_eq!(occurrences(&rw.message, id), 1);
    }
}

#[test]
fn an_interactive_rebase_squash_stacks_unindented_trailers_locally() {
    // The correction. `squash_forge` composes its message by hand, so on its own
    // it can only restate what its author believed a forge does — and the
    // unindented, oldest-first shape looked like something only a forge lands.
    // It is not: `git rebase -i` with `squash` produces exactly that shape with
    // local git and nothing else, which is a far commoner event than a forge
    // merge (every developer who tidies a branch before pushing).
    let mut fx = GitFixture::new(0xC0FF_EE0B);
    let rw = rebase_interactive_squash(&mut fx);
    assert_eq!(rw.expected.len(), 3);
    let flush = rw
        .message
        .lines()
        .filter(|l| l.starts_with("Rift-Checkpoint:"))
        .count();
    assert_eq!(
        flush, 3,
        "an interactive squash stacks column-0 trailers:\n{}",
        rw.message
    );
    assert!(
        !rw.message
            .lines()
            .any(|l| l.starts_with(char::is_whitespace) && l.trim_start().starts_with("Rift-")),
        "and indents none of them, unlike `git merge --squash`:\n{}",
        rw.message
    );
    // Every constituent survives, exactly once, and the fused sha is new.
    assert!(!rw.before.iter().any(|c| c.oid == rw.head));
    for id in &rw.expected {
        assert_eq!(occurrences(&rw.message, id), 1, "{}", rw.message);
    }
    let set = parse_checkpoint_claims(&rw.message);
    assert_eq!(
        set.ids(),
        rw.expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "oldest-first, i.e. creation order:\n{}",
        rw.message
    );
    assert_eq!(set.rejected, 0);
}

#[test]
fn squashing_with_an_explicit_message_discards_every_claim() {
    // The third shape, and the honest limit of trailer-carried provenance:
    // `git merge --squash` writes `.git/SQUASH_MSG`, but `git commit -m`
    // REPLACES it rather than seeding the editor with it. The fused commit then
    // claims nothing, and no reader can recover what it fused — there is no
    // trace to parse, so this is not something the scanner can be made
    // cleverer about. Recording it as a test rather than a comment because a
    // limit nobody has measured tends to get rediscovered as a bug.
    let mut fx = GitFixture::new(0xC0FF_EE0C);
    let rw = squash_merge_message_replaced(&mut fx);
    assert_eq!(rw.expected.len(), 2);
    for id in &rw.expected {
        assert_eq!(
            occurrences(&rw.message, id),
            0,
            "the -m message replaced SQUASH_MSG, so nothing survives:\n{}",
            rw.message
        );
    }
    let set = parse_checkpoint_claims(&rw.message);
    assert!(set.claims.is_empty(), "{set:?}");
    // Nothing is *rejected* either: this is silence, not a malformed trailer,
    // so a caller cannot distinguish it from a genuinely human commit. That is
    // the whole cost of the loss, and it belongs on the write side.
    assert_eq!(set.rejected, 0);
    // The constituents are still reachable on their own branch — the loss is in
    // the fused commit's message, not in the object database.
    let branch = fx.message_of("replaced");
    assert!(branch.contains(&rw.expected[1]), "{branch}");
}

#[test]
fn the_root_commit_carries_no_rift_trailer() {
    // Every scenario starts from history that predates Rift: a resolver must
    // handle "no trailer" as an ordinary answer, not an error.
    let fx = GitFixture::new(0xC0FF_EE05);
    let msg = fx.message_of("HEAD");
    assert!(!msg.contains("Rift-Checkpoint"), "{msg}");
}

#[test]
fn every_rewrite_leaves_every_checkpoint_claimed_somewhere_in_the_history() {
    // The survival property, stated over the whole reachable history rather than
    // over HEAD: a rewrite may move a claim to a different commit (rebase keeps
    // one commit per checkpoint) or fuse several onto one (squash), but it must
    // never drop one.
    for (fx, rw) in all(0x5EED_B005) {
        let log = fx.git_read(&["log", "--format=%B"]);
        let set = parse_checkpoint_claims(&log);
        let mut got: Vec<String> = set.ids().into_iter().map(str::to_owned).collect();
        let mut want = rw.expected.clone();
        got.sort();
        want.sort();
        assert_eq!(got, want, "{}: a rewrite lost a checkpoint claim", rw.name);
        assert_eq!(set.rejected, 0, "{}: refused a trailer it wrote", rw.name);
        assert!(!set.truncated, "{}", rw.name);
    }
}

#[test]
fn a_fused_commit_carries_every_constituent() {
    for build in [squash_merge as Builder, squash_forge] {
        let mut fx = GitFixture::new(0xC0FF_EE06);
        let rw = build(&mut fx);
        let set = parse_checkpoint_claims(&rw.message);
        let mut got: Vec<String> = set.ids().into_iter().map(str::to_owned).collect();
        let mut want = rw.expected.clone();
        got.sort();
        want.sort();
        assert_eq!(
            got, want,
            "{}: the fused commit must resolve ALL constituents",
            rw.name
        );
        assert!(set.is_multi(), "{}", rw.name);
        assert_eq!(set.rejected, 0, "{}", rw.name);
    }
}

#[test]
fn trailer_order_in_a_squash_does_not_mean_chronological_order() {
    // The finding that decides the selection rule. Both of these are squashes
    // carrying every constituent trailer, and their trailer orders are OPPOSITE:
    //
    //   * `git merge --squash` renders `git log` output, which is newest-first,
    //     so the FIRST trailer belongs to the LAST commit written;
    //   * a forge's squash-and-merge lists bullets oldest-first, so the first
    //     trailer belongs to the FIRST commit written.
    //
    // A resolver that takes "the first trailer" therefore means two opposite
    // things depending on how the squash was made — which is why Rift orders
    // candidates by created-at and keeps trailer order only as
    // a tie-break.
    //
    // Both halves are measured against real git. The oldest-first half used to
    // rest solely on `squash_forge`, whose message this file composes by hand —
    // which could only restate the belief, never establish it. `git rebase -i`
    // with `squash` produces the same oldest-first shape locally, so the third
    // block below is the one that makes the finding a measurement.
    let mut fx = GitFixture::new(0xC0FF_EE08);
    let local = squash_merge(&mut fx);
    let local_ids = parse_checkpoint_claims(&local.message);
    let mut reversed = local.expected.clone();
    reversed.reverse();
    assert_eq!(
        local_ids.ids(),
        reversed.iter().map(String::as_str).collect::<Vec<_>>(),
        "git merge --squash lists its constituents newest-first:\n{}",
        local.message
    );

    let mut fx = GitFixture::new(0xC0FF_EE09);
    let forge = squash_forge(&mut fx);
    let forge_ids = parse_checkpoint_claims(&forge.message);
    assert_eq!(
        forge_ids.ids(),
        forge
            .expected
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        "a forge squash lists its constituents oldest-first:\n{}",
        forge.message
    );

    let mut fx = GitFixture::new(0xC0FF_EE0D);
    let tidied = rebase_interactive_squash(&mut fx);
    let tidied_ids = parse_checkpoint_claims(&tidied.message);
    assert_eq!(
        tidied_ids.ids(),
        tidied
            .expected
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        "and so does a LOCAL interactive-rebase squash — same repository, same \
         git, opposite order to `git merge --squash`:\n{}",
        tidied.message
    );
    assert_ne!(
        local_ids.ids().first(),
        Some(&local.expected[0].as_str()),
        "the two local squashes really do disagree about which trailer is first",
    );
}

#[test]
fn a_squash_selects_the_checkpoint_that_touched_the_file() {
    let mut fx = GitFixture::new(0xC0FF_EE07);
    let rw = squash_merge(&mut fx);
    let set = parse_checkpoint_claims(&rw.message);
    // What a resolver would find in the store: each constituent commit touched
    // exactly one file, and created-at follows creation order — which, in this
    // message, is the REVERSE of trailer order. So a rule that used trailer
    // order would answer differently, and this test would catch it.
    let files = ["src/parse.rs", "src/lex.rs", "src/emit.rs"];
    let cands: Vec<CheckpointCandidate> = set
        .claims
        .iter()
        .map(|c| {
            let born = rw
                .expected
                .iter()
                .position(|e| *e == c.checkpoint_id)
                .expect("every claim is one of the constituents");
            CheckpointCandidate {
                checkpoint_id: c.checkpoint_id.clone(),
                ordinal: c.ordinal,
                created_at_ms: Some(100 * (i64::try_from(born).unwrap() + 1)),
                files_touched: vec![files[born].to_owned()],
            }
        })
        .collect();
    for (i, file) in files.iter().enumerate() {
        let sel = select_for_file(&cands, file).expect("a squash always has candidates");
        assert_eq!(sel.checkpoint_id, rw.expected[i], "{file}");
        assert_eq!(sel.basis, SelectionBasis::FileTouched);
        assert_eq!(sel.considered, 3);
    }
}

#[test]
fn git_interpret_trailers_cannot_do_this_job() {
    // The obvious "just use git" alternative, measured so nobody reaches for it
    // as a simplification later. `git interpret-trailers` reads the final
    // paragraph only, so on a locally squashed commit it emits NOTHING, and on a
    // forge squash it emits only the LAST of several checkpoints.
    for (build, want_lines) in [(squash_merge as Builder, 0usize), (squash_forge, 1)] {
        let mut fx = GitFixture::new(0xC0FF_EE0A);
        let rw = build(&mut fx);
        let path = fx.repo().join("msg.txt");
        std::fs::write(&path, &rw.message).expect("write message");
        let parsed = fx.git_read(&["interpret-trailers", "--parse", &path.display().to_string()]);
        let got: Vec<&str> = parsed
            .lines()
            .filter(|l| l.starts_with("Rift-Checkpoint:"))
            .collect();
        assert_eq!(
            got.len(),
            want_lines,
            "{}: interpret-trailers returned {got:?} for\n{}",
            rw.name,
            rw.message
        );
        // Rift's own scan recovers every one of them from the same bytes.
        assert_eq!(
            parse_checkpoint_claims(&rw.message).claims.len(),
            rw.expected.len(),
            "{}",
            rw.name
        );
        std::fs::remove_file(&path).expect("remove message");
    }
}

/// Every git override this fixture knows about is either **removed** or pinned
/// to a **value**, and the difference is not cosmetic.
///
/// `GIT_ATTR_NOSYSTEM` shipped in the scrub list, which is backwards: removing
/// it re-enables `$(prefix)/etc/gitattributes`, so the "hermetic" fixture was
/// the one that consulted the machine's system attributes file. The protective
/// state is `1`. `GIT_CONFIG_COUNT=0` is likewise a value rather than an
/// absence: its `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` pairs are unbounded in
/// number and cannot be enumerated in a scrub list, but git reads only pairs
/// `0..count`, so zero makes every one of them inert.
#[test]
fn the_fixture_environment_pins_every_git_override_this_module_knows_of() {
    use std::{collections::HashMap, path::Path};

    let fx = GitFixture::new(0x0E_0E_0E);
    let cmd = fx.command("git");
    let env: HashMap<String, Option<String>> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();

    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_COMMON_DIR",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_TEMPLATE_DIR",
        "GIT_EXTERNAL_DIFF",
        "GIT_SSH_COMMAND",
        "GIT_DEFAULT_HASH",
    ] {
        assert_eq!(
            env.get(key),
            Some(&None),
            "{key} must be removed from the child environment"
        );
    }
    for (key, want) in [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_ATTR_NOSYSTEM", "1"),
        ("GIT_CONFIG_COUNT", "0"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ] {
        assert_eq!(
            env.get(key).and_then(Option::as_deref),
            Some(want),
            "{key} must be pinned to {want:?}, not removed"
        );
    }
    // `core.excludesFile`/`core.attributesFile` fall back to
    // `$XDG_CONFIG_HOME/git/*`, which no `GIT_CONFIG_*` variable redirects, so
    // both roots must land inside this fixture's own temp tree.
    for key in ["HOME", "XDG_CONFIG_HOME"] {
        let got = env.get(key).and_then(Option::as_deref).unwrap_or_default();
        assert!(
            Path::new(got).join("gitconfig").is_file(),
            "{key} = {got:?} is not this fixture's root"
        );
    }
}

/// A hostile environment cannot reach inside a fixture — demonstrated against a
/// git that really executes the hook first, so the guard cannot pass vacuously.
///
/// Two vectors, both of which the old fixture was open to: `GIT_CONFIG_COUNT` +
/// `GIT_CONFIG_KEY_0`/`VALUE_0` (which outrank *every* config file, so pointing
/// `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` at a stub does nothing about them),
/// and `GIT_TEMPLATE_DIR` (whose `hooks/` are copied by `git init`).
///
/// Unix-only, and only because the demonstration needs an executable hook: the
/// variables themselves are read identically on Windows, and the environment
/// table above is checked on every platform.
#[cfg(unix)]
#[test]
fn a_hostile_inherited_environment_cannot_reach_inside_a_fixture() {
    use std::path::Path;

    let evil = tempfile::tempdir().expect("evil root");
    let hooks = fixture_hostile_hooks_dir(evil.path());

    let by_config: [(&str, &str); 3] = [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "core.hooksPath"),
        ("GIT_CONFIG_VALUE_0", hooks.to_str().expect("hooks path")),
    ];
    let by_template: [(&str, &str); 1] =
        [("GIT_TEMPLATE_DIR", evil.path().to_str().expect("evil path"))];

    for (what, poison) in [
        ("GIT_CONFIG_COUNT", &by_config[..]),
        ("GIT_TEMPLATE_DIR", &by_template[..]),
    ] {
        assert!(
            legacy_commit(poison).contains(MARKER),
            "{what} does not reproduce on this git, so the guard below would be \
             vacuous — do not weaken the guard, find out why"
        );
    }

    // The same environment, against the fixture: every vector planted at once.
    let both: Vec<(&str, &str)> = by_config
        .iter()
        .chain(by_template.iter())
        .copied()
        .collect();
    let mut poisoned = GitFixture::poisoned(0x5EED, &both);
    let with_poison = squash_merge(&mut poisoned);
    assert!(
        !poisoned
            .git_read(&["log", "--all", "--format=%B"])
            .contains(MARKER),
        "a hostile hook fired inside a fixture"
    );
    assert!(
        !Path::new(&poisoned.repo().join(".git/hooks/commit-msg")).exists(),
        "git init copied a hook out of a hostile template"
    );

    // Stronger than "no marker": the poisoned build is byte-identical to the
    // clean one, so the hostile environment changed nothing at all.
    let mut clean = GitFixture::new(0x5EED);
    let without = squash_merge(&mut clean);
    assert_eq!(with_poison.head, without.head);
    assert_eq!(with_poison.message, without.message);
}

/// The marker the hostile hook appends to every commit message it sees.
#[cfg(unix)]
const MARKER: &str = "Injected-By-A-Hostile-Hook";

/// A `hooks/` directory under `evil` holding an executable `commit-msg` hook
/// that appends [`MARKER`] — the payload both vectors try to smuggle in, one
/// as `core.hooksPath` and one as the `hooks/` of a template directory.
#[cfg(unix)]
fn fixture_hostile_hooks_dir(evil: &std::path::Path) -> std::path::PathBuf {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    let hooks = evil.join("hooks");
    fs::create_dir_all(&hooks).expect("evil hooks dir");
    let hook = hooks.join("commit-msg");
    fs::write(
        &hook,
        format!("#!/bin/sh\nprintf '%s\\n' '{MARKER}' >> \"$1\"\n"),
    )
    .expect("write hook");
    let mut perms = fs::metadata(&hook).expect("hook meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&hook, perms).expect("chmod hook");
    hooks
}

/// The arm that shows each vector working. What stays *legacy* about it is
/// the stub config file — the protection this fixture used to have, which is
/// exactly what these two vectors walk past.
///
/// It gets the fixture's isolation as well, with the vector planted back on
/// top afterwards. Without that it was the **second** git in this file
/// running with the ambient environment (the first being the one in
/// `a_hostile_default_hash_cannot_change_what_a_fixture_builds`, isolated one
/// commit earlier): a developer — or another agent's hook — carrying a
/// hostile `GIT_TEMPLATE_DIR` would have had hooks executed by its `init` and
/// `commit` too, in the file whose whole subject is that vector. The
/// `contains(MARKER)` assertion in the test is what keeps the isolation honest:
/// if the scrub swallowed the planted vector, they go red rather than quiet.
#[cfg(unix)]
fn legacy_commit(poison: &[(&str, &str)]) -> String {
    use std::{fs, process::Command};

    let home = tempfile::tempdir().expect("legacy root");
    let stub = home.path().join("gitconfig");
    fs::write(&stub, "[core]\n\tautocrlf = false\n").expect("stub config");
    let work = home.path().join("repo");
    fs::create_dir_all(&work).expect("legacy repo dir");
    let run = |args: &[&str]| {
        let mut c = Command::new("git");
        c.current_dir(&work);
        support::hermetic_env(&mut c, home.path());
        for (k, v) in [
            ("GIT_CONFIG_GLOBAL", stub.to_str().expect("stub path")),
            ("GIT_CONFIG_SYSTEM", stub.to_str().expect("stub path")),
            ("GIT_AUTHOR_NAME", "Legacy"),
            ("GIT_AUTHOR_EMAIL", "legacy@rift.invalid"),
            ("GIT_COMMITTER_NAME", "Legacy"),
            ("GIT_COMMITTER_EMAIL", "legacy@rift.invalid"),
            ("GIT_AUTHOR_DATE", "1735689600 +0000"),
            ("GIT_COMMITTER_DATE", "1735689600 +0000"),
        ] {
            c.env(k, v);
        }
        // The vector, planted back after the scrub that removes it — so
        // this arm demonstrates the poison it was handed and nothing else.
        for (k, v) in poison {
            c.env(k, v);
        }
        c.args(args).output().expect("spawn git")
    };
    assert!(run(&["init", "-q"]).status.success(), "legacy init");
    fs::write(work.join("a.txt"), "x\n").expect("legacy file");
    assert!(run(&["add", "-A"]).status.success(), "legacy add");
    assert!(
        run(&["commit", "-q", "-m", "subject"]).status.success(),
        "legacy commit"
    );
    String::from_utf8_lossy(&run(&["log", "-1", "--format=%B"]).stdout).into_owned()
}

/// `GIT_DEFAULT_HASH` is the vector a config scrub cannot reach: it is not
/// config at all, and it changes the repository's **format** rather than its
/// content, so every oid a fixture records becomes 64 characters instead of 40.
///
/// Demonstrated before it is guarded, the same way the hook vectors above are:
/// a repository built without an explicit `--object-format` under
/// `GIT_DEFAULT_HASH=sha256` really does mint sha256 oids on this git. The
/// fixture states its format on `git init` and scrubs the variable, so a
/// poisoned build is byte-identical to a clean one — not merely 40 characters
/// wide, which a coincidence could also produce.
///
/// **Which of the two guards this test attributes to:** neither, and that is
/// deliberate. Measured — removing both turns it red (64 against 40); removing
/// only the `--object-format` pin leaves it green, because the scrub alone
/// defeats the variable today. The pin defends a case no test here can stage,
/// git changing its own default hash, so it is redundancy on purpose rather
/// than a second thing this test proves.
#[test]
fn a_hostile_default_hash_cannot_change_what_a_fixture_builds() {
    use std::{fs, process::Command};

    let legacy = tempfile::tempdir().expect("legacy root");
    // This repository is built outside a fixture, on purpose — it is the arm
    // that shows the vector working. That made it **one of two** gits in this
    // file running with the ambient environment — the other is the
    // `legacy_commit` helper of
    // `a_hostile_inherited_environment_cannot_reach_inside_a_fixture`, which
    // redirected the two config *files* and scrubbed nothing, and is isolated
    // the same way as of the commit that corrected this sentence. In the file
    // about hostile environments, a developer (or another agent's hook)
    // carrying a hostile `GIT_TEMPLATE_DIR` would have had hooks executed by
    // both of their `init`s and `commit`s. Each gets the fixture's isolation,
    // and then its own vector planted back deliberately, after the scrub that
    // removes it.
    let run = |args: &[&str]| {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(legacy.path());
        support::hermetic_env(&mut cmd, legacy.path());
        cmd.env("GIT_DEFAULT_HASH", "sha256")
            .env("GIT_AUTHOR_NAME", "Legacy")
            .env("GIT_AUTHOR_EMAIL", "legacy@rift.invalid")
            .env("GIT_COMMITTER_NAME", "Legacy")
            .env("GIT_COMMITTER_EMAIL", "legacy@rift.invalid")
            .output()
            .expect("spawn git")
    };
    assert!(run(&["init", "-q"]).status.success(), "legacy init");
    fs::write(legacy.path().join("a.txt"), "x\n").expect("legacy file");
    assert!(run(&["add", "-A"]).status.success(), "legacy add");
    assert!(
        run(&["commit", "-q", "-m", "subject"]).status.success(),
        "legacy commit"
    );
    let legacy_head = String::from_utf8_lossy(&run(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(
        legacy_head.len(),
        64,
        "GIT_DEFAULT_HASH does not reproduce on this git, so the guard below \
         would be vacuous — do not weaken the guard, find out why"
    );

    let mut poisoned = GitFixture::poisoned(0x5EED, &[("GIT_DEFAULT_HASH", "sha256")]);
    let with_poison = squash_merge(&mut poisoned);
    let mut clean = GitFixture::new(0x5EED);
    let without = squash_merge(&mut clean);
    assert_eq!(with_poison.head.len(), 40, "the fixture stays sha1");
    assert_eq!(with_poison.head, without.head);
    assert_eq!(with_poison.message, without.message);
}
