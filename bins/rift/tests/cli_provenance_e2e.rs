//! `rift blame`, `rift why` and `rift checkpoint explain` against real git
//! repositories: the locked `--json` contracts over a date-pinned commit, and
//! the properties of *git's* own behaviour the porcelain parser depends on —
//! what `core.quotePath=false` buys and costs, what git quotes whatever that
//! flag says, and what the `--` terminator decides.
//!
//! The renderer's own tests (`bins/rift/src/provenance.rs`) are the assurance
//! for what reaches a terminal; these are the assurance for the route.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

#[allow(dead_code)] // The CLI suites share these builders; each file uses a subset.
#[path = "common/cli_fixture.rs"]
mod cli_fixture;
mod common;
use cli_fixture::{code, git, git_commit_at, rift, rift_dir};

/// An empty repository, hermetic and in the format `rift init` can open.
fn init_repo(home: &Path, repo: &Path) {
    assert!(git(home, repo, &["init", "-q", "."]).status.success());
}

/// Commit whatever the caller has written into `repo`, dates left to git.
/// Only the tests whose subject is a *path* use this; a test that asserts on
/// a commit oid needs the dates pinned, which [`seed_pinned_commit`] does.
fn commit_worktree(home: &Path, repo: &Path) {
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-q", "-m", "seed"])
        .status
        .success());
}

/// A repository holding one date-pinned commit of `f.rs`, opted into the
/// query surface. Both git dates are pinned, so the commit oid — and
/// therefore every document frozen over it below — is a constant of the
/// fixture rather than of the wall clock.
fn seed_pinned_commit(home: &Path, repo: &Path, contents: &str) {
    init_repo(home, repo);
    std::fs::write(repo.join("f.rs"), contents).unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(
        git_commit_at(home, repo, "2026-01-02T03:04:05 +0000", "seed")
            .status
            .success()
    );
    rift_dir(home, repo);
}

/// `rift blame`'s frozen answer over [`seed_pinned_commit`]'s `a\nb\nc\n`
/// tree, asked for line 2 alone. This IS the agent-facing contract.
fn the_locked_blame_document() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "file": "f.rs",
        "lines": 1,
        "range": { "start": 2, "end": 2 },
        "spans": [{
            "start": 2,
            "end": 2,
            "lines": 1,
            "commit": "eca66ff1b988af6ed2654840c0ef0a4aa1559c50",
            "path": "f.rs",
            "tag": "unknown",
            "selection": null,
            "attribution": null,
            "sessions": [],
        }],
        "commits": {
            "eca66ff1b988af6ed2654840c0ef0a4aa1559c50": {
                "claims": [],
                "rejected": 0,
                "truncated": false,
            },
        },
        "checkpoints": {},
        "summary": {
            "commits": 1,
            "checkpoints": 0,
            "lines_with_file_evidence": 0,
        },
        "reads": { "messages": 1, "facts": 0, "counts": 0 },
    })
}

/// `rift why`'s frozen answer over the same tree, for line 2 — including the
/// `file:` handle, whose hash is the sha256 of the fixture's exact bytes.
fn the_locked_why_document() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "file": "f.rs",
        "line": 2,
        "span": {
            "start": 2,
            "end": 2,
            "commit": "eca66ff1b988af6ed2654840c0ef0a4aa1559c50",
            "path": "f.rs",
        },
        "tag": "unknown",
        "selection": null,
        "attribution": null,
        "sessions": [],
        "checkpoint": null,
        "prompt": null,
        "cost": null,
        "handles": {
            "file": "file:f.rs@sha256:880553fca8fcea94e325ee2cfb48e5a985cc797f39a14cc6d3cedecfeb2ae4d2",
        },
        "reads": { "messages": 1, "facts": 0, "counts": 0 },
    })
}

