//! The properties, and the reachability tests that keep each one from
//! asserting over nothing.

use proptest::prelude::*;

use crate::inclusion::{capture_document, hard_refusal, resolve, Refusal, INCLUDE_FILE};

use super::fixtures::{doors, KEYED_TOKENS};

/// Does this `x` record's path field stand in for a path instead of naming
/// one? Both of the module's summaries end the same way — [`Walk::refuse`]'s
/// `<N unprintable bytes>` and [`discovered`]'s `<parent>/<N unnameable
/// bytes>`.
///
/// This is a **shape**, not an identity, exactly as the summaries
/// themselves are: a repository may legally hold a file named
/// `<7 unprintable bytes>`, and it would be captured as an `f` record that
/// no assertion here reads.
///
/// **Where that stops being harmless is an `x` record**, and it is worth
/// knowing before the next vocabulary edit rather than after it. Both
/// callers ask the same question — the search in
/// `a_captured_files_parent_always_resolves_inside_the_work_tree` and the
/// sweep in `the_summary_search_sees_the_records_it_asserts_about` — and it
/// is that no record under a keyed token is a summary. So a real file
/// legitimately *named* like one and refused under a keyed token (say
/// `<7 unnameable bytes>` earning `missing`) would trip that assertion as a
/// **false positive**. Nothing in that vocabulary produces one today — the
/// sweep walks it exhaustively and counts zero — and adding one is a
/// two-line experiment: `<7 unnameable bytes>` as a leaf reddens the sweep
/// deterministically and the property randomly. If either ever reports a
/// record that is both, the sweep's message names this paragraph as one of
/// the two things it can be.
fn is_summary(path: &str) -> bool {
    path.ends_with(" unprintable bytes>") || path.ends_with(" unnameable bytes>")
}

/// The draw behind `captured_records_only_ever_name_accepted_paths`, named
/// so the property and the counter that proves it non-vacuous cannot use
/// different ones.
///
/// Two thirds arbitrary spelling (the original alphabet, which is what
/// makes the property a statement about *whatever the include file says*)
/// and one third paths that name something real in [`doors`] — the two
/// captured files, the two directories above them, the symlinks that pass
/// the screen, and `.git/config`, which must not be captured. Without the
/// second branch every entry is `missing` and the property's assertions
/// never run, which is exactly what happened.
fn converse_draw() -> proptest::strategy::BoxedStrategy<Vec<String>> {
    proptest::collection::vec(
        prop_oneof![
            2 => "[a-zA-Z.\\\\/]{1,12}",
            1 => proptest::sample::select(vec![
                "real/a.txt", "real/sub/a.txt", "real", "real/", "real/sub",
                "cfg", "out", "g", "in", "deeper",
                "bs\\dir", "g\\h", ".git/config", "a.txt:s",
            ]).prop_map(str::to_owned),
        ],
        0..6,
    )
    .boxed()
}

/// The reachability half of the property above, as a test rather than as
/// something a verifier has to go and measure. It drives the same strategy
/// and counts what the assertions get to read; a future edit that empties
/// the tree, narrows the alphabet or drops the real-path branch turns the
/// property hollow again and fails **here**, loudly, instead of passing
/// silently for twenty thousand draws.
#[test]
fn the_converse_property_sees_the_records_it_asserts_about() {
    use proptest::strategy::Strategy as _;
    use proptest::test_runner::TestRunner;

    let strategy = converse_draw();
    let mut runner = TestRunner::deterministic();
    let (mut files, mut links, mut refusals) = (0usize, 0usize, 0usize);
    for _ in 0..200 {
        let entries = strategy.new_tree(&mut runner).unwrap().current();
        let dir = tempfile::tempdir().unwrap();
        let work = doors(dir.path());
        std::fs::write(work.join(INCLUDE_FILE), entries.join("\n")).unwrap();
        let text = String::from_utf8(capture_document(&work).as_slice().to_vec()).unwrap();
        for line in text.lines() {
            match line.as_bytes().first() {
                Some(b'f') => files += 1,
                Some(b'l') => links += 1,
                Some(b'x') => refusals += 1,
                _ => {}
            }
        }
    }
    assert!(
        files > 0,
        "the `f` assertion never runs: {files}/{links}/{refusals}"
    );
    assert!(
        refusals > 0,
        "…and the arbitrary half of the draw stopped reaching refusals: \
         {files}/{links}/{refusals}"
    );
    // Symlinks exist only on the Unix legs (see `doors`), so only there can
    // the `l` assertion have anything to read.
    #[cfg(unix)]
    assert!(
        links > 0,
        "the `l` assertion never runs: {files}/{links}/{refusals}"
    );
}

