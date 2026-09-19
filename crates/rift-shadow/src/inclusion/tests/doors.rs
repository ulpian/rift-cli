//! The doors out of the tree that a repository can ship: a symlinked
//! ancestor in any spelling, and the control file itself.

use crate::inclusion::{capture_document, INCLUDE_FILE, MAX_FILE_BYTES};
#[cfg(unix)]
use crate::inclusion::{resolve, Refusal, CAPTURE_DOC_HEADER};

use super::fixtures::tree;
#[cfg(unix)]
use super::fixtures::{doc, doors};

/// The door one component to the left of the one the link-record guard
/// closed. `lstat` does not
/// follow the FINAL component — that is what makes a symlink a link record —
/// but the kernel follows every INTERMEDIATE one, so an entry whose ancestor
/// is a symlink reads a file outside the tree under an in-tree name. The
/// spelling check cannot see it: `cfg/id_rsa` is an ordinary path.
#[cfg(unix)]
#[test]
fn an_entry_under_a_symlinked_ancestor_is_refused_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("private");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("id_rsa"), "PRIVATE-KEY-BYTES-XYZ").unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::os::unix::fs::symlink(&outside, work.join("cfg")).unwrap();
    std::fs::write(work.join(INCLUDE_FILE), "cfg/id_rsa\n").unwrap();

    let text = doc(&work);
    assert!(
        !text.contains("PRIVATE-KEY-BYTES-XYZ"),
        "an ancestor symlink is a door out of the tree: {text}"
    );
    assert_eq!(
        text,
        format!("{CAPTURE_DOC_HEADER}\nx cfg/id_rsa symlinked-parent\n"),
        "and the refusal is recorded, naming what was wrong with the path"
    );
}

/// The same door pointed at the substrate — and the reason the `.git` rule
/// needed more than a spelling check. `g -> .git` is a symlinked directory,
/// which git stores as mode `120000` and clone recreates, so a repository
/// can ship the whole vector: no local `ln`, no non-regular control file.
#[cfg(unix)]
#[test]
fn an_entry_under_an_ancestor_symlinked_at_the_git_dir_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(dir.path()).unwrap();
    std::fs::write(
        dir.path().join(".git/config"),
        "[remote \"origin\"]\n\turl = https://x:GIT-TOKEN-abc123@h/r\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(".git", dir.path().join("g")).unwrap();
    std::fs::write(dir.path().join(INCLUDE_FILE), "g/config\n").unwrap();

    let text = doc(dir.path());
    assert!(
        !text.contains("GIT-TOKEN-abc123"),
        "`.git` reached through a symlinked ancestor is still `.git`: {text}"
    );
    assert_eq!(
        text,
        format!("{CAPTURE_DOC_HEADER}\nx g/config symlinked-parent\n"),
    );
}

/// The backslash-component defect, and the reason the guard is now
/// component-wise. `bs\dir` is ONE
/// component on Unix and the kernel follows it; a check that splits `rel` on
/// `\` looks for `work/bs` and `work/bs/dir`, finds neither, and reports "no
/// symlinked ancestor" while the walk reads a private key through the link.
/// Asserted against [`resolve`] itself, not through the document, so it
/// stays a test of the guard rather than of whatever screens precede it.
#[cfg(unix)]
#[test]
fn a_backslash_named_ancestor_is_the_symlink_the_kernel_sees() {
    let dir = tempfile::tempdir().unwrap();
    let work = doors(dir.path());

    for entry in ["cfg/id_rsa", "bs\\dir/id_rsa", "deep/x\\y/id_rsa"] {
        assert_eq!(
            resolve(&work, entry),
            Err(Refusal::SymlinkedParent),
            "{entry}: every spelling of an ancestor symlink is one ancestor \
             symlink — the operating system's parse is the only one that \
             decides where the components are"
        );
    }
    for entry in ["g/config", "g\\h/config"] {
        assert_eq!(
            resolve(&work, entry),
            Err(Refusal::SymlinkedParent),
            "{entry}: `.git` reached through a symlinked ancestor is `.git`"
        );
    }
    assert_eq!(
        resolve(&work, "real/a.txt"),
        Ok(work.join("real/a.txt")),
        "and an ordinary path still resolves, to the path that is opened"
    );
}

