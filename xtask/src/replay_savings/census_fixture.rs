//! The census corpus: a second session tree the first fixture's test never
//! walks, replayed in both fact modes against a repository whose history it
//! builds itself.
//!
//! It exercises what the first corpus cannot reach cheaply — every
//! machine-format class, the size bands, the four inheritance routes, a path
//! only a commit still has, a path only the worktree has, and a window older
//! than every commit — so each has a pinned count.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::artifact::{write_artifact, Provenance};
use super::bucket::Bucket;
use super::inheritance::Inheritance;
use super::render::{render_json, render_table};
use super::replay::replay;
use super::report::Report;
use super::tests::SENTINEL;
use super::Options;

/// The first commit, 2026-01-15, which every window of the main tree began
/// after.
const FIRST_COMMIT: i64 = 1_768_435_200;
/// The second, 2026-03-01, after every window: the replay must not reach it.
const SECOND_COMMIT: i64 = 1_772_323_200;

/// `n` lines of at least `width` bytes each, the newline included: a line
/// is its number and padding, so a long file's later lines run wider.
fn lines(n: usize, width: usize) -> String {
    (0..n).fold(String::new(), |mut out, i| {
        let body = format!("{i} ");
        let _ = writeln!(out, "{body:x<w$}", w = width.saturating_sub(1));
        out
    })
}

/// Run git in `repo` with no configuration of this machine's reaching it.
fn git(repo: &Path, args: &[&str], at: i64) {
    let date = format!("@{at} +0000");
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?}: {status:?}");
}

/// The repository the census corpus is replayed against: one commit with a
/// file the worktree has since lost, and one after every window adding the
/// file the first commit lacks.
fn census_repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("census");
    let write = |rel: &str, body: &str| {
        let path = repo.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    let shared = (1..=60).fold(String::new(), |mut out, i| {
        let _ = writeln!(out, "shared line {i} of the census corpus");
        out
    });
    write("src/shared.rs", &shared);
    write("Cargo.lock", &lines(40, 12));
    write("web/app.min.js", "var a=1;\n");
    write("nb/a.ipynb", &lines(30, 20));
    write("img/logo.svg", &lines(5, 20));
    write("data/t.csv", &lines(10, 8));
    write("src/big.rs", &lines(1_600, 2));
    write("docs/guide.md", &lines(500, 2));
    write("conf/wide.toml", &lines(200, 100));
    write("src/huge.rs", &lines(1_000, 70));
    git(&repo, &["init", "-q"], FIRST_COMMIT);
    git(&repo, &["add", "-A"], FIRST_COMMIT);
    git(&repo, &["commit", "-q", "-m", "first"], FIRST_COMMIT);
    std::fs::remove_file(repo.join("data/t.csv")).unwrap();
    write("src/new.rs", &lines(20, 10));
    git(&repo, &["add", "-A"], SECOND_COMMIT);
    git(&repo, &["commit", "-q", "-m", "second"], SECOND_COMMIT);
    repo
}

/// The census corpus's options, in worktree mode or at each window's commit.
fn options(repo: &Path, history: bool) -> Options {
    Options {
        transcripts: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("replay-savings")
            .join("census"),
        repo: repo.to_path_buf(),
        json: false,
        history: history.then(|| repo.join(".git")),
        history_refs: vec!["HEAD".to_owned()],
        artifact: None,
    }
}

/// The census corpus replayed both ways.
fn replayed() -> (Report, Report) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = census_repo(tmp.path());
    let worktree = replay(&options(&repo, false)).unwrap();
    let history = replay(&options(&repo, true)).unwrap();
    (worktree, history)
}

#[test]
fn the_census_pins_every_machine_format_class() {
    let (worktree, history) = replayed();
    let row = |report: &Report, key: &str| report.machine_formats.get(key).copied().unwrap();
    for key in ["lockfile", "minified", "svg"] {
        assert_eq!(row(&history, key).all.calls, 1, "{key}");
        assert_eq!(row(&history, key).first_sight.calls, 1, "{key} first sight");
    }
    assert_eq!(row(&history, "notebook").all.calls, 1, "a notebook read");
    assert_eq!(
        row(&history, "tabular").first_sight.calls,
        1,
        "at its commit"
    );
    assert_eq!(row(&worktree, "tabular").all.calls, 1, "counted by shape");
    assert_eq!(
        row(&worktree, "tabular").first_sight.calls,
        0,
        "deleted since, so no first sight in the worktree"
    );
}