/// The vocabulary
/// `a_captured_files_parent_always_resolves_inside_the_work_tree` draws
/// from, lifted out of the draw so the sweep below reads the same list.
/// Ancestors: every door in [`doors`], real directories, and the segments
/// Win32 resolves to traversal.
const SUMMARY_ANCESTORS: [&str; 17] = [
    "cfg", "out", "in", "deeper", "g", "deep", "bs\\dir", "g\\h", "x\\y", "real", "sub", "..", ".",
    ".. ", ". ", "...", ".git",
];

/// Leaves: what an escape would fetch, what an ordinary capture holds, and
/// what would forge a record if the framing leaked.
const SUMMARY_LEAVES: [&str; 11] = [
    "id_rsa",
    "secret.txt",
    "notes.md",
    "config",
    "a.txt",
    "sub",
    ".git",
    "..",
    "one\nf /etc/passwd",
    "bell\u{7}rung",
    "f spaced.txt",
];

/// The reachability half of the summary search, in the shape
/// `the_converse_property_sees_the_records_it_asserts_about` already uses
/// next door — and it is here because the numbers that stood in for it were
/// **not constants**. "525 `x` records, 177 keyed, 52 summaries, 0 both"
/// was one run of a randomly seeded proptest; a verifier's run of the same
/// tree gave 548/173/42. Only the last of the four is a property, so only
/// the last is asserted as one, and the rest become a question a test
/// answers rather than a figure a reader has to trust: does the vocabulary
/// still reach both kinds of record the search reads?
///
/// The sweep is exhaustive rather than sampled — every ancestor × every
/// leaf × both joiners, in chunks well under [`MAX_ENTRIES`] so no chunk
/// ends early — so a vocabulary edit that stops producing keyed records, or
/// stops producing summaries, reddens here deterministically instead of
/// leaving the property next door asserting over nothing.
///
/// The `both` count is the same property the search asserts per record,
/// repeated here for its *message*: a record that is keyed and
/// summary-shaped is either the real defect or the false positive
/// [`is_summary`] warns about, and the reader needs to be told which two
/// things to look at.
#[test]
fn the_summary_search_sees_the_records_it_asserts_about() {
    let mut entries: Vec<String> = Vec::new();
    for ancestor in SUMMARY_ANCESTORS {
        for leaf in SUMMARY_LEAVES {
            for sep in ["/", "\\"] {
                entries.push(format!("{ancestor}{sep}{leaf}"));
            }
        }
    }
    let (mut keyed, mut summaries, mut both) = (0usize, 0usize, 0usize);
    for chunk in entries.chunks(32) {
        let dir = tempfile::tempdir().unwrap();
        let work = doors(dir.path());
        std::fs::write(work.join(INCLUDE_FILE), chunk.join("\n")).unwrap();
        let text = String::from_utf8(capture_document(&work).as_slice().to_vec()).unwrap();
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("x ") else {
                continue;
            };
            let (path, token) = rest.rsplit_once(' ').unwrap_or((rest, ""));
            let is_keyed = KEYED_TOKENS.contains(&token);
            keyed += usize::from(is_keyed);
            summaries += usize::from(is_summary(path));
            both += usize::from(is_keyed && is_summary(path));
        }
    }
    assert!(
        keyed > 0,
        "the keyed half of the summary search reads nothing: no entry in \
         this vocabulary earns a keyed refusal any more ({keyed} keyed, \
         {summaries} summaries)"
    );
    assert!(
        summaries > 0,
        "the summary half of the search reads nothing: no entry in this \
         vocabulary makes a writer summarise a path any more ({keyed} \
         keyed, {summaries} summaries)"
    );
    assert_eq!(
        both, 0,
        "a record is both keyed and summary-shaped. Either a keyed exit \
         summarised its path — which \
         `a_keyed_record_always_names_its_path_literally` proves cannot \
         happen, so start there — or this vocabulary now holds a real name \
         that ends like a summary, which is the false positive \
         `is_summary` documents"
    );
}

/// One entry of the summary search as (ancestors, leaf, joiner).
type SummaryEntry = (Vec<&'static str>, &'static str, bool);