/// `--line` end-to-end, and **the locked `--json` contract for `rift blame`**
/// — the agent-facing contract for the verb.
///
/// The fixture commit has both git dates pinned, so its oid is a constant and
/// the whole `--json` document is byte-comparable: the golden below IS the
/// agent-facing contract, and any shape change — a renamed key, a moved field,
/// a type change — reds this test until the change is deliberate
/// (`schema_version` bumps on breaking changes per the envelope's rule).
#[test]
fn blame_line_answers_true_numbers_and_the_json_contract_is_locked() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_pinned_commit(home.path(), repo.path(), "a\nb\nc\n");

    // The ranged human render speaks true file numbers, not excerpt numbers.
    let out = rift(
        home.path(),
        repo.path(),
        &["blame", "f.rs", "--line", "2-3"],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let human = String::from_utf8_lossy(&out.stdout);
    assert!(human.contains("--line 2-3"), "{human}");
    assert!(human.contains("2 lines across 1 commit;"), "{human}");
    assert!(
        !human.contains("1-3"),
        "excerpt renumbering leaked: {human}"
    );

    // The locked contract, byte-for-byte.
    let json = rift(
        home.path(),
        repo.path(),
        &["blame", "f.rs", "--line", "2", "--json"],
    );
    assert_eq!(code(&json), 0, "{json:?}");
    let got: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        got,
        the_locked_blame_document(),
        "the rift blame --json contract moved"
    );

    // A range past the end refuses with the range named — never a guess.
    let past = rift(
        home.path(),
        repo.path(),
        &["blame", "f.rs", "--line", "9-12", "--json"],
    );
    assert_eq!(code(&past), 4, "PRECONDITION: {past:?}");
    let v: serde_json::Value = serde_json::from_slice(&past.stdout).unwrap();
    assert_eq!(v["error"], "not_blameable");
    assert_eq!(v["line"], "9-12");

    // A malformed spec is a usage error before git is ever asked.
    let bad = rift(
        home.path(),
        repo.path(),
        &["blame", "f.rs", "--line", "3-1"],
    );
    assert_ne!(code(&bad), 0, "{bad:?}");
}

/// `rift why` end-to-end, and **the locked `--json` contract**: the colon spec
/// answers one line, the flag spelling answers
/// identically, a range is refused with the single-line sentence, and the
/// whole document is byte-comparable over the date-pinned fixture — including
/// the `file:` handle, whose hash is the sha256 of the fixture's exact bytes.
#[test]
fn why_answers_one_line_and_the_json_contract_is_locked() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_pinned_commit(home.path(), repo.path(), "a\nb\nc\n");

    let json = rift(home.path(), repo.path(), &["why", "f.rs:2", "--json"]);
    assert_eq!(code(&json), 0, "{json:?}");
    let got: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let expected = the_locked_why_document();
    assert_eq!(got, expected, "the rift why --json contract moved");

    // The flag spelling answers the same line the colon spelling did.
    let flagged = rift(
        home.path(),
        repo.path(),
        &["why", "f.rs", "--line", "2", "--json"],
    );
    let same: serde_json::Value = serde_json::from_slice(&flagged.stdout).unwrap();
    assert_eq!(same, expected, "colon and flag spellings must agree");

    // The human render teaches the policy-gated ladder.
    let human = rift(home.path(), repo.path(), &["why", "f.rs:2"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("recall retrieve-exact"), "{text}");

    // A range is a refusal with the single-line sentence, in both spellings.
    for args in [
        &["why", "f.rs:2-3"] as &[&str],
        &["why", "f.rs", "--line", "2-3"],
    ] {
        let out = rift(home.path(), repo.path(), args);
        assert_ne!(code(&out), 0, "{out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("single line"), "{err}");
    }
}

/// `rift checkpoint explain`'s refusal contracts, locked: no claimed
/// checkpoint at HEAD, an unknown id, and the flag exclusion — each says
/// exactly why, in the machine shape an agent scripts against. (The
/// affirmative render over a REAL shard is pinned in `hooks_archive_e2e`, where an
/// archive exists to explain.)
#[test]
fn checkpoint_explain_refuses_cleanly_without_a_checkpoint() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_pinned_commit(home.path(), repo.path(), "a\n");

    let out = rift(
        home.path(),
        repo.path(),
        &["checkpoint", "explain", "--json"],
    );
    assert_eq!(code(&out), 4, "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "error": "no_checkpoint", "target": "HEAD" })
    );

    let miss = rift(
        home.path(),
        repo.path(),
        &["checkpoint", "explain", "deadbeef1", "--json"],
    );
    assert_eq!(code(&miss), 4);
    let m: serde_json::Value = serde_json::from_slice(&miss.stdout).unwrap();
    assert_eq!(m["error"], "no_checkpoint");
    assert_eq!(m["target"], "deadbeef1");

    let both = rift(
        home.path(),
        repo.path(),
        &["checkpoint", "explain", "--full", "--short"],
    );
    assert_ne!(code(&both), 0, "{both:?}");
}