/// The same doors from the outside: whatever the refusal is spelled, no byte
/// from beyond the tree reaches the document, and every entry is accounted
/// for by a record.
#[cfg(unix)]
#[test]
fn no_spelling_of_an_ancestor_symlink_reaches_bytes_outside_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let work = doors(dir.path());
    let entries = [
        "cfg/id_rsa",
        "bs\\dir/id_rsa",
        "bs\\dir/notes.md",
        "deep/x\\y/id_rsa",
        "g/config",
        "g\\h/config",
    ];
    std::fs::write(work.join(INCLUDE_FILE), entries.join("\n")).unwrap();

    let text = doc(&work);
    for forbidden in [
        "PRIVATE-KEY-BYTES-XYZ",
        "MY-PRIVATE-JOURNAL-ENTRY",
        "GIT-TOKEN-abc123",
    ] {
        assert!(
            !text.contains(forbidden),
            "capture left the work tree: {text}"
        );
    }
    assert_eq!(
        text.lines().filter(|l| l.starts_with("x ")).count(),
        entries.len(),
        "and each entry is refused by name, none silently dropped: {text}"
    );
}

/// The control file is a door too, and it was the one left open. Git stores
/// a symlink as mode `120000` and clone recreates it, so a repository can
/// **ship** `.worktreeinclude` pointed at a file only the developer can
/// read. Reading through it turns every line of `~/.aws/credentials` into an
/// include entry — and an entry that names nothing in the tree is echoed
/// verbatim into the durable document as `x <line> missing`.
#[cfg(unix)]
#[test]
fn a_control_file_symlinked_out_of_the_tree_is_never_read() {
    let dir = tempfile::tempdir().unwrap();
    let credentials = dir.path().join("credentials");
    std::fs::write(
        &credentials,
        "aws_access_key_id = AKIA-EXFILTRATED-BY-THE-CONTROL-FILE\n",
    )
    .unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("ordinary.txt"), "in the tree").unwrap();
    std::os::unix::fs::symlink(&credentials, work.join(INCLUDE_FILE)).unwrap();

    let text = doc(&work);
    assert!(
        !text.contains("AKIA-EXFILTRATED-BY-THE-CONTROL-FILE"),
        "a symlinked control file must not be read through: {text}"
    );
    assert!(
        capture_document(&work).as_slice().is_empty(),
        "and the capture is the empty one, byte for byte: {text}"
    );
}

/// The same door, pointed at the substrate instead of the home directory.
/// `hard_refusal` guards *walked* paths; it never saw the control file, so
/// `.git`'s own bytes could be read as a list of paths and echoed back out.
#[cfg(unix)]
#[test]
fn a_control_file_symlinked_into_the_git_dir_is_never_read() {
    let dir = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(dir.path()).unwrap();
    std::fs::write(
        dir.path().join(".git/config"),
        "[remote \"origin\"]\n\turl = https://x:GIT-CONFIG-TOKEN-c0ffee@host/r.git\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        dir.path().join(".git/config"),
        dir.path().join(INCLUDE_FILE),
    )
    .unwrap();

    let text = doc(dir.path());
    assert!(
        !text.contains("GIT-CONFIG-TOKEN-c0ffee"),
        "the control file must not launder .git's bytes into the document: {text}"
    );
    assert!(capture_document(dir.path()).as_slice().is_empty(), "{text}");
}

/// The check is not only about symlinks. A directory and an absurd file are
/// both "not a list", and both answer the way the ruling says: the empty
/// capture, silently, with nothing added to the document.
#[test]
fn a_non_regular_or_oversize_control_file_is_the_empty_list() {
    let dir = tree(&[("a.txt", "kept")], None);
    std::fs::create_dir_all(dir.path().join(INCLUDE_FILE)).unwrap();
    assert!(
        capture_document(dir.path()).as_slice().is_empty(),
        "a directory is not a list"
    );

    let big = tree(&[("a.txt", "kept")], None);
    let padding = "# padding\n".repeat(usize::try_from(MAX_FILE_BYTES).unwrap() / 10 + 1);
    std::fs::write(big.path().join(INCLUDE_FILE), format!("a.txt\n{padding}")).unwrap();
    assert!(
        capture_document(big.path()).as_slice().is_empty(),
        "an oversize control file is declined whole, never parsed to its cap \
         — a git hook does not read an arbitrary number of bytes because a \
         repository asked it to"
    );
}
