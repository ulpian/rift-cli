//! Symlinks and discovered names: a link is never followed, an `l` record
//! holds its target exactly or not at all, and a name the document cannot
//! spell is refused under its parent. Every test here needs a symlink the
//! process can create, so the module is Unix-only.

use std::path::{Path, PathBuf};

use crate::inclusion::{discovered, hard_refusal, link_record, CAPTURE_DOC_HEADER, INCLUDE_FILE};

use super::fixtures::doc;

#[cfg(unix)]
#[test]
fn a_symlink_is_recorded_and_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside-the-tree");
    std::fs::write(&outside, "PRIVATE-KEY-BYTES").unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::os::unix::fs::symlink(&outside, work.join("notes.txt")).unwrap();
    std::fs::write(work.join(INCLUDE_FILE), "notes.txt\n").unwrap();

    let text = doc(&work);
    assert!(
        !text.contains("PRIVATE-KEY-BYTES"),
        "following the link would exfiltrate anything the developer can \
         read: {text}"
    );
    assert!(
        text.contains("l notes.txt -> ") && text.contains("outside-the-tree"),
        "the link is preserved as a link: {text}"
    );
}

/// The `l` record's reader rule, from all **three** directions that can
/// break it. This docstring said "both sides" and the test drove two; the
/// third was the one that was live.
///
/// All three ship through git: a repository can carry a symlink *named*
/// `a -> b` in its index and clone materialises it, and a symlink's
/// *target* is arbitrary bytes in a blob, so `ln -s '-> y' link` travels as
/// an ordinary blob too. So this is a rule the format needs, not a corner.
///
/// 1. the NAME holds the separator — written, and read back exactly;
/// 2. the TARGET holds the separator — refused;
/// 3. the TARGET merely **begins** `-> ` — refused now, and **not** by the
///    guard this replaces. `target.contains(" -> ")` cannot see it, because
///    the writer emits ` -> ` itself and the concatenation supplies the
///    second separator: `l arrowtarget -> -> y` rsplits to
///    `("arrowtarget ->", "y")` when the truth is `("arrowtarget", "-> y")`.
///    Both sides misread it and no test failed — which is why the writer
///    now reads its own record back instead of screening a substring.
///
/// The fourth case is the control that keeps the rule from silently
/// becoming "no arrows in targets": a target of `->` moves no separator,
/// round-trips, and is still written.
#[cfg(unix)]
#[test]
fn a_link_record_is_read_back_by_its_last_arrow() {
    fn one_link(name: &str, target: &str) -> String {
        let held = tempfile::tempdir().unwrap();
        let work = held.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(target, work.join(name)).unwrap();
        std::fs::write(work.join(INCLUDE_FILE), format!("{name}\n")).unwrap();
        doc(&work)
    }

    let text = one_link("a -> b", "elsewhere");
    let rest = text.lines().find_map(|l| l.strip_prefix("l ")).unwrap();
    assert_eq!(
        rest.rsplit_once(" -> "),
        Some(("a -> b", "elsewhere")),
        "the last ` -> ` is the separator the writer put there, so a link \
         whose NAME holds one round-trips exactly: {text}"
    );

    // …exact only because a target that would move that separator is
    // refused rather than written ambiguously. Remove the round-trip check
    // and this document reads back as the link `link -> x` pointing at `y`.
    assert_eq!(
        one_link("link", "x -> y"),
        format!("{CAPTURE_DOC_HEADER}\nx link unsafe-link-target\n"),
        "a target that holds the separator cannot be recorded unambiguously"
    );

    // The direction a substring guard cannot reach: the writer's own ` -> `
    // plus a target beginning `-> ` is two separators, and the last one is
    // not the writer's.
    assert_eq!(
        one_link("arrowtarget", "-> y"),
        format!("{CAPTURE_DOC_HEADER}\nx arrowtarget unsafe-link-target\n"),
        "`l arrowtarget -> -> y` reads back as (\"arrowtarget ->\", \"y\"), \
         which is neither the path nor the target"
    );

    // The control: the rule is the round-trip, not a ban on arrows.
    let text = one_link("plain", "->");
    let rest = text.lines().find_map(|l| l.strip_prefix("l ")).unwrap();
    assert_eq!(
        rest.rsplit_once(" -> "),
        Some(("plain", "->")),
        "a target that reads back correctly is still written: {text}"
    );
}