/// A provenance tool that silently answers "nothing" is worse than one that
/// errors, and that is what a sha256 repository used to get: the parser
/// accepted 40-character oids only, so every porcelain header missed and a
/// three-line, one-commit file printed "0 lines across 0 commits".
#[test]
fn blame_answers_a_sha256_repository_rather_than_reporting_it_empty() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let init = git(
        home.path(),
        repo.path(),
        &["init", "-q", "--object-format=sha256", "."],
    );
    assert!(
        init.status.success(),
        "git must support --object-format=sha256 (2.29+): {init:?}"
    );
    std::fs::write(repo.path().join("f.rs"), "a\nb\nc\n").unwrap();
    commit_worktree(home.path(), repo.path());
    rift_dir(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["blame", "f.rs"]);
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("3 lines across 1 commit;"),
        "sha256 blame: {stdout}"
    );
    // The oid is 64 wide and the span is real, not the all-zero placeholder.
    let json = rift(home.path(), repo.path(), &["blame", "f.rs", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let commit = v["spans"][0]["commit"].as_str().unwrap().to_owned();
    assert_eq!(commit.len(), 64, "sha256 oid width");
    assert!(commit.bytes().any(|b| b != b'0'), "not the null oid");
    assert_eq!(v["lines"], 3);
}

/// What `core.quotePath=false` buys, stated as a fact about the output.
///
/// git's default c-quotes every byte ≥ 0x80, so `café.rs` arrives from the
/// porcelain as `"caf\303\251.rs"` — which matches nothing the projection
/// recorded, so every non-ASCII path silently loses its file evidence and
/// degrades to a guess. The path this verb reports is the one the repository
/// itself spells, and `git ls-files` is asked what that is rather than assumed,
/// because macOS may store it in a different normal form than the argv used.
#[test]
fn blame_reports_a_non_ascii_path_as_the_repository_spells_it() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(home.path(), repo.path());
    std::fs::write(repo.path().join("café.rs"), "x\n").unwrap();
    commit_worktree(home.path(), repo.path());
    rift_dir(home.path(), repo.path());

    let listed = git(
        home.path(),
        repo.path(),
        &["-c", "core.quotePath=false", "ls-files"],
    );
    let recorded = String::from_utf8_lossy(&listed.stdout).trim().to_owned();
    let out = rift(home.path(), repo.path(), &["blame", &recorded, "--json"]);
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let path = v["spans"][0]["path"].as_str().unwrap();
    assert_eq!(path, recorded, "the repository's own spelling");
    assert!(!path.starts_with('"'), "not c-quoted: {path}");
    assert!(!path.contains("\\3"), "no octal escapes: {path}");
}

/// The code points `paths_contain` cannot tell from nothing — exactly
/// `char::is_whitespace`, and exactly the trailing-collision class swept in
/// `bins/rift/src/provenance.rs`.
#[cfg(unix)]
const COLLIDING_WHITESPACE: [char; 25] = [
    '\u{9}', '\u{a}', '\u{b}', '\u{c}', '\u{d}', '\u{20}', '\u{85}', '\u{a0}', '\u{1680}',
    '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
    '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}',
];

