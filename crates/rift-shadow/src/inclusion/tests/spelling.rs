//! What a path earns from its spelling alone — the `.git` rule in every
//! spelling, the two-platform screen, and the segment reading both questions
//! share — asserted against `hard_refusal` and against the decision under it.

use std::path::{Component, Path};

#[cfg(unix)]
use crate::inclusion::CAPTURE_DOC_HEADER;
use crate::inclusion::{hard_refusal, resolve, Refusal, INCLUDE_FILE};

#[cfg(unix)]
use super::fixtures::doors;
use super::fixtures::{doc, tree};

#[test]
fn a_git_path_is_refused_however_it_is_spelled() {
    let dir = tree(
        &[
            (".git/config", "[core]"),
            ("vendor/dep/.git/HEAD", "ref: x"),
        ],
        Some(".git/config\nvendor/dep/.git/HEAD\n.GIT/config\n.git\n"),
    );
    let text = doc(dir.path());
    assert!(
        !text.contains("[core]") && !text.contains("ref: x"),
        "no git-dir content is ever captured: {text}"
    );
    assert_eq!(
        text.lines().filter(|l| l.ends_with("git-path")).count(),
        4,
        "every spelling is refused by name: {text}"
    );
}

#[test]
fn a_directory_root_never_walks_into_a_nested_git_dir() {
    let dir = tree(
        &[
            ("pkg/keep.txt", "kept"),
            ("pkg/.git/config", "SECRET-GIT-STATE"),
        ],
        Some("pkg/\n"),
    );
    let text = doc(dir.path());
    assert!(text.contains(" kept"), "the ordinary file is captured");
    assert!(
        !text.contains("SECRET-GIT-STATE"),
        "the walk refuses the nested git dir: {text}"
    );
    assert!(text.contains("x pkg/.git git-path"), "and says so: {text}");
}

/// The limit the module doc states, measured rather than inferred: the
/// checks here are over names and over ancestors, so a **hardlink** — a
/// second name for an inode, a regular file at an accepted path with no
/// symlink in its ancestry — is captured under the name it was given. Git
/// cannot ship one (clone materialises regular files), so reaching this
/// needs local write access to the tree, at which point the same hand could
/// write the bytes into an ordinary file. Pinned so that the doc's limit is
/// a fact about this code and not a guess about it.
#[cfg(unix)]
#[test]
fn a_hardlink_is_captured_under_the_name_it_was_given() {
    let dir = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(dir.path()).unwrap();
    std::fs::write(dir.path().join(".git/config"), "GIT-CONFIG-BYTES").unwrap();
    std::fs::hard_link(
        dir.path().join(".git/config"),
        dir.path().join("innocuous.txt"),
    )
    .unwrap();
    std::fs::write(dir.path().join(INCLUDE_FILE), "innocuous.txt\n").unwrap();

    assert_eq!(
        doc(dir.path()),
        "rift-capture/1\nf innocuous.txt\n GIT-CONFIG-BYTES\n",
        "the `.git` rule is over paths, and a hardlink is a path that is \
         not one — the doc says so, and this is why it may"
    );
}

/// The third item of that finding: the `.git` comparison trimmed trailing dots and
/// spaces (Win32 strips them while resolving) and the traversal check read
/// the raw segment, so `..` refused and `.. ` did not — under this module's
/// own stated premise, parent traversal with the refusal record echoing it.
/// One notion of a segment now answers both.
#[test]
fn the_two_segment_questions_are_asked_about_the_same_segment() {
    for spelling in ["..", ".. ", ". ", ".", "...", "a/.. /b", "a/. /b", "   "] {
        assert_eq!(
            hard_refusal(spelling),
            Some(Refusal::UnsafePath),
            "{spelling:?}: a segment that resolves to nothing names no child"
        );
    }
    for spelling in [".git", ".GIT", ".git.", ".git ", "a/.git./b"] {
        assert_eq!(
            hard_refusal(spelling),
            Some(Refusal::GitPath),
            "{spelling:?}: and the git dir is still named as the git dir"
        );
    }
    assert_eq!(hard_refusal("a.txt"), None);
    assert_eq!(
        hard_refusal("dot."),
        None,
        "a real name that merely ends in \
         a dot resolves to `dot`, which is a child like any other"
    );

    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve(dir.path(), "a/.. /b"),
        Err(Refusal::UnsafePath),
        "and the decision agrees with the screen"
    );
}