/// **An empty target is refused; a target that RENDERS as empty is
/// written**, and to a human the second is the first. A byte-order mark is
/// valid UTF-8, non-empty, not a control character (it is `Cf`, not `Cc`),
/// and round-trips, so `l t_bom -> \u{FEFF}` is written — and printed to a
/// terminal it reads `l t_bom -> `, indistinguishable from the empty target
/// that earns `unsafe-link-target`.
///
/// Nothing here is wrong: the record holds the exact bytes `readlink`
/// returned, which is the guarantee. What would be wrong is a reader
/// inferring emptiness from a *rendering*, so the format text says it in
/// one clause and this test is why it may. The same holds for any
/// zero-width or bidi-control-free formatting character; the BOM is just
/// the one a repository acquires by accident.
#[cfg(unix)]
#[test]
fn a_target_that_renders_as_nothing_is_not_an_empty_target() {
    let held = tempfile::tempdir().unwrap();
    let work = held.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::os::unix::fs::symlink("\u{FEFF}", work.join("t_bom")).unwrap();
    // Measured on APFS: `ls` renders these two as `t_bom -> ` and
    // `t_empty ->`, which is the whole point of the clause.
    let empty_exists = std::os::unix::fs::symlink("", work.join("t_empty")).is_ok();
    std::fs::write(work.join(INCLUDE_FILE), "t_bom\nt_empty\n").unwrap();

    let text = doc(&work);
    let rest = text
        .lines()
        .find_map(|l| l.strip_prefix("l "))
        .unwrap_or_default();
    assert_eq!(
        rest.rsplit_once(" -> "),
        Some(("t_bom", "\u{FEFF}")),
        "a BOM-only target is WRITTEN, and reads back as the exact bytes \
         `readlink` returned: {text}"
    );
    // The control, asserted through the walk's own admission decision:
    // Linux refuses `symlink("")` with ENOENT, so on that leg no empty link
    // can exist for the walk to reach, and the contrast is measured here.
    assert_eq!(
        link_record("t_bom", "\u{FEFF}").as_deref(),
        Some("t_bom -> \u{FEFF}"),
        "the decision admits the BOM target the walk wrote"
    );
    assert_eq!(
        link_record("t_empty", ""),
        None,
        "while an EMPTY target is REFUSED — the two are one invisible \
         character apart on a terminal and a whole rule apart here"
    );
    // Where the OS lets the control exist (APFS does), the walk itself
    // still says so in the record.
    if empty_exists {
        assert!(
            text.contains("x t_empty unsafe-link-target"),
            "the walk refuses the empty target as the decision does: {text}"
        );
    }
}