/// The draw behind `a_captured_files_parent_always_resolves_inside_the_work_tree`:
/// up to three [`SUMMARY_ANCESTORS`] over one of [`SUMMARY_LEAVES`], joined by
/// `/` or `\`. SHAPED, not uniform — drawn as (ancestors, leaf, joiner) so the
/// *shape* of the escape class is the thing searched; the property's own
/// docstring has the measurement of what that bought.
fn summary_draw() -> proptest::strategy::BoxedStrategy<Vec<SummaryEntry>> {
    proptest::collection::vec(
        (
            proptest::collection::vec(proptest::sample::select(SUMMARY_ANCESTORS.to_vec()), 0..3),
            proptest::sample::select(SUMMARY_LEAVES.to_vec()),
            any::<bool>(),
        ),
        1..4,
    )
    .boxed()
}

/// The include-file lines a draw of [`summary_draw`] spells.
fn spelled(entries: &[SummaryEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|(ancestors, leaf, slash)| {
            let sep = if *slash { "/" } else { "\\" };
            let mut segments = ancestors.clone();
            segments.push(leaf);
            segments.join(sep)
        })
        .collect()
}

proptest! {
    /// The `.git` exclusion, as a property over the whole path space: a
    /// segment spelled `.git` in ANY case, at ANY depth, under either
    /// separator, with ANY run of trailing dots and spaces — which Win32
    /// strips during path resolution, so `.git.` and `.git ` open the same
    /// directory there. This is the rule an include file must not be able to
    /// argue with, on any platform this ships to.
    #[test]
    fn any_path_with_a_git_segment_is_refused(
        before in proptest::collection::vec("[a-z]{1,4}", 0..4),
        after in proptest::collection::vec("[a-z]{1,4}", 0..4),
        upper in proptest::collection::vec(any::<bool>(), 4..5),
        trailing in "[. ]{0,3}",
        sep in any::<bool>(),
    ) {
        let mut git: String = ".git".chars().zip(upper.iter()).map(|(c, up)| {
            if *up { c.to_ascii_uppercase() } else { c }
        }).collect();
        git.push_str(&trailing);
        let mut segs = before.clone();
        segs.push(git);
        segs.extend(after.clone());
        let path = segs.join(if sep { "/" } else { "\\" });
        prop_assert_eq!(hard_refusal(&path), Some(Refusal::GitPath));
    }

    /// The converse, so the trimming above cannot swallow ordinary names: a
    /// segment that merely STARTS with `.git` is not `.git`, whatever it
    /// trails, and stays capturable.
    #[test]
    fn a_name_that_only_starts_with_git_is_not_the_git_dir(
        tail in "[a-z]{1,6}",
        trailing in "[. ]{0,3}",
    ) {
        let path = format!(".git{tail}{trailing}");
        prop_assert_eq!(hard_refusal(&path), None);
    }

    /// The class three point patches did not close, stated over the function
    /// that now decides it and searched with the spellings that broke it.
    ///
    /// Why this exists beside the end-to-end property below, restated after
    /// measurement because the first answer was wrong. That one passed with
    /// the defect present, and the reason recorded here was "its
    /// vocabulary held no `\`". Widening the vocabulary did not fix it:
    /// with its guard restored and the `\` screen in place, the end-to-end
    /// property is green 10 runs out of 10 even with `\` in its sample,
    /// because `hard_refusal` refuses those entries before `resolve` is
    /// consulted. **A screen upstream of a defect hides it from every
    /// property downstream of the screen, whatever they draw.**
    ///
    /// This one is downstream of nothing. It targets [`resolve`] directly
    /// and consults no screen, so nothing upstream can mask it: if
    /// `resolve` hands back a path, the directory that path will be read
    /// from is inside the work tree. In that same screen-in-place tree it
    /// reddens **7 of 10 runs** at 256 cases — on a uniform draw, which is
    /// the point: aiming the property at the deciding function bought more
    /// than shaping the sample did. Delete the `hard_refusal` `\` rule and
    /// this still holds; delete the ancestor check and it does not.
    ///
    /// Its vocabulary names the spellings three rounds cost: `\` (an
    /// ordinary filename byte on Unix, a separator on Windows), `.. ` and
    /// `. ` (which Win32 resolves to `..` and `.`), every ancestor door in
    /// the tree, and both separators as the joiner.
    #[test]
    fn resolve_only_ever_lands_inside_the_work_tree(
        segments in proptest::collection::vec(
            proptest::sample::select(vec![
                "bs\\dir", "g\\h", "x\\y", "deep", "cfg", "g", "out", "in",
                "deeper", "real", "sub", "id_rsa", "secret.txt", "notes.md",
                "a.txt", "config", "..", ".. ", ".", ". ", "...", "",
                ".git", ".GIT", ".git ", ".worktreeinclude",
                // A newline and a bare control character are ordinary
                // filename bytes on Unix, so a path property that never
                // contains one is a single-line property in disguise.
                // `resolve` sees entries before the screen does, so these
                // reach it exactly as spelled.
                "one\nf /etc/passwd", "bell\u{7}rung",
            ]),
            1..4,
        ),
        sep in any::<bool>(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let work = doors(dir.path());
        let root = std::fs::canonicalize(&work).unwrap();

        let entry = segments.join(if sep { "/" } else { "\\" });
        let Ok(full) = resolve(&work, &entry) else { return Ok(()) };

        prop_assert!(
            full.starts_with(&work),
            "`{}` resolved to {}, which is not under the work tree even \
             before anything is followed", entry, full.display(),
        );
        // The parent is where the bytes come from: `lstat` leaves the final
        // component alone (that is what makes a link a link record), so the
        // question an ancestor symlink answers differently is where the
        // directory holding the file actually is.
        if let Some(parent) = full.parent() {
            if let Ok(resolved) = std::fs::canonicalize(parent) {
                prop_assert!(
                    resolved.starts_with(&root),
                    "`{}` would be read from {} — outside {}",
                    entry, resolved.display(), root.display(),
                );
            }
        }
    }

    /// Whatever the include file says, a captured path is always one that
    /// `hard_refusal` accepts: the document's `f`/`l` records can only name
    /// paths that passed the gate above.
    ///
    /// **This property shipped hollow, and how it was hollow is the part
    /// worth keeping.** It ran over an EMPTY tempdir, so every drawn entry
    /// named nothing and earned `missing`: over 20 000 draws it produced
    /// 16 698 non-empty documents, 50 056 `x` records and **0 `f` and 0
    /// `l`** — both `prop_assert_eq!` bodies below executed **zero times**.
    /// Its alphabet could not even spell the control file it needed. A
    /// property whose vocabulary and shape are both fine can still assert
    /// nothing, because it never reached a state its assertions read.
    ///
    /// It now runs over [`doors`] — real files, real directories, real
    /// symlinks — and [`converse_draw`] mixes arbitrary spellings with
    /// paths that actually name something in that tree, so the records
    /// exist to be examined. Reachability is not left to a reader to
    /// notice: `the_converse_property_sees_the_records_it_asserts_about`
    /// drives the same strategy and fails if either record kind is absent.
    #[test]
    fn captured_records_only_ever_name_accepted_paths(
        entries in converse_draw(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let work = doors(dir.path());
        std::fs::write(work.join(INCLUDE_FILE), entries.join("\n")).unwrap();
        let text = String::from_utf8(
            capture_document(&work).as_slice().to_vec(),
        ).unwrap();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("f ") {
                prop_assert_eq!(hard_refusal(rest), None, "f {}", rest);
            }
            if let Some(rest) = line.strip_prefix("l ") {
                let path = rest.rsplit_once(" -> ").map_or(rest, |(p, _)| p);
                prop_assert_eq!(hard_refusal(path), None, "l {}", rest);
            }
        }
    }

    /// The property the spelling rules cannot express, over a tree built to
    /// break it: every captured file's parent, RESOLVED, is inside the work
    /// tree. `hard_refusal` says what a path may be spelled; this says where
    /// the bytes actually came from, which is the question an ancestor
    /// symlink answers differently from the name it was captured under.
    /// The vocabulary carries a NEWLINE and a bare control character on
    /// purpose (a maintainer note, 2026-08-07): the document is line-framed,
    /// `f <path>` is a header, and a property over paths that never contains
    /// `\n` is a single-line property wearing an arbitrary-input costume.
    /// Both are handled at different layers — the include file is read by
    /// lines, so a newline SPLITS an entry and can never sit inside one, and
    /// a surviving control character earns a refusal with the echo
    /// summarised rather than printed (`unsafe-path`, or `git-path` when
    /// the vocabulary also drew a `.git` segment — the summary belongs to no
    /// one token, see [`Refusal::UnsafePath`]) — so the second assertion below states
    /// the framing itself: every line of the document is a record or a
    /// content line, whatever the include file said.
    ///
    /// It also carries the backslash-component spellings — `\` in a link's own name, `.. `,
    /// `. `, `...` — and the tree now holds a `.git` with a credential in
    /// it, so "no bytes from outside" and "no bytes from the substrate" are
    /// searched together.
    ///
    /// The generator is SHAPED, not uniform — drawn as (ancestors, leaf,
    /// joiner) so the *shape* of the escape class is the thing searched.
    /// What that bought is a **retraction**: this comment used to claim a
    /// uniform draw found the restored escape in "0 of 256" cases. It
    /// does not. Re-measured with that guard restored byte-for-byte, the
    /// `\` screen removed, and the leak proven live first — vocabulary held
    /// constant across all three shapes so only the shape varies, 20 runs ×
    /// 256 cases and then 20 000 single draws per shape:
    ///
    /// | draw | runs red | per case |
    /// |---|---|---|
    /// | shaped, as shipped | 20/20 | 112/20 000 = 1 in 179 |
    /// | uniform over the same 25 tokens | 4/20 | 32/20 000 = 1 in 625 |
    /// | pre-reshape (flat vec, chunked) | 7/20 | 16/20 000 = 1 in 1250 |
    ///
    /// So reshaping sharpened the search ~3.5× per case against a uniform
    /// draw. Real, and worth keeping — but a 3.5× sharpening, not the
    /// difference between a blind property and a sighted one.
    ///
    /// **What does produce a zero is not the generator at all.** With the
    /// same guard restored and the `\` screen LEFT IN PLACE, detection is
    /// 0/10 runs for *every* shape, this one included: the screen refuses
    /// every `\`-bearing entry before `capture_document` ever reaches
    /// `resolve`, so an end-to-end property cannot see the defect whatever
    /// it draws. The belt masks the braces. The original "0 of 256" was
    /// almost certainly measured with the screen in, and attributed to
    /// shape an effect produced by the screen.
    ///
    /// That is precisely why the property ABOVE this one exists: it targets
    /// `resolve` directly and consults no screen, and in the same
    /// screen-in-place tree it reddens 7/10 runs on a *uniform* draw. What
    /// makes this class findable by search is the layer a property is aimed
    /// at, not the shape of its sample.
    #[test]
    fn a_captured_files_parent_always_resolves_inside_the_work_tree(
        entries in summary_draw(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let work = doors(dir.path());
        let list = spelled(&entries);
        std::fs::write(work.join(INCLUDE_FILE), list.join("\n")).unwrap();

        let text = String::from_utf8(
            capture_document(&work).as_slice().to_vec(),
        ).unwrap();
        for forbidden in [
            "PRIVATE-KEY-BYTES-XYZ", "MY-PRIVATE-JOURNAL-ENTRY", "GIT-TOKEN-abc123",
        ] {
            prop_assert!(
                !text.contains(forbidden),
                "captured bytes from outside the tree (or from the \
                 substrate): {}", text,
            );
        }

        let root = std::fs::canonicalize(&work).unwrap();
        for (n, line) in text.lines().enumerate() {
            // The framing, stated over the whole document: after the header
            // every line is a record or a space-prefixed content line, so
            // nothing an include file names can forge a record of its own.
            prop_assert!(
                n == 0
                    || line.starts_with("f ")
                    || line.starts_with("l ")
                    || line.starts_with("x ")
                    || line.starts_with(' '),
                "line {} is neither a record nor content: {:?}", n, line,
            );

            // The summary property, as a search rather than as three
            // trees: no record under a keyed token ever stands in for its
            // path. The reader's rule for an `x` record is the last
            // whitespace field, which is what `refuse` writes.
            // (`a_keyed_record_always_names_its_path_literally` proves the
            // three premises; this is what would find a fourth.)
            if let Some(rest) = line.strip_prefix("x ") {
                let (path, token) = rest.rsplit_once(' ').unwrap_or((rest, ""));
                prop_assert!(
                    !(KEYED_TOKENS.contains(&token) && is_summary(path)),
                    "a keyed record summarised its path, so the token no \
                     longer tells a reader whether the path field names \
                     anything: {:?}", line,
                );
            }

            let Some(rel) = line.strip_prefix("f ") else { continue };
            let parent = work.join(rel).parent().unwrap().to_owned();
            let resolved = std::fs::canonicalize(&parent).unwrap();
            prop_assert!(
                resolved.starts_with(&root),
                "`f {}` was read from {} — outside {}",
                rel, resolved.display(), root.display(),
            );
        }
    }
}