/// The `filename` value the porcelain parser would see for `name` under
/// `core.quotePath=<setting>` — taken from git rather than predicted, which is
/// the whole point of the sweeps below.
#[cfg(unix)]
fn porcelain_filename_header(home: &Path, repo: &Path, setting: &str, name: &str) -> String {
    let flag = format!("core.quotePath={setting}");
    let out = git(
        home,
        repo,
        &["-c", &flag, "blame", "--line-porcelain", "--", name],
    );
    assert!(out.status.success(), "blame {name:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("filename ").map(ToOwned::to_owned))
        .expect("a filename header")
}

/// One tracked file per colliding code point, each in a directory of its own.
/// On a normalization-insensitive filesystem (APFS) `U+2000` folds onto
/// `U+2002` and `U+2001` onto `U+2003` inside a single directory, so two of
/// the 25 would silently become one file and the sweep would be measuring the
/// filesystem instead of git.
#[cfg(unix)]
fn one_directory_per_colliding_name(repo: &Path) -> Vec<(char, String)> {
    let mut names = Vec::new();
    for (i, c) in COLLIDING_WHITESPACE.iter().enumerate() {
        let dir = format!("d{i:02}");
        std::fs::create_dir(repo.join(&dir)).unwrap();
        let name = format!("{dir}/a.rs{c}");
        std::fs::write(repo.join(&name), "one\n").unwrap();
        names.push((*c, name));
    }
    names
}

/// The code points whose names survive the porcelain unchanged under this
/// setting — the ones that reach the parser raw.
#[cfg(unix)]
fn names_arriving_raw(
    home: &Path,
    repo: &Path,
    names: &[(char, String)],
    setting: &str,
) -> Vec<char> {
    names
        .iter()
        .filter(|(_, n)| porcelain_filename_header(home, repo, setting, n) == *n)
        .map(|(c, _)| *c)
        .collect()
}

/// **The cost of `core.quotePath=false`, made executable.** `blame` passes that
/// flag so a non-ASCII path stops losing its file evidence; the flag is also
/// what lets 19 of the 25 matcher-collision code points reach the porcelain
/// parser raw. The first version of this note said the opposite — that those 19
/// predated the verb — so the number is a test now rather than a sentence.
///
/// Measured here on whatever git runs the suite, both settings, one repository:
///
/// | | raw at the parser |
/// |---|---|
/// | `core.quotePath=true` (git's default) | 1 — `U+0020` |
/// | `core.quotePath=false` (what `blame` passes) | 20 |
/// | reachable *only* because of the flag | **19** |
///
/// The five that never arrive raw are the C0 whitespace, which git c-quotes
/// whatever the setting. Unix only: `TAB`, `LF` and `CR` cannot be filenames on
/// Windows, so the sweep could only be partial there.
#[cfg(unix)]
#[test]
fn the_quote_path_flag_this_verb_adds_is_what_makes_nineteen_collisions_reachable() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(home.path(), repo.path());
    let names = one_directory_per_colliding_name(repo.path());
    commit_worktree(home.path(), repo.path());

    let by_default = names_arriving_raw(home.path(), repo.path(), &names, "true");
    let with_the_flag = names_arriving_raw(home.path(), repo.path(), &names, "false");
    assert_eq!(
        by_default,
        vec!['\u{20}'],
        "git's default lets exactly one of the 25 through raw"
    );
    assert_eq!(
        with_the_flag.len(),
        20,
        "the flag lets 20 through: {with_the_flag:?}"
    );
    let created_here: Vec<char> = with_the_flag
        .iter()
        .copied()
        .filter(|c| !by_default.contains(c))
        .collect();
    assert_eq!(
        created_here.len(),
        19,
        "19 of the 20 are reachable only because this verb passes the flag: \
         {created_here:?}"
    );

    // The five that are never raw are the C0 whitespace, and they are *quoted*
    // rather than merely different — asserted positively, because "not equal to
    // the name" would also be true if git had mangled them some other way.
    let never_raw: Vec<char> = COLLIDING_WHITESPACE
        .iter()
        .copied()
        .filter(|c| !with_the_flag.contains(c))
        .collect();
    assert_eq!(never_raw, vec!['\u{9}', '\u{a}', '\u{b}', '\u{c}', '\u{d}']);
    for (c, name) in &names {
        for setting in ["true", "false"] {
            let got = porcelain_filename_header(home.path(), repo.path(), setting, name);
            if never_raw.contains(c) {
                assert!(
                    got.starts_with('"') && got.ends_with('"'),
                    "U+{:04X} under {setting}: {got:?}",
                    *c as u32
                );
            }
        }
    }
}