/// A symlink target that is **not valid UTF-8**: refused, never
/// transcribed — and it needs its own rule because the round-trip guard
/// above structurally cannot see it.
///
/// A target is arbitrary bytes (it is a blob), so `a\xFF\xFEb` is a target
/// a repository can ship. Recorded through `to_string_lossy` it became
/// `a<U+FFFD><U+FFFD>b`, which is non-empty, holds no control character,
/// and rsplits back to exactly the pair that went in — so the record was
/// **well framed and false**: the pair it yields is not the pair the
/// filesystem holds. The first assertion below is that framing *success*,
/// so the two rules are visibly independent rather than one being read as
/// a consequence of the other.
///
/// The rule this settles is [`Refusal::Binary`]'s, one field over: this
/// module does not store a lossy transcription of a thing while claiming to
/// hold it. Content was already refused on that ground; a link target was
/// not. (`a_repository_can_ship_a_non_utf8_link_target_through_clone` is
/// the same class measured through a real index and a real clone, so
/// "shippable" is not argued here either.)
#[cfg(unix)]
#[test]
fn a_link_target_that_is_not_utf8_is_refused_rather_than_transcribed() {
    use std::os::unix::ffi::OsStrExt;

    let raw: &[u8] = b"a\xff\xfeb";
    let lossy = String::from_utf8_lossy(raw).into_owned();

    // The premise, asserted rather than assumed: the transcription passes
    // every check the `l` record already had. A guard that refuses this
    // would have to refuse `->`-free, control-free, non-empty targets too.
    let framed = format!("link -> {lossy}");
    assert_eq!(
        framed.rsplit_once(" -> "),
        Some(("link", lossy.as_str())),
        "the round-trip guard accepts the lossy record — it is exact about \
         the record's FRAMING and says nothing about whether the target is \
         the one on disk"
    );
    assert_ne!(
        lossy.as_bytes(),
        raw,
        "…and the pair it yields is not the pair the filesystem holds"
    );

    let held = tempfile::tempdir().unwrap();
    let work = held.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(raw), work.join("link")).unwrap();
    std::os::unix::fs::symlink("elsewhere", work.join("plain")).unwrap();
    std::fs::write(work.join(INCLUDE_FILE), "link\nplain\n").unwrap();
    assert_eq!(
        std::fs::read_link(work.join("link"))
            .unwrap()
            .as_os_str()
            .as_bytes(),
        raw,
        "the filesystem really holds those bytes, so the refusal below is \
         about a link that exists"
    );

    let text = doc(&work);
    assert!(
        text.contains("x link unsafe-link-target"),
        "a target the document cannot spell exactly is a refusal: {text}"
    );
    assert!(
        !text.contains(char::REPLACEMENT_CHARACTER),
        "and never a transcription of it: {text}"
    );
    assert!(
        text.contains("l plain -> elsewhere"),
        "the control — the rule is UTF-8 validity, not a ban on links: {text}"
    );
}

/// The same class, reached the way a victim would reach it: from a hostile
/// repository, through a real index entry at mode `120000` and a real
/// clone. The module says "clone materialises it" in four places and has
/// never measured it; every escape this module has had was a sentence that
/// was true of its rule set and false of something outside it.
#[cfg(unix)]
#[test]
fn a_repository_can_ship_a_non_utf8_link_target_through_clone() {
    use std::os::unix::ffi::OsStrExt;

    let raw: &[u8] = b"a\xff\xfeb";
    let held = tempfile::tempdir().unwrap();
    let clone = clone_shipping_link_targets(held.path(), raw);

    for at in ["link", "d/link"] {
        assert_eq!(
            std::fs::read_link(clone.join(at))
                .unwrap()
                .as_os_str()
                .as_bytes(),
            raw,
            "clone materialises the target byte-exact at {at} — the claim \
             this module makes in prose everywhere and had measured nowhere"
        );
    }
    let text = doc(&clone);
    assert!(
        text.contains(" fine"),
        "the walk really entered the included directory, so the discovered \
         refusal below is one it reached: {text}"
    );
    assert!(
        text.contains("x link unsafe-link-target") && text.contains("x d/link unsafe-link-target"),
        "so the refusal is what a cloned repository gets, typed or \
         discovered: {text}"
    );
    assert!(
        !text.contains(char::REPLACEMENT_CHARACTER),
        "and not a transcription: {text}"
    );
}

