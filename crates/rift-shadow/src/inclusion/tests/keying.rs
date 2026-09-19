//! What `Walk::recorded` keys and what it cannot: one record per path the
//! walk decided about, once per naming where no path exists, and the
//! summary that belongs to no token.

#[cfg(unix)]
use crate::inclusion::{discovered, resolve, Walk};
use crate::inclusion::{
    hard_refusal, Refusal, CAPTURE_DOC_HEADER, INCLUDE_FILE, MAX_ENTRIES, MAX_FILE_BYTES,
};

#[cfg(unix)]
use super::fixtures::doors;
use super::fixtures::{doc, tree, KEYED_TOKENS, UNKEYED_TOKENS};

/// **A summary is written under whatever token the record earned, so the
/// token says neither which summary nor whether either.** The previous
/// version of this test asserted only the `unsafe-path` instance, and the
/// docstring above it read "this token carries BOTH of the module's summary
/// records" — which is a claim about a token that this test could not
/// falsify, in a module whose discipline is that every claim names the test
/// that would fail if it were false. It is false: the path field and the
/// reason field are decided independently, and both callers that can hand
/// [`Walk::refuse`] a summary charge the record cap before the refusal is
/// chosen.
///
/// So the same summary is measured here under three tokens, from three
/// ordinary trees and no permissions trick — `unsafe-path`, `git-path`
/// ([`hard_refusal`] tests `.git` before path safety) and `too-many`
/// ([`Walk::visit`] charges [`Walk::cap_spent`] before [`hard_refusal`]) —
/// plus the unnameable summary under `too-many`, which [`Walk::walk_dir`]
/// reaches by the same charge-first ordering one function over. That last
/// one is asserted at the decision rather than end-to-end because no
/// filesystem on this host will hold the name (APFS: `EILSEQ`), the same
/// posture `a_discovered_name_is_a_path_to_visit_or_a_refusal_to_record`
/// already uses.
///
/// Neither summary is an identity. Two distinct paths of the same length
/// render the identical line, and a repository may legally hold a file
/// named exactly like either summary — which is why the shape is asserted
/// here rather than described, and why the format text withdraws the
/// promise instead of narrowing it to one token.
#[test]
fn a_summary_is_written_under_whatever_token_the_record_earned() {
    let entry = "a\u{7}b.txt";
    let dir = tree(&[], Some(&format!("{entry}\n")));
    assert_eq!(
        doc(dir.path()),
        format!(
            "{CAPTURE_DOC_HEADER}\nx <{} unprintable bytes> unsafe-path\n",
            entry.len()
        ),
        "a control-bearing path is summarised"
    );

    // `.git` is decided before path safety, so the summary is a `git-path`
    // record — an `x` line whose reason is not `unsafe-path` and whose path
    // field is still a byte count.
    let git = ".git/a\u{7}b";
    let dir = tree(&[], Some(&format!("{git}\n")));
    assert_eq!(
        doc(dir.path()),
        format!(
            "{CAPTURE_DOC_HEADER}\nx <{} unprintable bytes> git-path\n",
            git.len()
        ),
        "…under `git-path` when the `.git` screen answered first"
    );

    // And the record cap is charged before the spelling is judged at all,
    // so a control-bearing entry that arrives late is summarised under
    // `too-many`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("q")).unwrap();
    for i in 0..MAX_ENTRIES {
        std::fs::write(dir.path().join(format!("q/f{i:04}.txt")), "x").unwrap();
    }
    let late = "z\u{7}b.txt";
    std::fs::write(dir.path().join(INCLUDE_FILE), format!("q/\n{late}\n")).unwrap();
    let text = doc(dir.path());
    assert!(
        text.ends_with(&format!("x <{} unprintable bytes> too-many\n", late.len())),
        "…and under `too-many` when the cap was spent first: {}",
        text.lines().next_back().unwrap_or_default()
    );

    // The unnameable summary takes the same charge one function over
    // ([`Walk::walk_dir`] runs `cap_spent(&summary)` before refusing it), so
    // it is not an `unsafe-path`-only shape either.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let summary = discovered("d", std::ffi::OsStr::from_bytes(b"na\xff\xfeme")).unwrap_err();
        let mut w = Walk {
            doc: Vec::new(),
            records: MAX_ENTRIES,
            bytes: 0,
            recorded: std::collections::BTreeSet::new(),
            stopped: false,
        };
        assert!(w.cap_spent(&summary), "the cap is spent");
        assert_eq!(
            String::from_utf8(w.doc).unwrap(),
            "x d/<6 unnameable bytes> too-many\n",
            "the unnameable summary is the path field of a `too-many` record"
        );
    }
}