/// The dependency the backslash rule rests on, asserted against git rather
/// than read out of its documentation.
///
/// The parser refuses a `"` or a `\` in an **unquoted** `filename` value on the
/// grounds that git escapes both unconditionally — the same standing it gives
/// the C0 class, and the reason it is not an asymmetry any more. If that ever
/// stopped being true, the refusal would start costing real evidence silently,
/// so it is measured under both settings of the flag `blame` passes.
///
/// The backslash is not only a framing question: the matcher folds `\` to `/`,
/// so an unquoted `src/b\c.rs` would read as the different, real `src/b/c.rs`.
#[cfg(unix)]
#[test]
fn git_escapes_a_quote_and_a_backslash_whatever_core_quote_path_says() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(home.path(), repo.path());
    std::fs::create_dir(repo.path().join("src")).unwrap();
    for name in ["src/b\\c.rs", "src/d\"e.rs", "src/plain.rs"] {
        std::fs::write(repo.path().join(name), "one\n").unwrap();
    }
    commit_worktree(home.path(), repo.path());

    for setting in ["true", "false"] {
        let header =
            |name: &str| porcelain_filename_header(home.path(), repo.path(), setting, name);
        assert_eq!(
            header("src/b\\c.rs"),
            "\"src/b\\\\c.rs\"",
            "a literal backslash under core.quotePath={setting}"
        );
        assert_eq!(
            header("src/d\"e.rs"),
            "\"src/d\\\"e.rs\"",
            "a literal quote under core.quotePath={setting}"
        );
        // The control: an ordinary name is not quoted, so "everything is
        // quoted" is not what the two assertions above are observing.
        assert_eq!(header("src/plain.rs"), "src/plain.rs");
    }
}