/// A clone of a repository whose index carries two mode-`120000` entries
/// with `target` as their blob — a link the include list TYPES (`link`) and
/// one the walk DISCOVERS inside an included directory (`d/link`) — beside an
/// ordinary `d/ok.txt` and a control file naming `link` and `d`. The target's
/// bytes are a blob's contents, so nothing validates them.
#[cfg(unix)]
fn clone_shipping_link_targets(held: &Path, target: &[u8]) -> PathBuf {
    let origin = held.join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    let carrier = held.join("target.bin");
    std::fs::write(&carrier, target).unwrap();
    let oid = git(&origin, &["hash-object", "-w", carrier.to_str().unwrap()]);
    for at in ["link", "d/link"] {
        let cacheinfo = format!("120000,{oid},{at}");
        git(
            &origin,
            &["update-index", "--add", "--cacheinfo", &cacheinfo],
        );
    }
    std::fs::create_dir_all(origin.join("d")).unwrap();
    std::fs::write(origin.join("d/ok.txt"), "fine").unwrap();
    std::fs::write(origin.join(INCLUDE_FILE), "link\nd\n").unwrap();
    git(&origin, &["add", INCLUDE_FILE, "d/ok.txt"]);
    git(&origin, &["commit", "-q", "-m", "ship it"]);

    let clone = held.join("clone");
    let (from, to) = (origin.to_str().unwrap(), clone.to_str().unwrap());
    git(held, &["clone", "-q", from, to]);
    clone
}

/// Hermetic git: neither the operator's config nor an ambient `GIT_*` (a test
/// suite can be run from inside a hook) can change the answer.
#[cfg(unix)]
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// What a discovered name decides — asserted on **every** leg, including
/// the ones whose filesystem cannot be persuaded to produce the input
/// (APFS: `EILSEQ`; macOS FAT: percent-encoded back into ASCII; both
/// measured). The end-to-end test below is adjudicated on Linux; this one
/// is the decision itself, handed the name directly.
#[cfg(unix)]
#[test]
fn a_discovered_name_is_a_path_to_visit_or_a_refusal_to_record() {
    use std::os::unix::ffi::OsStrExt;

    assert_eq!(
        discovered("d", std::ffi::OsStr::new("ok.txt")),
        Ok("d/ok.txt".to_owned()),
        "an ordinary name is just the entry path"
    );
    assert_eq!(
        discovered("d", std::ffi::OsStr::from_bytes(b"na\xff\xfeme")),
        Err("d/<6 unnameable bytes>".to_owned()),
        "a name the document cannot spell names its PARENT — a path the \
         document can spell — and counts the child rather than inventing it"
    );
    // Whatever the summary says, it must be printable as-is: `refuse`
    // echoes a control-free path verbatim, so a summary that smuggled one
    // in would be re-summarised into `<N unprintable bytes>` and lose the
    // parent that is the whole point of it.
    let summary = discovered("a/b", std::ffi::OsStr::from_bytes(b"\xff")).unwrap_err();
    assert!(
        !summary.chars().any(char::is_control),
        "the summary is printable: {summary}"
    );
    assert_eq!(hard_refusal(&summary), None, "…and echoes as itself");
}