#[test]
fn the_census_pins_the_first_sight_sizes() {
    let (_, history) = replayed();
    let sizes = history.first_sight_sizes;
    let calls = |tallies: &[super::report::Tally]| -> Vec<u64> {
        tallies.iter().map(|tally| tally.calls).collect()
    };
    assert_eq!(calls(&sizes.by_lines), [12, 1, 1, 1, 1], "line bands");
    assert_eq!(calls(&sizes.by_bytes), [13, 1, 1, 1], "byte bands");
    assert_eq!(sizes.first_contact.calls, 4, "either bound");
    assert_eq!(sizes.first_contact_code.calls, 2, "code");
    assert_eq!(sizes.first_contact_markdown.calls, 1, "Markdown");
    assert_eq!(sizes.first_contact_toml.calls, 1, "TOML");
}

#[test]
fn the_census_pins_each_inheritance_route() {
    let (worktree, history) = replayed();
    for report in [&worktree, &history] {
        let class = |class: Inheritance| report.sibling_by_inheritance[class.index()].calls;
        assert_eq!(
            report.cross_window_pairs, 1,
            "one revision read by five windows"
        );
        assert_eq!(report.cross_window.calls, 4, "four later readers");
        assert_eq!(class(Inheritance::Fork), 1, "fork wins over the name");
        assert_eq!(class(Inheritance::Paste), 1, "paste wins over the name");
        assert_eq!(class(Inheritance::ByName), 1, "named only");
        assert_eq!(class(Inheritance::None), 1, "nothing handed down");
        assert_eq!(
            report.sibling_named_at_spawn.calls, 0,
            "the shared revision is the oldest of eleven the coordinator read \
             before the spawns, past the eight newest the section names"
        );
        let routes = report.sibling_by_inheritance;
        assert_eq!(
            routes.iter().map(|tally| tally.calls).sum::<u64>(),
            report.cross_window.calls,
            "the routes split the later windows' first reads, call for call"
        );
        assert_eq!(
            routes.iter().map(|tally| tally.tokens).sum::<u64>(),
            report.cross_window.tokens,
            "and token for token"
        );
    }
}

#[test]
fn the_census_pins_where_the_facts_came_from() {
    let (worktree, history) = replayed();
    assert!(!worktree.facts.history);
    assert_eq!(
        worktree.bucket(Bucket::NoLineCount).calls,
        1,
        "deleted since"
    );
    assert!(history.facts.history);
    assert_eq!(
        history.bucket(Bucket::NoLineCount).calls,
        0,
        "at its commit"
    );
    assert_eq!(
        history.facts.resolved_at_a_commit, 10,
        "every path the first commit has"
    );
    assert_eq!(
        history.facts.fell_back_to_the_worktree, 1,
        "added after the window began"
    );
    assert_eq!(
        history.facts.windows_without_a_commit, 1,
        "older than every commit"
    );
}

#[test]
fn a_census_run_publishes_no_path_and_no_prompt() {
    let (worktree, history) = replayed();
    let tmp = tempfile::tempdir().unwrap();
    let provenance = Provenance {
        describe: "v0-1-gabc1234".to_owned(),
        head: "abc1234".to_owned(),
        corpus_files: 12,
        history_refs: vec!["HEAD".to_owned()],
        started: 1_000,
        finished: 1_060,
    };
    let (table, document) = write_artifact(tmp.path(), &history, &provenance).unwrap();
    assert!(table.ends_with("replay-savings-1000.md"));
    let written = [
        std::fs::read_to_string(&table).unwrap(),
        std::fs::read_to_string(&document).unwrap(),
    ];
    let rendered = [render_table(&worktree), render_json(&worktree)];
    for text in written.iter().chain(&rendered) {
        assert!(!text.contains(SENTINEL), "a prompt reached the output");
        for fragment in [
            "shared",
            "Cargo.lock",
            ".csv",
            "guide",
            "/repo",
            "census corpus",
            "/Users/",
            "~/",
            "fixtures",
        ] {
            assert!(!text.contains(fragment), "`{fragment}` reached the output");
        }
    }
    assert!(written[0].contains("v0-1-gabc1234"), "the build is named");
    assert!(
        written[0].contains("12 transcript files, 2 session trees"),
        "the corpus is described by its counts"
    );
    assert!(
        written[0].contains("1970-01-01T00:16:40Z (1000)"),
        "the start"
    );
    assert!(written[0].contains("facts from: history at each window's commit"));
    let parsed: serde_json::Value = serde_json::from_str(&written[1]).unwrap();
    assert_eq!(parsed["provenance"]["finished_unix_seconds"], 1_060);
    assert_eq!(parsed["facts"]["mode"], "history");
}