/// The partition `a_keyed_record_always_names_its_path_literally` is stated
/// over, checked because the docstrings call it one: every [`Refusal`]
/// variant lands on exactly one of [`KEYED_TOKENS`] and [`UNKEYED_TOKENS`],
/// so a twelfth variant, or a token moved without being removed, or a rename
/// of a token string, reddens here rather than leaving a sentence that no
/// longer describes the enum. (WHICH side a token is on is asserted text; the
/// exit inventory and `a_path_named_twice_is_captured_once` are what fail if
/// that is wrong.)
#[test]
fn the_keyed_and_unkeyed_tokens_partition_refusal() {
    let all = [
        Refusal::GitPath,
        Refusal::UnsafePath,
        Refusal::SymlinkedParent,
        Refusal::Missing,
        Refusal::NotAFile,
        Refusal::UnsafeLinkTarget,
        Refusal::TooLarge,
        Refusal::Binary,
        Refusal::TooMany,
        Refusal::BudgetSpent,
        Refusal::TooDeep,
    ];
    for why in all {
        let token = why.as_str();
        assert_ne!(
            KEYED_TOKENS.contains(&token),
            UNKEYED_TOKENS.contains(&token),
            "`{token}` is on both sides of the partition, or on neither — \
             add it to exactly one, and to `Walk::recorded`'s enumeration, \
             in the same commit"
        );
    }
    let mut tokens: Vec<&str> = all.iter().map(|w| w.as_str()).collect();
    tokens.sort_unstable();
    let mut listed: Vec<&str> = KEYED_TOKENS
        .iter()
        .chain(&UNKEYED_TOKENS)
        .copied()
        .collect();
    listed.sort_unstable();
    assert_eq!(
        tokens, listed,
        "the two lists must cover `Refusal` exactly: 7 keyed + 4 unkeyed = \
         11 variants, disjoint, no gaps"
    );
}

/// Premise 2 of `a_keyed_record_always_names_its_path_literally`, over the
/// whole `char` space rather than a sample: every character that would make
/// `refuse` summarise is refused before `resolve` runs. The count is asserted
/// so that narrowing either `is_control` call cannot quietly empty the loop.
#[test]
fn every_control_character_is_refused_before_resolve_runs() {
    let controls: Vec<char> = (0u32..=0x0010_FFFF)
        .filter_map(char::from_u32)
        .filter(|c| c.is_control())
        .collect();
    assert_eq!(
        controls.len(),
        65,
        "the Cc block is 0x00–0x1F, 0x7F–0x9F; if this moved, the two \
         `is_control` calls this property rests on are no longer about the \
         same set"
    );
    for c in controls {
        assert_eq!(
            hard_refusal(&format!("a{c}b.txt")),
            Some(Refusal::UnsafePath),
            "U+{:04X} makes `Walk::refuse` summarise, so it must never \
             reach an exit that keys",
            c as u32
        );
    }
}