/// The rule of [`Walk::link`] one component to the left: a **name** the
/// document cannot spell is refused, not renamed.
///
/// Lossily converted, `na\xFF\xFEme` became the entry `d/na<U+FFFD><U+FFFD>me`,
/// which `resolve` rebuilt into a path the kernel does not have — so the
/// document said `missing`, a **false reason** for a file that exists,
/// under a path nobody typed.
///
/// **Adjudicated on the Linux leg.** APFS refuses a non-UTF-8 filename
/// outright (`EILSEQ`, measured on this machine), so the class is
/// unreachable on macOS and the body below degrades to the control
/// assertion there — the same posture as
/// `only_the_colon_screen_refuses_an_alternate_data_stream_name`, whose
/// Windows run is what adjudicates it.
/// `a_discovered_name_is_a_path_to_visit_or_a_refusal_to_record` covers the
/// record's text on every leg — it hands [`discovered`] the bytes directly
/// — and what only Linux can exercise is the branch that chooses it. (This
/// docstring cited `an_unnameable_child_is_summarised_under_its_parent`,
/// which does not exist and never did; in a module whose discipline is that
/// each property names the tests that would fail if it were false, a
/// citation to a test that does not exist is a claim without a test.)
#[cfg(unix)]
#[test]
fn a_directory_entry_that_cannot_be_named_is_refused_not_renamed() {
    use std::os::unix::ffi::OsStrExt;

    let held = tempfile::tempdir().unwrap();
    let work = held.path().join("work");
    std::fs::create_dir_all(work.join("d")).unwrap();
    std::fs::write(work.join("d/ok.txt"), "fine").unwrap();
    std::fs::write(work.join(INCLUDE_FILE), "d\n").unwrap();

    let raw: &[u8] = b"na\xff\xfeme";
    let hostile = work.join("d").join(std::ffi::OsStr::from_bytes(raw));
    let filesystem_holds_it = std::fs::write(&hostile, "HOSTILE-BYTES").is_ok();

    let text = doc(&work);
    assert!(
        text.contains("f d/ok.txt"),
        "the nameable sibling is captured either way: {text}"
    );
    if !filesystem_holds_it {
        return; // macOS: EILSEQ. The Linux CI leg asserts the rest.
    }
    assert!(
        !text.contains(char::REPLACEMENT_CHARACTER),
        "no invented path: {text}"
    );
    assert!(
        text.contains("x d/<6 unnameable bytes> unsafe-path"),
        "the child is refused under a summary of its parent: {text}"
    );
    assert!(
        !text.contains("missing"),
        "and not with a false reason for a file that exists: {text}"
    );
    assert!(
        !text.contains("HOSTILE-BYTES"),
        "and refusing it did not read it: {text}"
    );
}

/// The discovered half of the same rule, and the half a format reader is
/// likeliest to get wrong: a link the walk **meets** inside an included
/// directory is screened exactly as a typed one is, so it earns an `l`
/// record only when the record can hold it. Not followed either way — that
/// is the property that matters — but `unsafe-link-target` is what the
/// document says instead, and a text that promises an `l` record for every
/// discovered symlink is wrong about a class git can ship.
#[cfg(unix)]
#[test]
fn a_discovered_link_the_record_cannot_hold_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    // A directory outside the tree whose NAME holds the separator, so the
    // link's target does too — nothing about the link's own name is odd.
    let outside = dir.path().join("private -> x");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("id_rsa"), "PRIVATE-KEY-BYTES").unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("cfg")).unwrap();
    std::fs::write(work.join("cfg/ok.txt"), "fine").unwrap();
    std::os::unix::fs::symlink(&outside, work.join("cfg/linked")).unwrap();
    std::fs::write(work.join(INCLUDE_FILE), "cfg/\n").unwrap();

    let text = doc(&work);
    assert!(
        text.contains(" fine"),
        "the ordinary sibling is captured: {text}"
    );
    assert!(
        text.contains("x cfg/linked unsafe-link-target"),
        "a discovered link the record cannot hold is a REFUSAL, not an `l` \
         record: {text}"
    );
    assert!(
        !text.contains("l cfg/linked"),
        "and it is not written ambiguously either: {text}"
    );
    assert!(
        !text.contains("PRIVATE-KEY-BYTES") && !text.contains("id_rsa"),
        "and refusing it did not descend it: {text}"
    );
}

#[cfg(unix)]
#[test]
fn a_walk_records_a_symlinked_directory_without_descending_it() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("private");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("id_rsa"), "PRIVATE-KEY-BYTES").unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("cfg")).unwrap();
    std::fs::write(work.join("cfg/ok.txt"), "fine").unwrap();
    std::os::unix::fs::symlink(&outside, work.join("cfg/linked")).unwrap();
    std::fs::write(work.join(INCLUDE_FILE), "cfg/\n").unwrap();

    let text = doc(&work);
    assert!(text.contains(" fine"));
    assert!(
        !text.contains("PRIVATE-KEY-BYTES") && !text.contains("id_rsa"),
        "a symlinked directory is not a door into the walk: {text}"
    );
    assert!(text.contains("l cfg/linked -> "), "{text}");
}