/// The measured answer to "should an entry that means two files on two
/// platforms be refused outright": yes, and the belt is a screen over
/// [`resolve`], never a substitute for it — the second half of this test is
/// what says so, and it is the half that must keep passing if the screen is
/// ever removed.
#[test]
fn an_entry_that_two_platforms_would_read_differently_is_refused() {
    for spelling in [
        "src\\main.rs",
        "src\\",
        "bs\\/dir/id_rsa",
        // `:` is the other byte of the same class: an NTFS alternate data
        // stream separator (and the drive separator) on Windows, an
        // ordinary filename byte here. `.git:$I30` is how a Windows path
        // addresses the git dir's index without spelling `.git` as a
        // segment, which is why it is screened rather than left to a rule
        // that compares names.
        "a.txt:stream",
        ".git:$I30",
        "c:/x",
    ] {
        assert_eq!(
            hard_refusal(spelling),
            Some(Refusal::UnsafePath),
            "{spelling:?}: a path that means two things on two platforms \
             has no single thing to capture"
        );
    }
    assert_eq!(hard_refusal("src/main.rs"), None, "the dialect git uses");

    // The screen runs on walk-DISCOVERED names too, not only on typed
    // ones, so a symlink already on disk whose name holds `\` is recorded
    // `unsafe-path` rather than as a link. Nothing is captured either way;
    // the record says "ambiguous spelling" instead of "symlink", which is
    // the honest answer for a name a Windows checkout could not hold.
    #[cfg(unix)]
    {
        let held = tempfile::tempdir().unwrap();
        let work = held.path().join("w");
        std::fs::create_dir_all(work.join("p")).unwrap();
        std::os::unix::fs::symlink("/etc", work.join("p/a\\b")).unwrap();
        std::fs::write(work.join(INCLUDE_FILE), "p/\n").unwrap();
        assert_eq!(
            doc(&work),
            format!("{CAPTURE_DOC_HEADER}\nx p/a\\b unsafe-path\n"),
            "a discovered name is screened like a typed one"
        );
    }

    let dir = tree(&[("a.txt", "hi")], Some("src\\main.rs\n"));
    assert_eq!(
        doc(dir.path()),
        "rift-capture/1\nx src\\main.rs unsafe-path\n",
        "recorded, so a Windows developer learns the file wants `/` — \
         where it used to capture on Windows and record `missing` on Unix, \
         one line meaning two things"
    );

    // …and with the screen out of the way, the decision still holds — for
    // `\`. This is the belt-and-braces claim, stated where it can fail, and
    // stated for the ONE byte it is true of.
    #[cfg(unix)]
    {
        let held = tempfile::tempdir().unwrap();
        let work = doors(held.path());
        assert_eq!(
            resolve(&work, "bs\\dir/id_rsa"),
            Err(Refusal::SymlinkedParent),
            "the screen refuses this spelling, and so does the decision \
             underneath it — remove the `\\` screen and nothing escapes"
        );
        // The `:` half is the opposite, and the doc now says so. `:` is an
        // ordinary filename byte here as it is on Windows, so `resolve`
        // resolves an ADS-shaped name to an ordinary path in the tree; the
        // screen is what refuses it. Harmless on Unix, load-bearing there.
        assert_eq!(
            resolve(&work, "a.txt:stream"),
            Ok(work.join("a.txt:stream")),
            "`resolve` accepts a `:` name — the screen is not \
             belt-and-braces over this byte, it is the only rule"
        );
    }
}

/// The `:` half of the screen is **not** belt-and-braces, and this is the
/// assertion that says so on the platform where it matters.
///
/// [`hard_refusal`]'s doc claims `.git:$I30` and `a.txt:stream` reach
/// [`resolve`] as a single ordinary component on Windows — no drive prefix,
/// because Rust's `parse_drive` wants an ASCII letter at byte 0, and no
/// separator, because `:` is not one there either — so `resolve` returns
/// `Ok` and the screen is the only refusal. That is a claim about Windows
/// written from a Unix host, which is the exact shape that has been wrong
/// three times in this module, so it is written as an assertion and **CI's
/// Windows leg adjudicates it**, not this comment.
///
/// Deliberately not `#[cfg(windows)]`: a Windows-only test cannot be
/// compiled or reasoned about from here, and the assertion is meaningful on
/// both platforms — on Unix it is the same fact with no security weight
/// (`:` is an ordinary byte and the entry names a file in the tree), on
/// Windows it is an alternate-data-stream route to the git dir. If the
/// prefix parse were ever what the deleted sentence assumed, `parsed` would
/// hold a `Component::Prefix` and this would fail there and pass here.
#[test]
fn only_the_colon_screen_refuses_an_alternate_data_stream_name() {
    let dir = tempfile::tempdir().unwrap();
    for spelling in [".git:$I30", "a.txt:stream"] {
        let parsed: Vec<_> = Path::new(spelling).components().collect();
        assert_eq!(
            parsed.len(),
            1,
            "{spelling:?}: parsed as one component, not a drive-qualified \
             path: {parsed:?}"
        );
        assert!(
            matches!(parsed[0], Component::Normal(_)),
            "{spelling:?}: and that component is an ordinary name: {parsed:?}"
        );
        assert!(
            resolve(dir.path(), spelling).is_ok(),
            "{spelling:?}: so `resolve` accepts it — the ancestor rule \
             cannot see an alternate data stream, which is why the `:` rule \
             is the only one refusing this and not a second opinion"
        );
        assert_eq!(
            hard_refusal(spelling),
            Some(Refusal::UnsafePath),
            "{spelling:?}: …leaving the screen as the only thing refusing it"
        );
    }
}