/// **A keyed record always names its path literally**, and only unkeyed
/// exits can ever carry a summary. This is the test that fails if that
/// stops being true.
///
/// It is the *true* version of a sentence this delta withdrew twice. The
/// withdrawn one gave the summary to a token, which is false — the path
/// field and the reason field are decided independently
/// (`a_summary_is_written_under_whatever_token_the_record_earned`). What is
/// true lines up exactly with [`Walk::recorded`]'s keyed/unkeyed partition,
/// and it is **provable rather than measured**, from three facts each of
/// which already has a test that fails if it is false:
///
/// 1. [`Walk::refuse`] summarises exactly when `rel` holds a control
///    character, and otherwise writes `rel` byte for byte (its body, pinned
///    by the writers screen in
///    `every_refusal_exit_in_the_walk_is_enumerated_and_classified`, and
///    measured in both directions by the tests either side of this one).
/// 2. [`hard_refusal`] refuses **every** control character, and
///    [`Walk::visit`] runs it before [`resolve`]. Asserted over the whole
///    `char` space rather than a sample by
///    `every_control_character_is_refused_before_resolve_runs`.
/// 3. All eleven keyed exits are downstream of that screen — which is what
///    the exit inventory pins, in source order.
///
/// So `refuse`'s summary branch **cannot fire at a keyed exit**.
///
/// 4. **The module's other summary is not `refuse`'s, and the three
///    premises above say nothing about it.** [`discovered`]'s
///    `<parent>/<N unnameable bytes>` arrives at [`Walk::refuse`] *as* the
///    caller's `rel`; it holds no control character, so it is written out
///    literally, and no screen is involved in that at all. What keeps it
///    off a keyed record is where it can travel: [`discovered`] has one
///    call site, its `Err` summary is consumed in that call's own `Err`
///    arm, and that arm holds [`Walk::walk_dir`]'s two exits — **both
///    unkeyed**, and pinned as unkeyed by the exit inventory. It reaches no
///    keyed exit because it reaches no other exit. Argue this half from the
///    control-character premise instead and the argument is simply invalid;
///    it happens to reach a true conclusion.
///
/// So neither summary can appear on a keyed record, for two different
/// reasons: one is screened out of those exits, the other never travels to
/// them. A keyed record's path field is the path the walk decided about,
/// and a reader may treat it as one.
///
/// **The converse is not symmetric, and "the five unkeyed exits are the
/// ones that carry summaries" would be false at one of them.** [`resolve`]'s
/// `Err` arm is unkeyed and *also* downstream of [`hard_refusal`], so it
/// names its path literally too — and since it is the only exit that can
/// write [`Refusal::SymlinkedParent`], that token never appears on a
/// summary either (`no_summary_is_ever_written_under_symlinked_parent`).
/// Four of the five unkeyed exits can: [`Walk::visit`]'s `too-many` and
/// [`hard_refusal`]'s own refusal write `refuse`'s `<N unprintable bytes>`,
/// and [`Walk::walk_dir`]'s two are handed [`discovered`]'s
/// `<parent>/<N unnameable bytes>` as their `rel` already.
/// **By token: `unsafe-path`, `git-path` and `too-many` are the only three
/// of the eleven that can ever appear on a summary record. The other eight
/// — the seven keyed ones and `symlinked-parent` — never can.**
///
/// The document-wide half of this runs as a search rather than as three
/// trees: `a_captured_files_parent_always_resolves_inside_the_work_tree`
/// asserts it per `x` record over a vocabulary that draws a bare control
/// character on purpose.
///
/// The partition the property is stated over is checked by
/// `the_keyed_and_unkeyed_tokens_partition_refusal`, so "a partition of the
/// token space" is a thing a test fails on rather than a sentence in a
/// docstring. This test is the composition at a keyed exit's own shape, end
/// to end: the file is real, readable and regular, so without the screen
/// this entry resolves, `lstat`s clean and reaches `file`'s KEYED capture.
/// With it, the walk never gets there, and the record that names the summary
/// is unkeyed.
#[cfg(unix)]
#[test]
fn a_keyed_record_always_names_its_path_literally() {
    let dir = tempfile::tempdir().unwrap();
    let entry = "a\u{7}b.txt";
    std::fs::write(dir.path().join(entry), "SECRET-CONTENT\n").unwrap();
    std::fs::write(dir.path().join(INCLUDE_FILE), format!("{entry}\n")).unwrap();
    let text = doc(dir.path());
    assert_eq!(
        text,
        format!(
            "{CAPTURE_DOC_HEADER}\nx <{} unprintable bytes> unsafe-path\n",
            entry.len()
        ),
        "the file exists and could be read — the screen, not the \
         filesystem, is what keeps a summary off a keyed exit"
    );
    assert!(!text.contains("SECRET-CONTENT"), "{text}");
}

/// The one unkeyed exit that can never summarise either: `resolve`'s `Err`
/// arm is downstream of the same screen. `in` is a symlinked ancestor, so an
/// ordinary entry under it earns `symlinked-parent` — but a control-bearing
/// one is answered by `hard_refusal` first and never reaches `resolve` at
/// all. That is why `symlinked-parent` is not on the summary-capable list.
#[cfg(unix)]
#[test]
fn no_summary_is_ever_written_under_symlinked_parent() {
    let dir = tempfile::tempdir().unwrap();
    let work = doors(dir.path());
    assert_eq!(
        resolve(&work, "in/a.txt"),
        Err(Refusal::SymlinkedParent),
        "`in` is the symlinked ancestor this exit exists for"
    );
    let entry = "in/a\u{7}b.txt";
    std::fs::write(work.join(INCLUDE_FILE), format!("{entry}\n")).unwrap();
    assert_eq!(
        doc(&work),
        format!(
            "{CAPTURE_DOC_HEADER}\nx <{} unprintable bytes> unsafe-path\n",
            entry.len()
        ),
        "the screen answers before `resolve` does, so no summary is \
         ever written under `symlinked-parent`"
    );
}