/// The parse boundary's one dependency on git, asserted against git.
///
/// A filename is attacker-controlled in any repository you clone. This one is
/// *named* a newline followed by a plausible blame row, so if the porcelain
/// carried it raw the name would spell a second `filename` header of its own —
/// choosing the path that file evidence is matched against — and if the header
/// carried it raw the row would print above the real ones. git c-quotes a
/// pathname holding a control character unconditionally (`core.quotePath`
/// governs only bytes ≥ 0x80), which is what makes the first half safe, and
/// `safe_path` makes the second half safe. Unix only: a name like this cannot
/// exist on Windows, so there the test could only be unfalsifiable.
///
/// This one pins the **outcome**; the test after it pins the **dependency**.
/// The distinction is not pedantry: this name ends in an `ESC`, so the parser's
/// own fail-safe fires and every assertion below would still hold under a git
/// that had stopped quoting.
#[cfg(unix)]
#[test]
fn a_filename_that_spells_a_blame_row_forges_neither_a_row_nor_a_header() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(home.path(), repo.path());
    let evil = "a.rs\n          1-9  0123456789ab  ckpt cafecafecafe  ai  \
                trusted-model  conf 1.000  file-touched\nfilename hijacked.rs\u{1b}[31m";
    std::fs::write(repo.path().join(evil), "one\n").unwrap();
    commit_worktree(home.path(), repo.path());
    rift_dir(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["blame", "--", evil]);
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // One header, one span (the file has one line), one summary — and the
    // trailing newline `emit` adds. A forged row can only exist as an extra.
    assert_eq!(stdout.lines().count(), 3, "forged row: {stdout:?}");
    assert!(!stdout.contains('\u{1b}'), "raw ESC: {stdout:?}");
    assert!(
        stdout.contains("1 line across 1 commit;"),
        "one real line: {stdout}"
    );
    // The header quotes the name; the hijacked path never became one.
    assert!(stdout.starts_with("rift blame \"a.rs\\n"), "{stdout:?}");
    // `--json` before the `--`: after it, clap reads it as a second path.
    let json = rift(home.path(), repo.path(), &["blame", "--json", "--", evil]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let spans = v["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1, "one span, not one per forged header");
    assert_ne!(spans[0]["path"], "hijacked.rs");
}

/// The same threat with the fail-safe taken away, so only git's quoting is left.
///
/// Every segment of this name after the newline is ordinary text, so the
/// parser's control-character check cannot fire on it at all: if git handed the
/// name over raw, the porcelain would carry a second, fully ordinary-looking
/// `filename hijacked.rs` header, and the span would be matched against
/// `hijacked.rs` — a path the attacker chose, read as recorded evidence. What
/// prevents that is that git c-quotes the **whole** pathname onto one line, and
/// that is what this asserts: the span carries the quoted spelling, exactly.
///
/// The three outcomes are distinguishable, which is the point: `"b.rs\nfilename
/// hijacked.rs"` (quoted, today), `b.rs` (unquoted, split at the newline) and
/// `hijacked.rs` (unquoted, second header wins). Only the first passes.
#[cfg(unix)]
#[test]
fn the_span_path_depends_on_git_quoting_the_whole_name_not_on_the_fail_safe() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(home.path(), repo.path());
    let evil = "b.rs\nfilename hijacked.rs";
    std::fs::write(repo.path().join(evil), "one\n").unwrap();
    commit_worktree(home.path(), repo.path());
    rift_dir(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["blame", "--json", "--", evil]);
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let spans = v["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1, "one span, not one per forged header");
    assert_eq!(
        spans[0]["path"], "\"b.rs\\nfilename hijacked.rs\"",
        "git must c-quote the whole name onto one porcelain line"
    );
    assert_ne!(spans[0]["path"], "hijacked.rs", "the attacker's path");
    assert_ne!(spans[0]["path"], "b.rs", "the truncated path");
    // And the human render is still three lines with nothing raw in them.
    let human = rift(home.path(), repo.path(), &["blame", "--", evil]);
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert_eq!(stdout.lines().count(), 3, "forged row: {stdout:?}");
}

/// A C1 control is not a broken frame, and treating it as one threw evidence
/// away.
///
/// `core.quotePath=false` asks git to hand back bytes ≥ 0x80 raw — that is the
/// whole reason `café.rs` resolves — and `U+0080..=U+009F` are ≥ 0x80. The
/// parser used to test `char::is_control`, which is true for them, so a path git
/// had spelled correctly and in full lost its name and every span of that file
/// degraded to `path-excluded (guess)`. Fail-safe in direction, wrong about the
/// class, and silent. The fail-safe now fires on ASCII controls only — the class
/// git quotes whatever `core.quotePath` says.
#[cfg(unix)]
#[test]
fn a_c1_control_in_a_filename_keeps_the_path_git_spelled() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(home.path(), repo.path());
    std::fs::write(repo.path().join("g\u{85}.rs"), "one\ntwo\n").unwrap();
    commit_worktree(home.path(), repo.path());
    rift_dir(home.path(), repo.path());

    // The repository's own spelling, asked for rather than assumed.
    let listed = git(
        home.path(),
        repo.path(),
        &["-c", "core.quotePath=false", "ls-files"],
    );
    let recorded = String::from_utf8_lossy(&listed.stdout)
        .trim_end_matches('\n')
        .to_owned();
    assert!(recorded.contains('\u{85}'), "fixture name: {recorded:?}");

    let out = rift(
        home.path(),
        repo.path(),
        &["blame", "--json", "--", &recorded],
    );
    assert_eq!(code(&out), 0, "blame exits 0: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["spans"][0]["path"], recorded,
        "a C1 path keeps the name git spelled it with"
    );
    assert_eq!(v["lines"], 2);
}

/// A repository tracking two files whose names ARE flags — `-w`, a real
/// `git blame` option, and `--evil.rs`, not an option at all — beside an
/// ordinary one. Written through the filesystem, never through an argv, so
/// the fixture cannot be an artefact of git's own argument parsing.
fn seed_flag_named_files(home: &Path, repo: &Path) {
    init_repo(home, repo);
    std::fs::write(repo.join("real.rs"), "a\nb\n").unwrap();
    std::fs::write(repo.join("-w"), "a\nb\n").unwrap();
    std::fs::write(repo.join("--evil.rs"), "p\nq\nr\n").unwrap();
    commit_worktree(home, repo);
    rift_dir(home, repo);
}

