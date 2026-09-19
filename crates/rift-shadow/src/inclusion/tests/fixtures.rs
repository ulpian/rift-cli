//! The repository builders every subject shares: a throwaway tree, its
//! capture as text, the tree with every door this module has ever had left
//! open in it, and the keyed/unkeyed token partition the keying and property
//! tests both search with. Here rather than beside one subject so that no
//! test's fixture is another test's import.

use std::path::{Path, PathBuf};

use crate::inclusion::{capture_document, INCLUDE_FILE};

/// The document as text (tests read raw local bytes; nothing durable here).
pub(super) fn doc(work_tree: &Path) -> String {
    String::from_utf8(capture_document(work_tree).as_slice().to_vec()).unwrap()
}

pub(super) fn tree(files: &[(&str, &str)], include: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    if let Some(list) = include {
        std::fs::write(dir.path().join(INCLUDE_FILE), list).unwrap();
    }
    dir
}

/// The keyed half of [`Walk::recorded`]'s partition, as a value the
/// assertions can search with. Every exit that writes one of these keys the
/// path it names; the four in [`UNKEYED_TOKENS`] are keyed nowhere.
///
/// **The split is asserted text and the union is not.** Which side a token
/// belongs on is pinned by
/// `every_refusal_exit_in_the_walk_is_enumerated_and_classified` (source
/// order, labelled) and behaviourally by
/// `a_path_named_twice_is_captured_once` — a reader who moved a token
/// between these two lists would fail those, not this. But that the two
/// lists are **disjoint and exhaustive over [`Refusal`]** is checked here,
/// in `a_keyed_record_always_names_its_path_literally`: a twelfth variant,
/// or a token in both halves, or a token in neither, reddens. The
/// docstrings call that a partition of the token space, so a partition is
/// what a test has to fail on.
pub(super) const KEYED_TOKENS: [&str; 7] = [
    "missing",
    "not-a-file",
    "unsafe-link-target",
    "too-deep",
    "too-large",
    "binary",
    "budget-spent",
];

/// The other half: four tokens across five exits, and **the gap is not one
/// token written twice** — the sentence here used to read "five exits
/// (`too-many` is written at two of them)", which invites the arithmetic
/// 5 − 1 = 4 and is wrong in both directions. Two of the five exits write
/// whatever token their decider returned, so the map is many-to-many:
/// `too-many` at two exits ([`Walk::visit`]'s record cap and
/// [`Walk::walk_dir`]'s), `unsafe-path` at three ([`hard_refusal`]'s exit,
/// [`resolve`]'s `Err` arm, and `walk_dir`'s unnameable summary),
/// `git-path` at two (the first two of those, since either decider can
/// return it), and `symlinked-parent` at exactly one — [`resolve`]'s `Err`
/// arm, its only production site. See [`KEYED_TOKENS`] for what is asserted
/// about the pair and what is not.
pub(super) const UNKEYED_TOKENS: [&str; 4] =
    ["too-many", "git-path", "unsafe-path", "symlinked-parent"];

/// A tree with every door this module has ever had left open in it: a
/// directory outside the work tree holding the things an escape would fetch,
/// a `.git` inside it holding a credential, and symlinks into both — spelled
/// with `/` **and** with a `\` in the link's own name, because `\` is an
/// ordinary filename byte on Unix and a separator on Windows, and a guard
/// that splits it the wrong way sees ancestors the kernel does not.
///
/// Built on every platform (the symlinks are Unix-only, which is where they
/// can be created without privilege) so the properties over it run on every
/// platform too.
pub(super) fn doors(dir: &Path) -> PathBuf {
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("id_rsa"), "PRIVATE-KEY-BYTES-XYZ").unwrap();
    std::fs::write(outside.join("secret.txt"), "PRIVATE-KEY-BYTES-XYZ").unwrap();
    std::fs::write(outside.join("notes.md"), "MY-PRIVATE-JOURNAL-ENTRY").unwrap();

    let work = dir.join("work");
    std::fs::create_dir_all(work.join("real/sub")).unwrap();
    std::fs::write(work.join("real/a.txt"), "ordinary").unwrap();
    std::fs::write(work.join("real/sub/a.txt"), "deeper").unwrap();
    rift_git::test_support::fake_repo(&work).unwrap();
    std::fs::write(
        work.join(".git/config"),
        "[remote \"origin\"]\n\turl = https://x:GIT-TOKEN-abc123@h/r\n",
    )
    .unwrap();

    #[cfg(unix)]
    {
        // Out of the tree, spelled three ways: portable, with a `\` in the
        // link's own name, and one of each below a real directory.
        for name in ["cfg", "out", "bs\\dir", "deep/x\\y"] {
            let link = work.join(name);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&outside, link).unwrap();
        }
        // At the substrate, and back INTO the tree — an ancestor symlink is
        // refused either way, because the rule is about the shape of the
        // path, not about where it happens to land.
        for name in ["g", "g\\h"] {
            std::os::unix::fs::symlink(".git", work.join(name)).unwrap();
        }
        std::os::unix::fs::symlink("real", work.join("in")).unwrap();
        std::os::unix::fs::symlink("real/sub", work.join("deeper")).unwrap();
    }
    work
}