/// [`IncludeList::parse`] deduplicates entry *strings*, which does not catch
/// a directory root and a path inside it. `d/` plus `d/a.txt` recorded
/// `f d/a.txt` **twice**, content and all, with both copies counted against
/// the caps — a document that is not a function of the paths named.
///
/// **`an_unreadable_directory_named_twice_is_one_record` is the
/// counterexample this test used to stop one short of**, and it is the reason
/// the first three shapes are not enough on their own: file, link and
/// `too-large` are all *non-directory* decisions, and [`Walk::visit`] keys
/// only those. A directory earns a record of its own at two exits, and
/// neither was keyed — `p` plus an unreadable `p/d` said `x p/d missing`
/// twice. Three shapes agreeing is not the property; the property is over
/// every record a path can earn, so the shape that has no key is the one
/// worth the test.
///
/// **And `a_path_whose_lstat_fails_is_keyed_by_its_refusal` is the one the
/// fourth still stopped short of** — the same sentence, a third exit. Adding
/// the fourth fixed two *directory* exits and generalised the docstring to a
/// property over every record, but the exit that has no key is not "a
/// directory": it is any exit that decides **before** [`Walk::visit`]'s
/// insert, and `visit`'s own `symlink_metadata` failure returns three lines
/// before it. A directory at mode `r--` is the construction that reaches it —
/// readable enough to list the child, not searchable enough to `lstat` it —
/// and `d/` plus `d/x` wrote `x d/x missing` twice. Four shapes agreeing was
/// not the property either, so [`Walk::recorded`] now carries the
/// enumeration of all sixteen refusal exits and the tests below target the
/// ones that key outside `visit`'s insert: two directory exits and one file
/// exit, which is the whole of [`Walk::refuse_once`]'s caller set.
///
/// What is *not* deduplicated has its own test rather than a caveat here:
/// `a_refusal_with_no_path_to_key_on_is_recorded_once_per_naming` and
/// `a_second_case_spelling_is_a_second_path_to_this_document`.
#[test]
fn a_path_named_twice_is_captured_once() {
    let dir = tree(&[("d/a.txt", "CONTENT")], Some("d/\nd/a.txt\n"));
    assert_eq!(
        doc(dir.path()),
        format!("{CAPTURE_DOC_HEADER}\nf d/a.txt\n CONTENT\n"),
        "the walk reached it twice; the document names it once"
    );
}

#[cfg(unix)]
#[test]
fn a_link_named_twice_is_one_link_record() {
    let held = tempfile::tempdir().unwrap();
    let work = held.path().join("w");
    std::fs::create_dir_all(work.join("p")).unwrap();
    std::os::unix::fs::symlink("elsewhere", work.join("p/link")).unwrap();
    std::fs::write(work.join(INCLUDE_FILE), "p/\np/link\n").unwrap();
    let text = doc(&work);
    assert_eq!(
        text.lines().filter(|l| l.starts_with("l ")).count(),
        1,
        "a link reached twice is one link record: {text}"
    );
}

/// A path REFUSED twice is also one record, because the set is charged for
/// every non-directory path the walk decides about rather than only for the
/// ones it captures. That is what makes "one record per path named" true of
/// the whole document instead of only of its captures — the reading
/// `Walk::recorded`'s docstring used to understate.
#[test]
fn a_path_refused_twice_is_one_refusal_record() {
    let refused = tree(&[("d/big.bin", "")], Some("d/\nd/big.bin\n"));
    std::fs::write(
        refused.path().join("d/big.bin"),
        vec![0_u8; usize::try_from(MAX_FILE_BYTES).unwrap() + 1],
    )
    .unwrap();
    assert_eq!(
        doc(refused.path()),
        format!("{CAPTURE_DOC_HEADER}\nx d/big.bin too-large\n"),
        "the walk reached the refusal twice; the document says it once"
    );
}