/// The route itself, and the one argument shape the `--` terminator decides.
///
/// **What this pins.** The contract: every flag-shaped and revision-shaped
/// argument is answered as a *path* — exit 4, `not_blameable`, the file reported
/// exactly as asked rather than as parsed — so a future change that resolves a
/// revision, that lets clap swallow `-h` before the verb sees it, or that echoes
/// a parsed spelling, turns this red.
///
/// **And the terminator, which the first cut of this test wrongly said no
/// assertion could distinguish.** That measurement swept eleven argv shapes, but
/// every one of them named a path *that does not exist*, so both arms failed by
/// construction and the comparison was vacuous. `--` can only matter for a path
/// that is really tracked and is really spelled like a flag, so this repository
/// tracks two of them: `-w` (a real `git blame` option) and `--evil.rs` (not an
/// option at all). Measured on git 2.50.1 — with the terminator both exit 0;
/// without it `-w` is consumed as the whitespace option and git exits **129**
/// for want of a path, and `--evil.rs` exits 129 as `error: unknown option`.
/// Two different mechanisms, one outcome, and the earlier claim that git fails
/// "for want of a second argument" was only ever half the story. Delete
/// `"--".to_owned()` from `blame`'s argv and the two assertions below fail;
/// before this test existed, deleting it failed nothing in the whole workspace.
#[test]
fn a_flag_shaped_argument_is_a_path_and_the_terminator_is_what_makes_it_one() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_flag_named_files(home.path(), repo.path());

    // THE TERMINATOR. A tracked file named exactly like an option is blamed as
    // the file it is, for both mechanisms git has for refusing one.
    let opt = rift(home.path(), repo.path(), &["blame", "--", "-w"]);
    assert_eq!(code(&opt), 0, "`-w` is a tracked file here: {opt:?}");
    assert!(
        String::from_utf8_lossy(&opt.stdout).contains("2 lines across 1 commit;"),
        "{opt:?}"
    );
    let unknown = rift(home.path(), repo.path(), &["blame", "--", "--evil.rs"]);
    assert_eq!(
        code(&unknown),
        0,
        "`--evil.rs` is tracked here: {unknown:?}"
    );
    assert!(
        String::from_utf8_lossy(&unknown.stdout).contains("3 lines across 1 commit;"),
        "{unknown:?}"
    );

    // A flag-shaped path that is *not* tracked is still answered as a path.
    let missing = rift(home.path(), repo.path(), &["blame", "--", "--missing.rs"]);
    assert_eq!(code(&missing), 4, "`--missing.rs` is a path: {missing:?}");
    let json = rift(
        home.path(),
        repo.path(),
        &["blame", "--json", "--", "--missing.rs"],
    );
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["error"], "not_blameable");
    assert_eq!(
        v["file"], "--missing.rs",
        "reported as asked, not as parsed"
    );

    // Revisions are paths here too: `HEAD` names no file in this worktree, so
    // the answer is exit 4 rather than a blame of the commit it resolves to.
    for arg in [
        "HEAD",
        "main",
        "@",
        "HEAD~1",
        "-h",
        "--help",
        "--version",
        "-",
        "--",
        "--reverse",
    ] {
        let out = rift(home.path(), repo.path(), &["blame", "--", arg]);
        assert_eq!(code(&out), 4, "`{arg}` must be a path, got {out:?}");
    }
    // A real path still answers.
    let ok = rift(home.path(), repo.path(), &["blame", "--", "real.rs"]);
    assert_eq!(code(&ok), 0, "a tracked path answers: {ok:?}");
    assert!(String::from_utf8_lossy(&ok.stdout).contains("2 lines across 1 commit;"));
    // …and without the `--` too, for the ordinary spelling.
    assert_eq!(
        code(&rift(home.path(), repo.path(), &["blame", "real.rs"])),
        0
    );
    // No path at all is a usage error, not a panic or an empty answer.
    assert_eq!(code(&rift(home.path(), repo.path(), &["blame"])), 2);
    // An empty path is refused before git is reached.
    assert_eq!(
        code(&rift(home.path(), repo.path(), &["blame", "--", ""])),
        1
    );
}

/// `blame` is a projection query, so it answers the init guard like the rest:
/// a repository Rift has never seen is exit 3, not an empty blame.
#[test]
fn blame_in_a_non_rift_repo_exits_not_init() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(home.path(), repo.path());
    std::fs::write(repo.path().join("real.rs"), "a\n").unwrap();
    let out = rift(home.path(), repo.path(), &["blame", "real.rs"]);
    assert_eq!(code(&out), 3, "no `.rift/` is exit 3: {out:?}");
}