/// The shape the three tests above stop one short of, and the one that
/// falsified the clause: a **directory** earns a record of its own when it
/// cannot be read, and `visit` inserts into `recorded` only for
/// non-directories. Named twice, `p/d` said `missing` twice.
#[cfg(unix)]
#[test]
fn an_unreadable_directory_named_twice_is_one_record() {
    use std::os::unix::fs::PermissionsExt;

    let held = tempfile::tempdir().unwrap();
    let work = held.path().join("w");
    std::fs::create_dir_all(work.join("p/d")).unwrap();
    std::fs::write(work.join(INCLUDE_FILE), "p/\np/d\n").unwrap();
    std::fs::set_permissions(work.join("p/d"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = std::fs::read_dir(work.join("p/d")).is_err();
    let text = doc(&work);
    std::fs::set_permissions(work.join("p/d"), std::fs::Permissions::from_mode(0o700)).unwrap();
    if unreadable {
        assert_eq!(
            text,
            format!("{CAPTURE_DOC_HEADER}\nx p/d missing\n"),
            "a DIRECTORY reached twice is one record too: {text}"
        );
    } else {
        // Running as root, where mode 000 does not stop `read_dir`.
        assert_eq!(text, format!("{CAPTURE_DOC_HEADER}"), "{text}");
    }
}

/// The fifth shape, and the third exit at which this one property has been
/// reopened. The four above all reach a decision the walk keys — three
/// downstream of `visit`'s own insert, one through `refuse_once` — and the
/// exit that keyed nothing is `visit`'s OWN `symlink_metadata` failure,
/// which returns three lines BEFORE that insert. A directory at mode `r--`
/// is the construction that separates the two syscalls the shape needs:
/// `read_dir` lists the child (read permission), and `lstat` on the child
/// fails (no search permission), so the path is decided about without ever
/// being keyed. Named twice, `d/x` said `missing` twice — two records, one
/// path, two charges against `MAX_ENTRIES`.
#[cfg(unix)]
#[test]
fn a_path_whose_lstat_fails_is_keyed_by_its_refusal() {
    use std::os::unix::fs::PermissionsExt;

    let held = tempfile::tempdir().unwrap();
    let work = held.path().join("w");
    std::fs::create_dir_all(work.join("d")).unwrap();
    std::fs::write(work.join("d/x"), "CONTENT").unwrap();
    std::fs::write(work.join(INCLUDE_FILE), "d/\nd/x\n").unwrap();
    std::fs::set_permissions(work.join("d"), std::fs::Permissions::from_mode(0o400)).unwrap();
    let unsearchable = std::fs::symlink_metadata(work.join("d/x")).is_err();
    let text = doc(&work);
    std::fs::set_permissions(work.join("d"), std::fs::Permissions::from_mode(0o700)).unwrap();
    if unsearchable {
        assert_eq!(
            text,
            format!("{CAPTURE_DOC_HEADER}\nx d/x missing\n"),
            "a path whose `lstat` fails is keyed by its refusal like any \
             other path the walk decided about: {text}"
        );
    } else {
        // Running as root, where a missing execute bit does not stop
        // `lstat` — so the file is simply captured, once.
        assert_eq!(
            text,
            format!("{CAPTURE_DOC_HEADER}\nf d/x\n CONTENT\n"),
            "{text}"
        );
    }
}

/// **The exceptions, pinned rather than asserted away.** The identity this
/// module deduplicates on is the resolved path, and it has exactly one — so
/// a record that never had a path cannot be keyed at all, and is written
/// once per naming rather than once per path.
///
/// Two shapes reach it and both are recorded here so the format text can
/// state them instead of promising a set. A refusal decided **before a
/// resolved path exists** ([`hard_refusal`], and [`resolve`]'s own `Err`)
/// has nothing to key on, so an entry the list types *and* the walk
/// discovers earns two identical lines. And the
/// `<parent>/<N unnameable bytes>` summary is never a path by construction
/// — that is what it is *for* — so a directory named twice re-summarises
/// the same child.
///
/// [`resolve`]'s `Err` covers a **fourth** unkeyed token, `symlinked-parent`,
/// which the filesystem decides rather than the spelling. It is enumerated
/// here for completeness and not asserted below, because it cannot
/// duplicate by construction: the walk descends only real directories, so a
/// *discovered* path never has a symlinked ancestor, and two typed spellings
/// of one entry are deduplicated as strings by `IncludeList::parse`. That is
/// an argument, not a measurement — which is exactly why it is written here
/// rather than asserted as a guarantee.
///
/// A **fifth** unkeyed token completes the list [`Walk::recorded`]
/// enumerates: `too-many`, at both callers of [`Walk::cap_spent`] —
/// [`Walk::visit`] charges the cap before [`resolve`] has produced a path,
/// and [`Walk::walk_dir`] charges it on the unnameable summary. Also not
/// asserted below, and for a reason that *is* structural rather than an
/// argument about shapes: [`Walk::exhausted`] sets `stopped`, and both
/// loops test it, so one document holds at most one `too-many` record and
/// there is no second naming to record. Listed anyway, because a token that
/// cannot duplicate is still a token this set does not key, and treating
/// those two as the same claim is what
/// `a_path_whose_lstat_fails_is_keyed_by_its_refusal` re-taught this module.
///
/// Inventing a second key (the entry string) would close both, and is the
/// move this module has been rejected for three times: a check written in a
/// different vocabulary than the operation it guards. The entry string is
/// not what the walk opens, and a set keyed on it would answer a different
/// question than the one `recorded` exists to answer.
#[test]
fn a_refusal_with_no_path_to_key_on_is_recorded_once_per_naming() {
    let typed_and_discovered = tree(&[("d/.git/config", "[core]")], Some("d/\nd/.git\n"));
    let text = doc(typed_and_discovered.path());
    assert_eq!(
        text.matches("x d/.git git-path").count(),
        2,
        "a spelling refusal is decided before `resolve` runs, so there is \
         no path to key it on and the walk records it once per naming: \
         {text}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let held = tempfile::tempdir().unwrap();
        let work = held.path().join("w");
        std::fs::create_dir_all(work.join("p/d")).unwrap();
        std::fs::write(work.join(INCLUDE_FILE), "p/\np/d\n").unwrap();
        let raw: &[u8] = b"na\xff\xfeme";
        let hostile = work.join("p/d").join(std::ffi::OsStr::from_bytes(raw));
        if std::fs::write(&hostile, "HOSTILE-BYTES").is_ok() {
            let text = doc(&work);
            assert_eq!(
                text.matches("x p/d/<6 unnameable bytes> unsafe-path")
                    .count(),
                2,
                "and a summary that is not a path cannot be keyed either: \
                 {text}"
            );
        }
        // macOS: APFS returns EILSEQ. The Linux leg adjudicates this half.
    }
}

/// **A path is a spelling, not an inode**, and the document never claims
/// otherwise in either direction — which is the same fact
/// `a_hardlink_is_captured_under_the_name_it_was_given` records from the
/// other side.
///
/// On a case-insensitive filesystem — the macOS default, and what this
/// repository's own volumes are — `A.txt` and `a.txt` are one file and two
/// paths, so the document holds two records and two copies of the content,
/// and charges both against both caps for one inode. The caps still bound
/// the document; they are spent, not defeated.
///
/// It is recorded rather than closed. Closing it means folding case, and a
/// fold this module computes will disagree with the filesystem's on some
/// input — HFS+ froze Unicode 3.2, APFS did not, and a volume may be
/// case-sensitive at all — which is precisely the shape that broke this
/// module three times: a check written in a different vocabulary than the
/// operation it guards. The alternative key, the inode, is worse: it would
/// silently drop the hardlink record the module deliberately keeps.
///
/// Adjudicated on whichever leg the filesystem decides; the case-sensitive
/// arm below is the control, not a pass.
#[test]
fn a_second_case_spelling_is_a_second_path_to_this_document() {
    let dir = tree(&[("A.txt", "CONTENT")], Some("A.txt\na.txt\n"));
    let text = doc(dir.path());
    if std::fs::symlink_metadata(dir.path().join("a.txt")).is_ok() {
        assert_eq!(
            text,
            format!("{CAPTURE_DOC_HEADER}\nf A.txt\n CONTENT\nf a.txt\n CONTENT\n"),
            "one file, two spellings, two records and two copies: {text}"
        );
    } else {
        assert_eq!(
            text,
            format!("{CAPTURE_DOC_HEADER}\nf A.txt\n CONTENT\nx a.txt missing\n"),
            "case-sensitive control: the second spelling is another path \
             and there is nothing at it: {text}"
        );
    }
}
