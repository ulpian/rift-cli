//! The search corpus and what a session over it costs with and without the
//! levers — the fixture behind the nine-tenths bar for searches.
//!
//! **Everything here is generated, and generated in Rust.** The repository is
//! a pure function of this file: five thousand paths with the depth and the
//! extensions a real checkout has, the same on every machine and every
//! platform, with nothing committed for a reviewer to take on trust. A search
//! fixture has to be big or it measures nothing — an answer that names eight
//! paths out of ten is not a saving, and the gate this file feeds stands an
//! answer down whenever the frame would cost more than the listing it stands
//! in for.
//!
//! **What is priced here, and what is deliberately not.** Rift's search levers
//! act on two shapes and only two: a search for file *names* (a glob, or the
//! `find … -name` listing grammar) is answered from the index Rift already
//! holds, and a search inside file *contents* is given the result cap its own
//! tool documents. Only the first has a pool: the listing the model would have
//! received is a set of paths this tree can count with the tokenizer of
//! record. A content grep has no declared native cost anywhere in this
//! repository — its output is every matching line of every matching file — so
//! it is never priced, never substituted, and never counted into a share.
//! What the cap publishes instead is its own honesty figure: how often a
//! bounded search was asked again in the turn it was bounded in, which is the
//! rate at which the lever sends the model straight back for the rest.
//!
//! **The pinned baselines, one per shape.** One function prices a listing —
//! `rift_recall_serve::declared_search_baseline` — and this file holds it
//! against the artifacts it is derived from: the hundred-file cap a glob stops
//! at and the roughly thirty-thousand-character ceiling a command's result is
//! read back inline up to are both read out of the committed Claude Code
//! tool-field fixture, the formula is restated here as a listing of paths one
//! per line cut at whichever of the two applies, and every figure the product
//! books for a search in the session below is compared with the figure this
//! fixture computes for the same search from the corpus it wrote.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

mod common;

// ----------------------------------------------------------- the generator ---

/// A tiny deterministic source of variety, so two files are not the same file
/// and no run is a different corpus from the last.
///
/// A linear congruential generator with the constants Numerical Recipes
/// publishes: nothing here is cryptography, and the only property that matters
/// is that it is a pure function of its seed on every platform.
struct Noise(u64);

impl Noise {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 17
    }

    fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
        &from[(self.next() as usize) % from.len()]
    }
}

/// The package names the generated crates carry.
///
/// Trees, and deliberately not the names of anything in this repository: a
/// corpus that borrowed real crate names would read as a claim about them.
const PACKAGES: [&str; 20] = [
    "alder", "birch", "cedar", "dogwood", "elm", "fir", "gum", "hazel", "ilex", "juniper",
    "katsura", "larch", "maple", "nutmeg", "oak", "pine", "quince", "rowan", "spruce", "teak",
];

/// The areas a generated package is divided into.
const AREAS: [&str; 6] = ["core", "engine", "index", "io", "model", "render"];

/// The screens the generated front end has.
const SCREENS: [&str; 8] = [
    "account",
    "billing",
    "dashboard",
    "editor",
    "history",
    "search",
    "settings",
    "workspace",
];

/// The chapters the generated documentation has.
const DOC_AREAS: [&str; 6] = [
    "architecture",
    "concepts",
    "guides",
    "operations",
    "reference",
    "tutorials",
];

/// Nouns the generated file names and bodies are built from.
const NOUNS: [&str; 16] = [
    "buffer", "cursor", "handle", "ledger", "marker", "parser", "queue", "record", "scanner",
    "session", "shard", "stream", "table", "token", "walker", "window",
];

/// Verbs the generated bodies are built from.
const VERBS: [&str; 8] = [
    "append", "collect", "flush", "merge", "resolve", "rewind", "scan", "seal",
];

/// The literal a content search in the session below looks for.
///
/// A word, in the bodies of files, which is the one thing a name index can
/// never answer — the reason the cap exists and the answer does not.
const REVIEW_MARKER: &str = "needs_review";

/// One generated file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GenFile {
    /// Repository-relative, `/`-spelled.
    path: String,
    /// A small plausible body. Never large: this fixture prices path listings,
    /// and a corpus whose bytes dominated its own generation would cost the
    /// suite minutes to say nothing.
    body: String,
    /// Whether this repository's own sensitive-path rules keep it out of the
    /// index altogether — a fact asserted below rather than assumed.
    sensitive: bool,
}

/// The generated repository: five thousand paths with the shape of a checkout
/// somebody works in.
fn corpus() -> Vec<GenFile> {
    let mut noise = Noise::new(0x5EA4_C401_1234_5678);
    let mut files: Vec<GenFile> = Vec::with_capacity(CORPUS_FILES);
    sources(&mut noise, &mut files);
    the_rest(&mut noise, &mut files);
    files
}

/// Add one generated file, classified against this repository's own built-in
/// sensitive-path rules as it goes in.
fn add(files: &mut Vec<GenFile>, path: String, body: String) {
    let sensitive = rift_config::BUILTIN_SENSITIVE_PATHS
        .iter()
        .any(|glob| rift_config::glob_match(glob, &path));
    files.push(GenFile {
        path,
        body,
        sensitive,
    });
}

/// The 4,420 sources: libraries, their suites, the binaries, the front end.
fn sources(noise: &mut Noise, files: &mut Vec<GenFile>) {
    let mut push = |path: String, body: String| add(files, path, body);

    // 3,600 library sources, four deep, the bulk of any checkout.
    for package in PACKAGES {
        for area in AREAS {
            for unit in 0..30 {
                let noun = noise.pick(&NOUNS);
                let verb = noise.pick(&VERBS);
                let review = if unit % 5 == 0 {
                    format!("    // {REVIEW_MARKER}: the {noun} bound is a guess\n")
                } else {
                    String::new()
                };
                push(
                    format!("crates/{package}/src/{area}/{noun}_{unit:02}.rs"),
                    format!(
                        "//! The {area} {noun}.\n\npub struct {noun}_{unit:02};\n\n\
                         impl {noun}_{unit:02} {{\n{review}    pub fn {verb}(&self) -> usize {{\n\
                         \x20       {unit}\n    }}\n}}\n"
                    ),
                );
            }
        }
    }
    // 120 test suites beside them.
    for package in PACKAGES {
        for area in AREAS {
            push(
                format!("crates/{package}/tests/{area}_suite.rs"),
                format!("#[test]\nfn the_{area}_holds() {{\n    assert!(true);\n}}\n"),
            );
        }
    }
    // 300 binary sources.
    for bin in ["rift", "importer", "packer", "prober", "watcher"] {
        for unit in 0..60 {
            let noun = noise.pick(&NOUNS);
            push(
                format!("bins/{bin}/src/{noun}_{unit:02}.rs"),
                format!("fn {noun}_{unit:02}() -> &'static str {{\n    \"{bin}\"\n}}\n"),
            );
        }
    }
    // 400 front-end sources, two files per component.
    for screen in SCREENS {
        for unit in 0..25 {
            let noun = noise.pick(&NOUNS);
            push(
                format!("web/app/{screen}/{noun}_{unit:02}.tsx"),
                format!("export function {noun}{unit:02}() {{\n  return null;\n}}\n"),
            );
            push(
                format!("web/app/{screen}/{noun}_{unit:02}.module.css"),
                format!(".{noun}_{unit:02} {{\n  display: flex;\n}}\n"),
            );
        }
    }
}

/// The 580 that are not code: the prose, the denied subtree, the over-cap
/// family, the odds and ends, and the five nobody indexes.
fn the_rest(noise: &mut Noise, files: &mut Vec<GenFile>) {
    let mut push = |path: String, body: String| add(files, path, body);

    // 240 chapters of prose.
    for area in DOC_AREAS {
        for unit in 0..40 {
            let noun = noise.pick(&NOUNS);
            push(
                format!("docs/{area}/{noun}-{unit:02}.md"),
                format!("# The {noun}\n\nWhat the {area} {noun} is for.\n"),
            );
        }
    }
    // 120 sources under a directory this repository's policy denies, so the
    // screen has something real to keep out of both the frame and the pool.
    for area in ["compliance", "finance", "legal", "pricing"] {
        for unit in 0..30 {
            let noun = noise.pick(&NOUNS);
            push(
                format!("internal/{area}/{noun}_{unit:02}.rs"),
                format!("// {area}\npub const {noun}_{unit:02}: u32 = {unit};\n"),
            );
        }
    }
    // 137 generated sources whose names are all the same length, so any
    // hundred of them is the same listing to the token: the one search here
    // that goes over the tool's own cap, priced exactly rather than bounded.
    for unit in 0..137 {
        push(
            format!("services/api/gen/case_{unit:03}.rs"),
            format!("pub const CASE_{unit:03}: u32 = {unit};\n"),
        );
    }
    // 78 files of the odds and ends every repository has.
    for unit in 0..20 {
        push(
            format!("config/service_{unit:02}.toml"),
            format!("[service]\nport = {}\n", 8000 + unit),
        );
    }
    for unit in 0..12 {
        push(
            format!(".github/workflows/check_{unit:02}.yml"),
            format!("name: check {unit:02}\non: [push]\n"),
        );
    }
    for unit in 0..26 {
        push(
            format!("scripts/task_{unit:02}.sh"),
            format!("#!/bin/sh\necho task {unit:02}\n"),
        );
    }
    for unit in 0..20 {
        push(
            format!("data/sample_{unit:02}.json"),
            format!("{{\"sample\": {unit}}}\n"),
        );
    }
    // Five files this repository calls sensitive by its own built-in rules.
    // They are written, committed and never indexed; the session below asks
    // for them by name and is answered about nothing.
    for path in [
        "services/api/.env.production",
        "ops/deploy_secret.json",
        "keys/service.pem",
        "keys/deploy.key",
        "config/id_rsa",
    ] {
        push(path.to_owned(), "PLACEHOLDER=value\n".to_owned());
    }
}

/// Whether a generated path is one of the Rust sources.
fn is_rust(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

/// How many files the generated repository holds.
///
/// Stated as a constant rather than counted, so a change to the generator that
/// moved the corpus has to move this number too and say why in its commit.
const CORPUS_FILES: usize = 5_000;

/// The paths of the corpus that this repository's index will actually hold.
///
/// The walk drops what the repository calls sensitive before anything ranks,
/// so an expectation built from the whole corpus would be an expectation about
/// a listing nobody would have received.
fn indexable(files: &[GenFile]) -> Vec<String> {
    files
        .iter()
        .filter(|file| !file.sensitive)
        .map(|file| file.path.clone())
        .collect()
}

// ------------------------------------------------------------------ the pin ---

/// The committed artifact the hundred-file cap is read off.
const TOOL_FIELD_PIN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md"
);

/// ★ **The cap this fixture and the product both price against is the tool's
/// own, read back out of the pinned artifact.**
///
/// `NATIVE_LISTING_PATHS` is not a round number somebody liked: it is the
/// figure the `Glob` tool's own page states, recorded verbatim in this
/// repository's Claude Code tool-field fixture. A constant that drifted from
/// the sentence it was taken from would be a second claim about one tool, and
/// the whole pool below is measured against it — so the sentence is found by
/// its own words and the number in it is compared with the constant.
#[test]
fn the_hundred_file_cap_is_the_tools_own_pinned_figure() {
    let pinned = std::fs::read_to_string(TOOL_FIELD_PIN).unwrap_or_else(|e| {
        panic!("the committed tool-field pin is readable: {TOOL_FIELD_PIN} {e}")
    });
    let excerpt = pinned
        .lines()
        .find(|line| line.contains("capped at") && line.contains("files"))
        .unwrap_or_else(|| panic!("the pin still records what the tool caps a listing at"));
    assert!(
        excerpt.contains("Results are sorted by modification time and capped at 100 files"),
        "the sentence the cap is read off, verbatim: {excerpt}"
    );
    let stated: usize = excerpt
        .split_whitespace()
        .skip_while(|word| *word != "at")
        .nth(1)
        .and_then(|word| word.parse().ok())
        .unwrap_or_else(|| panic!("the sentence states a number: {excerpt}"));
    assert_eq!(
        stated,
        rift_recall_serve::NATIVE_LISTING_PATHS,
        "the constant every figure below is priced against is the tool's own"
    );
}

/// ★ **The other ceiling this fixture prices against is the client's own, read
/// back out of the same pinned artifact.**
///
/// A `Glob` stops after a hundred files. A shell command's result does not: it
/// arrives inline up to roughly thirty thousand characters, and past that the
/// model is handed a file path instead. So a `find` over a large subtree really
/// does put forty times a glob's worth of paths in front of a model, and the
/// pool below says so — but only where an artifact in this tree states the
/// number, which is the same discipline that governs what may be written on a
/// channel. Codex and Antigravity document no result ceiling here, so their
/// listings take Claude Code's ceilings by the owner's consistency ruling — an
/// assumption the code states as one, not those clients' documentation.
#[test]
fn the_shell_result_ceiling_is_the_clients_own_pinned_figure() {
    let pinned = std::fs::read_to_string(TOOL_FIELD_PIN).unwrap();
    assert!(
        pinned.contains("Inline up to roughly 30,000 characters"),
        "the sentence a shell result's ceiling is read off is still pinned"
    );
    assert!(
        pinned.contains(
            "a valid result over roughly 30,000 characters arrives as a file path plus preview"
        ),
        "and so is what happens past it"
    );
    assert_eq!(
        NATIVE_INLINE_CHARS, 30_000,
        "the figure this fixture prices a shell listing at is the one above"
    );
    assert_eq!(
        Ceiling::of("claude-code", "listing answer"),
        Ceiling::InlineChars(NATIVE_INLINE_CHARS),
        "the family whose page states it"
    );
    for family in ["codex", "antigravity"] {
        assert_eq!(
            Ceiling::of(family, "listing answer"),
            Ceiling::InlineChars(NATIVE_INLINE_CHARS),
            "{family} states no result ceiling in this tree and takes the one \
             above, by the owner's consistency ruling rather than by a pin of \
             its own"
        );
        assert_eq!(
            rift_recall_serve::SearchCeiling::of(family, rift_recall_serve::ListingShape::Listing),
            rift_recall_serve::SearchCeiling::InlineChars(NATIVE_INLINE_CHARS),
            "and the product reads the same table this fixture does"
        );
    }
    assert_eq!(
        Ceiling::of("claude-code", "glob answer"),
        Ceiling::Files(rift_recall_serve::NATIVE_LISTING_PATHS),
        "and a glob is bounded by its own tool whatever the client's result ceiling is"
    );
}

/// ★ **The declared listing baseline is the paths, one per line, cut at the
/// ceiling that shape's client documents — and nothing else.**
///
/// The formula restated where a reader can check it against the function,
/// because a pool is only as honest as the sentence that defines it. Four
/// claims: it is exactly the tokenizer's count of the listing; a glob stops
/// where the glob tool stops; a shell listing stops where the client stops
/// reading a command's result inline, which is a character budget and not a
/// file count; and a content search declares nothing at all.
#[test]
fn the_declared_listing_baseline_is_the_paths_one_per_line() {
    let paths: Vec<String> = (0..2_000)
        .map(|i| format!("crates/alder/src/core/token_{i:04}.rs"))
        .collect();
    let head = &paths[..rift_recall_serve::NATIVE_LISTING_PATHS];
    assert_eq!(
        glob_baseline(&paths),
        rift_tokens::count(&head.join("\n")),
        "the listing a glob would have printed, counted as it stands"
    );

    // The listing arm, spelled here the way `pool_of` spells it: sorted, one
    // per line, filled until the next path would not fit.
    let mut sorted = paths.clone();
    sorted.sort();
    let mut listing = String::new();
    for path in &sorted {
        if listing.len() + path.len() + 1 > NATIVE_INLINE_CHARS {
            break;
        }
        if !listing.is_empty() {
            listing.push('\n');
        }
        listing.push_str(path);
    }
    assert_eq!(
        rift_recall_serve::declared_search_baseline(
            false,
            rift_recall_serve::ListingShape::Listing,
            "claude-code",
            &paths,
        ),
        rift_tokens::count(&listing),
        "the listing a shell command would have printed, cut where the client \
         stops reading it"
    );
    assert!(
        listing.lines().count() > 5 * rift_recall_serve::NATIVE_LISTING_PATHS,
        "and that is many times a glob's worth of paths: {}",
        listing.lines().count()
    );

    assert_eq!(
        rift_recall_serve::declared_search_baseline(
            true,
            rift_recall_serve::ListingShape::Listing,
            "claude-code",
            &paths,
        ),
        0,
        "a content grep has no declared native cost in this tree, at any breadth"
    );
    assert_eq!(glob_baseline(&[]), 0);
}

/// **A listing costs the same whatever order the walk produced it in.**
///
/// This is what licenses every expectation below. The fixture knows which
/// paths a search matched, because it wrote them; it does not know which order
/// the engine ranked them in, and it must not have to. The tokenizer of record
/// splits on newlines before it merges anything, so a listing's price is the
/// sum of its lines and the order is not part of the question — asserted here
/// rather than assumed, because if it ever stopped being true every figure in
/// this file would quietly become approximate.
#[test]
fn a_listing_costs_the_same_however_the_walk_ordered_it() {
    let files = corpus();
    let mut paths = indexable(&files);
    paths.truncate(rift_recall_serve::NATIVE_LISTING_PATHS);
    let forward = glob_baseline(&paths);
    paths.reverse();
    assert_eq!(forward, glob_baseline(&paths));
    let mut noise = Noise::new(0xA11C_E000_5EED_0001);
    for _ in 0..3 {
        for i in (1..paths.len()).rev() {
            paths.swap(i, (noise.next() as usize) % (i + 1));
        }
        assert_eq!(
            forward,
            glob_baseline(&paths),
            "a listing's price is the sum of its lines"
        );
    }
}

/// **Any hundred of the over-cap family is the same listing to the token.**
///
/// The one search below that matches more paths than the tool would have
/// printed is answered from a family of names that are all the same length, so
/// which hundred of them the ranking chose cannot change what the listing cost
/// — which is what makes that search's figure an exact expectation rather than
/// a range.
#[test]
fn any_hundred_of_the_over_cap_family_costs_the_same() {
    let files = corpus();
    let family: Vec<String> = indexable(&files)
        .into_iter()
        .filter(|path| path.starts_with("services/api/gen/"))
        .collect();
    assert_eq!(family.len(), 137, "the family, over the tool's own cap");
    let one = rift_tokens::count(&family[0]);
    for path in &family {
        assert_eq!(
            rift_tokens::count(path),
            one,
            "{path} is priced differently from its siblings"
        );
    }
    let head = glob_baseline(&family[..rift_recall_serve::NATIVE_LISTING_PATHS]);
    let tail = glob_baseline(&family[37..37 + rift_recall_serve::NATIVE_LISTING_PATHS]);
    assert_eq!(head, tail, "any hundred of them is one price");
}

/// **The corpus is the same repository on every build.**
///
/// A fixture whose paths drifted between runs would publish a different pool
/// every time it ran, and no figure it produced could be compared with the one
/// before it. Everything here is written from one seed with no clock, no
/// environment and no directory order in the path, so the repository is a pure
/// function of this file.
#[test]
fn the_corpus_is_the_same_repository_on_every_build() {
    let first = corpus();
    let second = corpus();
    assert_eq!(first, second, "two builds of one corpus");
    assert_eq!(first.len(), CORPUS_FILES);
    let mut paths: Vec<&str> = first.iter().map(|f| f.path.as_str()).collect();
    paths.sort_unstable();
    let before = paths.len();
    paths.dedup();
    assert_eq!(paths.len(), before, "no path is generated twice");
    assert_eq!(
        first.iter().filter(|f| f.sensitive).count(),
        5,
        "the five files this repository's own rules keep out of any index"
    );
    assert!(
        first
            .iter()
            .any(|f| f.body.contains(REVIEW_MARKER) && is_rust(&f.path)),
        "and a literal only a content search can find"
    );
}

// ------------------------------------------------------------- the session ---

/// The subtree this repository's policy refuses to serve, so the screen has
/// something real to keep out of the frame and out of the pool.
const DENIED_GLOB: &str = "internal/**";

/// The session the levers are switched off for, and the one they are on for.
///
/// Two sessions over one repository and one index: the off arm establishes
/// what the native tool would have put in front of the model, the on arm what
/// Rift put there instead, and a second checkout would have measured a second
/// corpus.
const SESSION_OFF: &str = "search-off";
const SESSION_ON: &str = "search-on";

/// A hermetic repository holding the whole corpus, committed, with a live
/// session for each arm.
fn seed_repo(tmp: &Path, files: &[GenFile]) -> PathBuf {
    use std::process::{Command, Stdio};
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    // An identity with no address in it: this repository's own redaction
    // census counts every address-shaped literal in its tracked text against a
    // recorded ceiling, and a fixture is not a reason to spend one.
    git(&["config", "user.email", "searches"]);
    git(&["config", "user.name", "searches"]);
    for file in files {
        let path = repo.join(&file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &file.body).unwrap();
    }
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the corpus"]);
    // Recall declared OFF: it is on unless written so, and on it would hand
    // each session its pointer and book that spend beside the search
    // answers — a figure this fixture does not measure. The search levers
    // read their own `[search]` table and run either way.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"searches\"\n[features]\ntotal_recall = false\n",
    )
    .unwrap();
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    for session in [SESSION_OFF, SESSION_ON] {
        std::fs::write(
            sessions.join(format!("{session}.json")),
            format!(
                r#"{{"schema":1,"session_id":"{session}","state":"active","agent":"claude-code",
                 "current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,
                 "native_id_map":{{}},"updated_at_ms":1783000000000}}"#
            )
            .replace('\n', "")
            .replace("                 ", ""),
        )
        .unwrap();
    }
    repo
}

/// Switch the search levers on or off for the whole repository.
///
/// Written in place both times, deliberately: the index this fixture answers
/// from is keyed by the newest directory modification time in the checkout, so
/// creating a directory between the two arms would age the index out and the
/// second arm would measure a cold checkout instead of a lever.
fn write_policy(repo: &Path, mode: &str) {
    let dir = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("serve.toml"),
        format!("denied_paths = [\"{DENIED_GLOB}\"]\n\n[search]\nmode = \"{mode}\"\n"),
    )
    .unwrap();
}

/// Drive one real agent hook the way its family sends it, in a stated turn,
/// and keep its stdout.
fn hook(
    repo: &Path,
    agent: &str,
    verb: &str,
    session: &str,
    turn: u32,
    payload: &str,
    cache: &Path,
) -> serde_json::Value {
    use std::io::Write as _;
    use std::process::Stdio;
    let body = format!(r#"{{"schema":1,"session":"{session}","turn":{turn},"payload":{payload}}}"#);
    let mut child = common::rift_cmd(repo)
        .args(["hooks", agent, verb])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "a hook never fails the agent: {agent} {verb}"
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one object on stdout: {text} {e}"))
}

/// The session beginning, which is the moment the worker is asked for the walk
/// this member's index is built by.
fn session_start(repo: &Path, session: &str, cache: &Path) {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = common::rift_cmd(repo)
        .args(["hooks", "claude-code", "SessionStart"])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::json!({ "session_id": session, "source": "startup" })
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    assert!(child.wait().unwrap().success());
}

/// The decision one response carries, on whichever envelope its family's own
/// pinned row spells it.
///
/// Three families, three shapes, one question: Claude Code nests a decision and
/// its reason under `hookSpecificOutput`, and the other two state them at the
/// top level. Reading only the first would have read every answer on the other
/// two families as no answer at all.
fn declined(out: &serde_json::Value) -> Option<String> {
    let field = |nested: &str, flat: &str| {
        out.pointer(&format!("/hookSpecificOutput/{nested}"))
            .or_else(|| out.pointer(&format!("/{flat}")))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    // The two decline words the three families' pinned rows spell, and only
    // those: one of them answers a call with `allow` where the others say
    // nothing at all, and reading that as a decision would read every
    // untouched call as an answered one.
    let word = field("permissionDecision", "decision")?;
    if !matches!(word.as_str(), "deny" | "block") {
        return None;
    }
    let reason = field("permissionDecisionReason", "reason").unwrap_or_default();
    assert!(!reason.is_empty(), "a decline always says why: {out}");
    Some(reason)
}

/// One row of the family's own audit relation.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Audit {
    id: String,
    verb: String,
    reason: String,
    avoided: u32,
    returned: u32,
}

/// The family's one recall projection, resolved the way the product resolves
/// it — through the family manifest, never through a path.
fn audits(repo: &Path, cache: &Path) -> Vec<Audit> {
    use rift_store::CanonicalStore as _;
    let paths = rift_git::discover(repo).expect("the fixture is a repository");
    let cache_dir = cache.display().to_string();
    let lookup = |key: &str| (key == "RIFT_CACHE_DIR").then(|| cache_dir.clone());
    let rift_git::FamilyState::Minted(family) =
        rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
            .unwrap()
    else {
        panic!("the session above should have minted the family");
    };
    let store = rift_store::open_migrated(
        rift_store::recall_db_under(cache, &family.recall_id),
        &rift_store::recall_migrations(),
    )
    .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        store
            .query(
                "SELECT audit_id, verb, COALESCE(reason, ''), tokens_avoided, tokens_returned \
                 FROM memd_audit_event",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| Audit {
                id: row.get_text(0).unwrap_or_default(),
                verb: row.get_text(1).unwrap_or_default(),
                reason: row.get_text(2).unwrap_or_default(),
                avoided: u32::try_from(row.get_i64(3).unwrap_or(0)).unwrap_or(0),
                returned: u32::try_from(row.get_i64(4).unwrap_or(0)).unwrap_or(0),
            })
            .collect()
    })
}

/// The rows this call added, which is how a figure is attributed to the search
/// that earned it: an answer carries no file and no revision, so its row has no
/// handle to look it up by.
fn added(before: &[Audit], after: &[Audit]) -> Vec<Audit> {
    after
        .iter()
        .filter(|row| !before.iter().any(|seen| seen.id == row.id))
        .cloned()
        .collect()
}

// ------------------------------------------------------------- the searches ---

/// What the client the call was made on would have delivered, before Rift
/// answered it.
///
/// Two shapes, two ceilings, both pinned in this repository's own committed
/// artifacts, and they are nothing like each other: the `Glob` tool stops
/// after a hundred files, and a shell command's result arrives inline up to
/// roughly thirty thousand characters — so a `find` over a large subtree puts
/// forty times a glob's worth of paths in front of the model. The product
/// prices each shape at its own ceiling, and this fixture holds it to that
/// figure search by search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ceiling {
    /// The `Glob` tool's own documented file cap.
    Files(usize),
    /// A shell result's own documented inline character ceiling.
    InlineChars(usize),
}

impl Ceiling {
    /// The ceiling for one shape on one family.
    ///
    /// A glob is bounded by its own tool on every family. A listing is bounded
    /// by how much of a command's result the client reads back inline, and one
    /// client's page states that figure: Claude Code's. Codex and Antigravity
    /// state none anywhere in this tree and take the same figure by the
    /// owner's consistency ruling — one number across the alpha clients rather
    /// than three — which is an assumption about them and is written down as
    /// one both here and beside the constant the product reads.
    fn of(_family: &str, lever: &str) -> Self {
        match lever {
            "listing answer" => Self::InlineChars(NATIVE_INLINE_CHARS),
            _ => Self::Files(rift_recall_serve::NATIVE_LISTING_PATHS),
        }
    }
}

/// How many characters of a command's result a client reads back inline.
///
/// Pinned, like the file cap beside it, and read back out of the same
/// committed artifact by [`the_shell_result_ceiling_is_the_clients_own_pinned_figure`].
const NATIVE_INLINE_CHARS: usize = 30_000;

/// One scripted name search, and everything this fixture knows about it before
/// the session runs.
struct NameCase {
    /// What a reader of the published rows sees.
    label: &'static str,
    family: &'static str,
    /// Which of the two shapes the answer arm recognises this is.
    lever: &'static str,
    /// The call, spelled the way this family spells it.
    payload: String,
    /// The paths this member's index will match. The frame's total.
    matched: Vec<String>,
    /// Those of them an answer may name, which is what the pool may count.
    priced: Vec<String>,
    /// For a **glob**, whether the hundred the product prices is one this
    /// fixture knows — see [`claim_of`]. Ignored for a listing, whose price
    /// does not depend on a ranking at all.
    exact: bool,
    /// Whether Rift answers this shape at all.
    answerable: bool,
    /// Whether the bar's denominator counts it — see the ★ below.
    prescribed: bool,
}

/// The paths of the corpus one glob matches, as the index will match them.
fn matching_glob(files: &[GenFile], pattern: &str) -> Vec<String> {
    indexable(files)
        .into_iter()
        .filter(|path| rift_config::glob_match(pattern, path))
        .collect()
}

/// The paths of the corpus one `find <dir> -name <pattern>` listing matches:
/// the whole subtree, matched on the last segment.
fn matching_listing(files: &[GenFile], dir: &str, pattern: &str) -> Vec<String> {
    indexable(files)
        .into_iter()
        .filter(|path| {
            path.strip_prefix(dir)
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(|rest| {
                    rift_config::glob_match(pattern, rest.rsplit('/').next().unwrap_or(rest))
                })
        })
        .collect()
}

/// What is left of a match set once this repository's deny list has had it.
fn servable(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !rift_config::glob_match(DENIED_GLOB, path))
        .cloned()
        .collect()
}

/// **The pool.** What the model would have received had Rift answered nothing:
/// the paths of the listing, one per line, cut at the ceiling the client this
/// call was made on documents.
///
/// Repository-relative, where the native call prints absolute paths, and
/// without the truncation notice the tool would have added — both of which
/// make this smaller than what really arrives.
fn pool_of(case: &NameCase) -> u32 {
    match Ceiling::of(case.family, case.lever) {
        // Faithful where the fixture knows the order, conservative where it
        // cannot: `Glob` prints the hundred most recently modified files, and
        // a corpus committed in one go has nothing to tell them apart — so the
        // hundred priced here are the cheapest hundred, which is the least the
        // model could have received.
        Ceiling::Files(cap) => glob_baseline(&cheapest_head(&case.priced, cap)),
        // A shell listing has an order — the sorted one the product prints —
        // so this fills the client's character budget in that same order
        // rather than choosing one.
        Ceiling::InlineChars(cap) => {
            let mut sorted = case.priced.clone();
            sorted.sort();
            let mut listing = String::new();
            for path in sorted {
                if listing.len() + path.len() + 1 > cap {
                    break;
                }
                if !listing.is_empty() {
                    listing.push('\n');
                }
                listing.push_str(&path);
            }
            rift_tokens::count(&listing)
        }
    }
}

/// **The claim.** What the product will book for this search, and the most it
/// could book: the same paths, cut at the ceiling this call's own client
/// documents for this call's own shape — which is the same cut [`pool_of`]
/// makes, computed here from the corpus this fixture wrote rather than from
/// the decision the product left behind.
///
/// **A listing is one exact figure.** Its ceiling is a character budget over
/// the whole screened match set, so nothing about the ranking enters it: the
/// claim is the pool, and a product that priced it as a glob — or cut it in
/// some other order — books a different number and this fixture goes red.
///
/// **A glob is one figure wherever the hundred is knowable** — all of them
/// when fewer than a hundred matched, and any hundred of the equal-cost family
/// when more did. It differs only where the ranking chooses a hundred out of
/// thousands, and there the fixture states the cheapest and the dearest
/// hundred and requires the booked figure to lie between them.
fn claim_of(case: &NameCase) -> (u32, u32) {
    let cap = rift_recall_serve::NATIVE_LISTING_PATHS;
    if let Ceiling::InlineChars(_) = Ceiling::of(case.family, case.lever) {
        let exact = pool_of(case);
        return (exact, exact);
    }
    if case.priced.len() <= cap || case.exact {
        let exact = glob_baseline(&cheapest_head(&case.priced, cap));
        return (exact, exact);
    }
    let mut by_cost = case.priced.clone();
    by_cost.sort_by_cached_key(|path| (rift_tokens::count(path), path.clone()));
    (
        glob_baseline(&by_cost[..cap]),
        glob_baseline(&by_cost[by_cost.len() - cap..]),
    )
}

/// The product's own price for a glob's listing, spelled once so the six
/// places that ask for it cannot drift apart.
fn glob_baseline(paths: &[String]) -> u32 {
    rift_recall_serve::declared_search_baseline(
        false,
        rift_recall_serve::ListingShape::Glob,
        "claude-code",
        paths,
    )
}

/// The `n` cheapest paths of a listing, or all of it: the least the hundred a
/// ranking chose could have cost.
fn cheapest_head(paths: &[String], n: usize) -> Vec<String> {
    let mut by_cost = paths.to_vec();
    by_cost.sort_by_cached_key(|path| (rift_tokens::count(path), path.clone()));
    by_cost.truncate(n);
    by_cost
}

/// A Claude Code glob, with the one argument that family's pinned row names.
fn glob(pattern: &str) -> String {
    serde_json::json!({ "tool_name": "Glob", "tool_input": { "pattern": pattern } }).to_string()
}

/// A listing on a shell-shaped family, spelled as the published grammar spells
/// it, scoped by an absolute directory so no working directory is in question.
fn find(root: &Path, dir: &str, pattern: &str) -> String {
    let at = root.join(dir);
    serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": format!("find {} -name '{pattern}'", common::shell_spelling(&at)) },
        "cwd": root.display().to_string(),
    })
    .to_string()
}

/// The whole script: seven name searches the bar is taken over, across the
/// three families, the two shapes and every breadth from twenty-five matches
/// to four thousand — and two beside them.
fn name_cases(root: &Path, files: &[GenFile]) -> Vec<NameCase> {
    let case =
        |label, family, lever, payload, matched: Vec<String>, exact, answerable, prescribed| {
            let priced = servable(&matched);
            NameCase {
                label,
                family,
                lever,
                payload,
                matched,
                priced,
                exact,
                answerable,
                prescribed,
            }
        };
    vec![
        case(
            "a glob over more paths than the tool would have printed",
            "claude-code",
            "glob answer",
            glob("services/api/gen/case_*.rs"),
            matching_glob(files, "services/api/gen/case_*.rs"),
            true,
            true,
            true,
        ),
        case(
            "a glob over one chapter of the documentation",
            "claude-code",
            "glob answer",
            glob("docs/architecture/*.md"),
            matching_glob(files, "docs/architecture/*.md"),
            true,
            true,
            true,
        ),
        case(
            "a glob over every source in the repository",
            "claude-code",
            "glob answer",
            glob("**/*.rs"),
            matching_glob(files, "**/*.rs"),
            false,
            true,
            true,
        ),
        case(
            "a listing of every source under the library tree",
            "claude-code",
            "listing answer",
            find(root, "crates", "*.rs"),
            matching_listing(files, "crates", "*.rs"),
            false,
            true,
            true,
        ),
        case(
            "a listing of one screen of the front end",
            "claude-code",
            "listing answer",
            find(root, "web/app/settings", "*.tsx"),
            matching_listing(files, "web/app/settings", "*.tsx"),
            true,
            true,
            true,
        ),
        case(
            "a listing of one area of one package",
            "codex",
            "listing answer",
            find(root, "crates/cedar/src/engine", "*.rs"),
            matching_listing(files, "crates/cedar/src/engine", "*.rs"),
            true,
            true,
            true,
        ),
        case(
            "a listing of one binary's sources",
            "antigravity",
            "listing answer",
            serde_json::json!({
                "toolCall": {
                    "name": "find_by_name",
                    "args": {
                        "SearchDirectory": root.join("bins/rift/src").display().to_string(),
                        "Pattern": "*.rs",
                    },
                },
            })
            .to_string(),
            matching_listing(files, "bins/rift/src", "*.rs"),
            true,
            true,
            true,
        ),
        case(
            "a glob an answer could not have beaten",
            "claude-code",
            "glob answer",
            glob("scripts/task_2*.sh"),
            matching_glob(files, "scripts/task_2*.sh"),
            true,
            false,
            false,
        ),
        case(
            "a glob scoped by an argument no artifact pins",
            "claude-code",
            "glob answer",
            serde_json::json!({
                "tool_name": "Glob",
                "tool_input": { "pattern": "docs/guides/*.md", "path": "docs" },
            })
            .to_string(),
            matching_glob(files, "docs/guides/*.md"),
            true,
            false,
            false,
        ),
    ]
}

// --------------------------------------------------------- what it removed ---

/// What one search cost, both ways.
struct Priced {
    label: &'static str,
    family: &'static str,
    lever: &'static str,
    prescribed: bool,
    /// Whether Rift answered it at all.
    answered: bool,
    /// How many paths matched in the index — the frame's own total.
    total: usize,
    /// The declared native cost of the listing the model would have received,
    /// under the ceiling its own client documents.
    pool: u32,
    /// What the product's own accounting will claim for it, which prices each
    /// shape at the ceiling its own client documents — the same cut the pool
    /// is taken at, except for the one glob whose hundred a ranking chooses.
    claim: u32,
    /// What the agent received instead: the frame, or — where Rift did nothing
    /// — the listing it read itself, charged in full.
    received: u32,
    /// What the product booked as avoided for it.
    booked: u32,
}

/// The whole scripted session with the levers off: every search runs exactly as
/// the agent wrote it, and the model receives the listing this fixture prices.
fn play_without_the_levers(repo: &Path, cache: &Path, cases: &[NameCase]) {
    for (n, case) in cases.iter().enumerate() {
        let out = hook(
            repo,
            case.family,
            "PreToolUse",
            SESSION_OFF,
            u32::try_from(n).unwrap_or(0) + 1,
            &case.payload,
            cache,
        );
        assert_eq!(
            declined(&out),
            None,
            "{}: a repository with the lever off decides nothing: {out}",
            case.label
        );
        assert!(
            out.pointer("/hookSpecificOutput/updatedInput").is_none(),
            "{}: and amends nothing either: {out}",
            case.label
        );
    }
}

/// The same session with the levers on, measured search by search.
fn play_with_the_levers(
    repo: &Path,
    cache: &Path,
    files: &[GenFile],
    cases: &[NameCase],
) -> Vec<Priced> {
    let mut priced = Vec::new();
    for (n, case) in cases.iter().enumerate() {
        let pool = pool_of(case);
        let (claim, dearest) = claim_of(case);
        let before = audits(repo, cache);
        let out = hook(
            repo,
            case.family,
            "PreToolUse",
            SESSION_ON,
            u32::try_from(n).unwrap_or(0) + 1,
            &case.payload,
            cache,
        );
        let frame = declined(&out);
        let rows = added(&before, &audits(repo, cache));
        let Some(frame) = frame else {
            assert!(
                !case.answerable,
                "{}: this shape is answered, and was not: {out}",
                case.label
            );
            assert!(
                rows.is_empty(),
                "{}: nothing was said, so nothing is claimed: {rows:?}",
                case.label
            );
            priced.push(Priced {
                label: case.label,
                family: case.family,
                lever: case.lever,
                prescribed: case.prescribed,
                answered: false,
                total: case.matched.len(),
                pool,
                claim,
                // Rift did nothing, so the agent ran the search and paid for
                // the listing in full. Charged against the mechanism.
                received: pool,
                booked: 0,
            });
            continue;
        };
        assert!(
            case.answerable,
            "{}: this shape is not answered",
            case.label
        );
        assert!(
            out.pointer("/hookSpecificOutput/updatedInput").is_none(),
            "{}: a decision carries no amendment: {out}",
            case.label
        );
        check_the_frame(case, files, &frame);
        check_the_row(case, &rows, &frame, claim, dearest);
        priced.push(Priced {
            label: case.label,
            family: case.family,
            lever: case.lever,
            prescribed: case.prescribed,
            answered: true,
            total: case.matched.len(),
            pool,
            claim,
            received: rift_tokens::count(&frame),
            booked: rows[0].avoided,
        });
    }
    priced
}

/// What an answer said, and what it may never say.
fn check_the_frame(case: &NameCase, files: &[GenFile], frame: &str) {
    assert!(
        frame.starts_with("[rift] Search answered: ")
            && frame.contains(&format!(" of {} paths matching", case.matched.len())),
        "{}: the ranked head and the total it is a head of: {frame}",
        case.label
    );
    assert!(
        !frame.contains("internal/"),
        "{}: a denied path is not a name an answer says: {frame}",
        case.label
    );
    assert!(
        !frame.contains(REVIEW_MARKER) && !frame.contains("pub fn"),
        "{}: names, never contents: {frame}",
        case.label
    );
    for file in files.iter().filter(|file| file.sensitive) {
        assert!(
            !frame.contains(&file.path),
            "{}: {} is not this index's to name",
            case.label,
            file.path
        );
    }
}

/// The row that answer owed, and the two figures on it.
///
/// This is the fixture↔product cross-check: the product priced the paths its
/// own decision left behind, this fixture priced the paths it wrote, and the
/// two are one quantity wherever the fixture knows which hundred the tool would
/// have printed. Seven pounds either way on either side and this goes red.
fn check_the_row(case: &NameCase, rows: &[Audit], frame: &str, claim: u32, dearest: u32) {
    assert_eq!(
        rows.len(),
        1,
        "{}: one answer, one row: {rows:?}",
        case.label
    );
    let row = &rows[0];
    assert_eq!(row.verb, "memd.search_answered", "{}", case.label);
    // The row an answer books says which kind of figure is on it: an answer
    // that was made declares its baseline and says so, where one that was only
    // recorded carries its own tag and no claim. Every row here is the first
    // kind, because this arm runs with the lever enforcing.
    assert_eq!(
        row.reason, "declared_baseline",
        "{}: an answered search says its figure is declared, not measured: {row:?}",
        case.label
    );
    assert_eq!(
        row.returned,
        rift_tokens::count(frame),
        "{}: the frame the model read is what the row books as returned",
        case.label
    );
    if case.exact {
        assert_eq!(
            row.avoided, claim,
            "{}: the product's declared cost of this listing and this fixture's \
             are one quantity",
            case.label
        );
    } else {
        assert!(
            row.avoided >= claim && row.avoided <= dearest,
            "{}: the hundred the ranking chose is priced between the cheapest \
             hundred ({claim}) and the dearest ({dearest}): {}",
            case.label,
            row.avoided
        );
    }
}

/// A share as a percentage, with a share of nothing reported as nothing.
fn share(part: u32, whole: u32) -> f64 {
    if whole == 0 {
        0.0
    } else {
        f64::from(part) * 100.0 / f64::from(whole)
    }
}

/// The figures over one set of searches.
fn publish(label: &str, rows: &[&Priced]) -> (u32, u32) {
    let pool: u32 = rows.iter().map(|r| r.pool).sum();
    let received: u32 = rows.iter().map(|r| r.received).sum();
    let removed = pool.saturating_sub(received);
    let claim: u32 = rows.iter().map(|r| r.claim).sum();
    println!(
        "{label}: {} searches · pool {pool} · received {received} · removed {removed} \
         ({:.2} %) · the product claimed {} of it, and would claim {claim} if every \
         hundred it prices were the ranking's own ({:.2} % on that floor)",
        rows.len(),
        share(removed, pool),
        rows.iter().map(|r| u64::from(r.booked)).sum::<u64>(),
        share(claim.saturating_sub(received), claim),
    );
    (pool, received)
}

// ------------------------------------------- the searches that are not answered ---

/// How many markers of one kind this member's windows hold.
///
/// The re-query rate has exactly one definition and it is these files:
/// `c-<key>` is a search a window bounded, `q-<key>` is one asked again in the
/// turn it was bounded in, and the rate is the second count over the first.
/// Counted here the way the gate writes them, from outside the binary.
fn markers(repo: &Path, prefix: &str) -> usize {
    let dir = repo.join(".rift/state/search/w");
    let Ok(windows) = std::fs::read_dir(&dir) else {
        return 0;
    };
    windows
        .flatten()
        .filter_map(|window| std::fs::read_dir(window.path()).ok())
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .count()
}

/// The content searches, which are bounded and never answered.
///
/// Three of them, and the second is asked twice in one turn: a bounded search
/// followed by the same search again is the escape hatch the tool's own total
/// invites, and how often it is taken is the figure this lever is judged by.
fn play_the_content_searches(repo: &Path, cache: &Path) -> (usize, usize) {
    let grep = |input: serde_json::Value| {
        serde_json::json!({ "tool_name": "Grep", "tool_input": input }).to_string()
    };
    let unbounded = grep(serde_json::json!({ "pattern": REVIEW_MARKER }));
    let counted = grep(serde_json::json!({ "pattern": "TODO", "output_mode": "count" }));
    let bounded = grep(serde_json::json!({ "pattern": "fn scan", "head_limit": 5 }));

    // Bounded, then asked again in the same turn: the round trip the cap costs.
    let first = hook(
        repo,
        "claude-code",
        "PreToolUse",
        SESSION_ON,
        101,
        &unbounded,
        cache,
    );
    assert_eq!(
        first.pointer("/hookSpecificOutput/updatedInput/head_limit"),
        Some(&serde_json::Value::from(40)),
        "a content search that named no bound is given the one its tool documents: {first}"
    );
    assert_eq!(
        declined(&first),
        None,
        "a content search is never substituted: {first}"
    );
    let again = hook(
        repo,
        "claude-code",
        "PreToolUse",
        SESSION_ON,
        101,
        &unbounded,
        cache,
    );
    assert!(
        again.pointer("/hookSpecificOutput/updatedInput").is_none(),
        "the same search again runs with no bound at all: {again}"
    );
    // The result the tool really ran with, so the bound is verified rather
    // than assumed — which is the only thing that books the row.
    let executed = |input: serde_json::Value, response: serde_json::Value| {
        serde_json::json!({
            "tool_name": "Grep",
            "tool_input": input,
            "tool_response": response,
        })
        .to_string()
    };
    hook(
        repo,
        "claude-code",
        "PostToolUse",
        SESSION_ON,
        101,
        &executed(
            serde_json::json!({ "pattern": REVIEW_MARKER, "head_limit": 40 }),
            serde_json::json!({ "mode": "files_with_matches", "numFiles": 40 }),
        ),
        cache,
    );
    // A second search bounded, and never asked again.
    hook(
        repo,
        "claude-code",
        "PreToolUse",
        SESSION_ON,
        102,
        &counted,
        cache,
    );
    hook(
        repo,
        "claude-code",
        "PostToolUse",
        SESSION_ON,
        102,
        &executed(
            serde_json::json!({ "pattern": "TODO", "output_mode": "count", "head_limit": 40 }),
            serde_json::json!({ "mode": "count", "numFiles": 0 }),
        ),
        cache,
    );
    // A search that named its own bound is left exactly as it was written.
    let explicit = hook(
        repo,
        "claude-code",
        "PreToolUse",
        SESSION_ON,
        103,
        &bounded,
        cache,
    );
    assert!(
        explicit
            .pointer("/hookSpecificOutput/updatedInput")
            .is_none(),
        "a bound the agent chose is not one to choose again: {explicit}"
    );
    (markers(repo, "c-"), markers(repo, "q-"))
}

/// The listing shapes Rift will not answer, each left exactly as written.
///
/// A directory's contents are what a file index cannot enumerate: it would
/// answer `ls` with no subdirectory names and a total that counted none of
/// them. Nothing in this tree pins what such a call costs a model either, so
/// these are published as a count and divided by nothing.
fn play_the_shapes_rift_declines(repo: &Path, root: &Path, cache: &Path) -> usize {
    let shell = |command: String| {
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "cwd": root.display().to_string(),
        })
        .to_string()
    };
    let calls = [
        (
            "claude-code",
            shell(format!("ls -R {}", root.join("docs").display())),
        ),
        (
            "claude-code",
            shell(format!(
                "rg --files {}",
                common::shell_spelling(&root.join("web"))
            )),
        ),
        (
            "antigravity",
            serde_json::json!({
                "toolCall": {
                    "name": "list_dir",
                    "args": { "DirectoryPath": root.join("bins").display().to_string() },
                },
            })
            .to_string(),
        ),
    ];
    for (n, (family, payload)) in calls.iter().enumerate() {
        let out = hook(
            repo,
            family,
            "PreToolUse",
            SESSION_ON,
            110 + u32::try_from(n).unwrap_or(0),
            payload,
            cache,
        );
        assert_eq!(
            declined(&out),
            None,
            "a directory listing is not this index's to answer: {out}"
        );
    }
    calls.len()
}

/// The savings figure as a person is shown it.
fn savings_json(repo: &Path, cache: &Path) -> serde_json::Value {
    let out = common::rift_cmd(repo)
        .args(["savings", "--json"])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one object: {text} {e}"))
}

/// Everything one run publishes: per search, per lever, per family, the corpus
/// and the wider set — printed before anything is asserted, so a shortfall
/// names its lever rather than a number.
fn publish_every_row(priced: &[Priced]) -> (u32, u32) {
    println!("\nrecall-search-savings: per search");
    for row in priced {
        println!(
            "  {:<56} {:<11} {:<15} total {:>5} · pool {:>6} · received {:>5} · \
             removed {:>6} ({:>6.2} %) {} {}",
            row.label,
            row.family,
            row.lever,
            row.total,
            row.pool,
            row.received,
            row.pool.saturating_sub(row.received),
            share(row.pool.saturating_sub(row.received), row.pool),
            if row.answered {
                "answered "
            } else {
                "untouched"
            },
            if row.prescribed { "in    " } else { "beside" },
        );
        if row.claim != row.pool {
            println!(
                "      (the hundred this glob's ranking chose is not the hundred \
                 the pool prices, so the claim is stated as a range; booked {})",
                row.booked
            );
        }
    }
    println!("recall-search-savings: per lever");
    for lever in ["glob answer", "listing answer"] {
        let rows: Vec<&Priced> = priced
            .iter()
            .filter(|r| r.prescribed && r.lever == lever)
            .collect();
        publish(&format!("  {lever:<15}"), &rows);
    }
    println!("recall-search-savings: per family");
    for family in ["claude-code", "codex", "antigravity"] {
        let rows: Vec<&Priced> = priced
            .iter()
            .filter(|r| r.prescribed && r.family == family)
            .collect();
        publish(&format!("  {family:<15}"), &rows);
    }
    let corpus_rows: Vec<&Priced> = priced.iter().filter(|r| r.prescribed).collect();
    let every_row: Vec<&Priced> = priced.iter().collect();
    let (pool, received) = publish("recall-search-savings (the corpus)", &corpus_rows);
    publish("recall-search-savings (and beside it)", &every_row);

    (pool, received)
}

/// **The figure a person is shown, end to end on the fixture family.**
///
/// One relation states the delta, and it is the declared listings and nothing
/// else: no bound books a figure, no untouched search books one, and the second
/// relation the per-family rows come from states the same total rather than a
/// second copy of it.
fn check_the_figure_a_person_is_shown(repo: &Path, cache: &Path, priced: &[Priced]) {
    let claimed: u64 = priced.iter().map(|r| u64::from(r.booked)).sum();
    let returned: u64 = priced
        .iter()
        .filter(|r| r.answered)
        .map(|r| u64::from(r.received))
        .sum();
    let savings = savings_json(repo, cache);
    assert_eq!(
        savings["overall"]["tokens_avoided"].as_u64(),
        Some(claimed),
        "the delta a person is shown is the declared listings and nothing else: {savings}"
    );
    assert_eq!(
        savings["overall"]["tokens_returned"].as_u64(),
        Some(returned),
        "and what those answers cost: {savings}"
    );
    assert_eq!(
        savings["overall"]["relation"],
        serde_json::json!("memd_audit_event")
    );
    let by_family: u64 = savings["by_family"]
        .as_array()
        .expect("families")
        .iter()
        .map(|row| row["tokens_avoided"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(
        by_family, claimed,
        "the second relation states the same figure rather than a second copy \
         of it: {savings}"
    );
}

// ---------------------------------------------------------------- the bar ---

/// ★ The corpus's own numbers, and the nine-tenths bar.
///
/// **What the pool is.** The declared native cost of the listing each of these
/// searches would have put in front of the model: the paths themselves, one per
/// line, minus the ones this repository refuses to name, cut at the ceiling the
/// client the call was made on documents — a hundred files for a `Glob`, thirty
/// thousand characters for a shell result, and Claude Code's ceilings for the
/// two clients that document none, by the owner's consistency ruling. Declared,
/// never measured: the listing never ran.
///
/// **What the product claims, beside it.** The same pool, priced the same way:
/// `declared_search_baseline` prices each shape at the ceiling its own client
/// documents, so a shell listing of four thousand paths is booked as the paths
/// that would have fitted the client's inline result and not as a hundred of
/// them. The one place the two figures may differ is the glob whose hundred
/// the ranking chooses out of thousands, and there the fixture states the
/// cheapest and the dearest hundred and requires the booked figure to lie
/// between them.
///
/// **What removed is.** The pool minus what the agent received instead: the
/// frame, and — for every search Rift left alone — the listing it read itself,
/// charged in full.
///
/// **What the bar sits on.** The seven name searches the script names before
/// anything is measured, across the three families and the two shapes the
/// levers act on, from twenty-five matches to four thousand. Two more are run
/// in the same session and published beside them — a listing an answer could
/// not have beaten, and a glob scoped by an argument no artifact pins — so the
/// wider figure a reader wants is on the same page as the bar.
///
/// **What is outside every fraction, and why.** Content searches. A grep's
/// native output is every matching line of every matching file, nothing in
/// this tree pins what that costs, and a name search is never substituted for
/// one — so a grep has no pool, is never in a numerator or a denominator, and
/// publishes its honesty figure instead: the rate at which a bounded search
/// was asked again in the turn it was bounded in. Recursive directory
/// listings are outside it for the same reason from the other side: Rift
/// declines to answer them at all, and nothing here pins what one costs.
#[test]
fn the_search_fixture_removes_at_least_ninety_percent_of_its_pool() {
    let files = corpus();
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path(), &files);
    let root = repo.canonicalize().unwrap();
    let cache = tmp.path().join("cache");
    let cases = name_cases(&root, &files);

    // One index, built once, and both arms measured against it. It is built
    // under the shipped policy because an index is not a decision about any
    // agent — a repository with the levers off keeps no index at all, and an
    // arm that measured a cold checkout would be measuring the walk rather
    // than the lever.
    write_policy(&repo, "enforce");
    session_start(&repo, SESSION_OFF, &cache);
    session_start(&repo, SESSION_ON, &cache);
    assert!(
        repo.join(".rift/state/search/index.bin").is_file(),
        "the worker built the index both arms are measured against"
    );

    // The arm without the levers: the tool runs, and the model receives the
    // listing this fixture prices. The file is rewritten in place, so the
    // index this arm and the next share does not age out between them.
    write_policy(&repo, "off");
    play_without_the_levers(&repo, &cache, &cases);
    assert!(
        audits(&repo, &cache)
            .iter()
            .all(|row| !row.verb.starts_with("memd.search")),
        "a repository with the lever off books nothing for a search"
    );

    // The arm with them on, over the same corpus and the same index.
    write_policy(&repo, "enforce");
    let priced = play_with_the_levers(&repo, &cache, &files, &cases);

    // A name search for the files this repository calls sensitive is answered
    // about nothing: they never entered the index, so the answer is not made
    // and no name is said.
    for pattern in ["**/*.pem", "**/*.key", "**/.env*"] {
        let out = hook(
            &repo,
            "claude-code",
            "PreToolUse",
            SESSION_ON,
            120,
            &glob(pattern),
            &cache,
        );
        assert_eq!(
            declined(&out),
            None,
            "{pattern}: nothing here is a statement about the index: {out}"
        );
        assert!(!out.to_string().contains("keys/"), "{pattern}: {out}");
    }

    let (capped, requeried) = play_the_content_searches(&repo, &cache);
    let declined = play_the_shapes_rift_declines(&repo, &root, &cache);

    let (pool, received) = publish_every_row(&priced);

    // ---- the honesty figures, which are not savings ----
    println!(
        "recall-search-savings: the bounded content searches — {capped} bounded, \
         {requeried} asked again in the turn they were bounded in, re-query rate \
         {:.2} % · avoided 0 by construction (no declared native cost exists for a \
         content grep in this tree) · {declined} directory-listing shapes left alone \
         and divided by nothing",
        share(
            u32::try_from(requeried).unwrap_or(0),
            u32::try_from(capped).unwrap_or(0)
        ),
    );
    assert_eq!(capped, 2, "two content searches were bounded");
    assert_eq!(requeried, 1, "one of them was asked again in its own turn");
    let books = audits(&repo, &cache);
    let bounded: Vec<&Audit> = books
        .iter()
        .filter(|row| row.verb == "memd.grep_capped")
        .collect();
    assert_eq!(bounded.len(), capped, "one row per bound: {books:?}");
    assert!(
        bounded
            .iter()
            .all(|row| row.avoided == 0 && row.returned == 0),
        "a bound claims nothing and costs nothing: {bounded:?}"
    );

    check_the_figure_a_person_is_shown(&repo, &cache, &priced);

    // ---- the bar ----
    let removed = pool.saturating_sub(received);
    println!(
        "recall-search-savings: the gate is >= 90 % over the seven name searches the \
         script names — pool {pool} · received {received} · removed {removed} \
         ({:.2} %)",
        share(removed, pool)
    );
    assert!(
        share(removed, pool) >= 90.0,
        "the search fixture removes {:.2} % of its pool; per-lever and per-family \
         figures are printed above, so the shortfall names its lever",
        share(removed, pool)
    );
    assert!(
        !repo
            .join(".rift/state/search/answers")
            .read_dir()
            .unwrap()
            .any(|entry| entry.is_ok()),
        "and every answer's paths were taken away with the pricing"
    );
}

// ------------------------------------------------ the search denominator ---

/// The measured tool results this family's capture projection holds, as
/// `(tool_name, channel, basis, tokens)`.
fn result_rows(repo: &Path, cache: &Path) -> Vec<(String, String, String, i64)> {
    use rift_store::CanonicalStore as _;
    let paths = rift_git::discover(repo).expect("the fixture is a repository");
    let cache_dir = cache.display().to_string();
    let lookup = |key: &str| (key == "RIFT_CACHE_DIR").then(|| cache_dir.clone());
    let rift_git::FamilyState::Minted(family) =
        rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
            .unwrap()
    else {
        panic!("the session above should have minted the family");
    };
    let db = rift_store::projection_db_for_family_under(cache, &family);
    if !db.exists() {
        return Vec::new();
    }
    let store = rift_store::SqliteStore::open(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        store
            .query(
                "SELECT tool_name, channel, basis, output_tokens FROM tool_result_tokens \
                 ORDER BY tool_name, basis",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r.get_text(0).unwrap_or_default(),
                    r.get_text(1).unwrap_or_default(),
                    r.get_text(2).unwrap_or_default(),
                    r.get_i64(3).unwrap_or(0),
                )
            })
            .collect()
    })
}

/// ★ **A search enters the denominator, and one nobody could measure is
/// counted rather than dropped.**
///
/// Before this relation, `read_tokens` joined `tool_reads_file`, so only a
/// call that resolved to a file could ever be in the pool the share is taken
/// over — and a search, which returns repository content on every call and
/// resolves to no file, was invisible. Five real calls through the real
/// binary, three rows, and the two bases that do not depend on what is
/// installed on the machine running this:
///
/// * a glob, matched against the index Rift already keeps, and a content
///   search whose pattern is a literal, matched by the same matcher the pull
///   verb uses — `in_process`, nothing executed at all;
/// * a search the allow-list covers whose composition no argv can run —
///   counted with **no tokens** under `unmeasured`, because a call nobody
///   measured is a fact and a figure nobody measured is not.
///
/// And the two calls that are *not* this relation's: a shell **read**, which
/// is already in the denominator through its own read fact and would be
/// counted twice, and a command that is not a search at all.
///
/// The third basis, `reexecuted`, is what the argv arm records; it is not
/// asserted here because whether `grep` and `rg` exist is a property of the
/// runner and never of the figure. Its own rules are pinned in
/// `search_measure`'s unit tests, which need no `grep` to state them.
#[test]
fn a_search_result_is_measured_and_an_unrunnable_one_is_counted() {
    let files = corpus();
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path(), &files);
    let root = repo.canonicalize().unwrap();
    let cache = tmp.path().join("cache");
    write_policy(&repo, "enforce");
    session_start(&repo, SESSION_ON, &cache);
    assert!(
        repo.join(".rift/state/search/index.bin").is_file(),
        "the worker built the index the in-process arms answer from"
    );

    let post = |payload: String| {
        hook(
            &repo,
            "claude-code",
            "PostToolUse",
            SESSION_ON,
            1,
            &payload,
            &cache,
        );
    };
    // One decision first, so the family has a serving projection at all: the
    // savings verb reports nothing for a family that has never served, which
    // is a different sentence from a zero and is not this test's subject.
    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        SESSION_ON,
        1,
        &glob("docs/architecture/*.md"),
        &cache,
    );
    post(glob("docs/architecture/*.md"));
    post(
        serde_json::json!({
            "tool_name": "Grep",
            "tool_input": { "pattern": REVIEW_MARKER },
        })
        .to_string(),
    );
    post(
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": "grep -rn needs_review docs | head -5" },
            "cwd": root.display().to_string(),
        })
        .to_string(),
    );
    // Neither of these is this relation's: the first is a read and is already
    // in the denominator, the second is not a search Rift can see at all.
    post(
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {
                "command": format!("cat {}", common::shell_spelling(&root.join("Cargo.toml"))),
            },
            "cwd": root.display().to_string(),
        })
        .to_string(),
    );
    post(
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": "cargo build --release" },
            "cwd": root.display().to_string(),
        })
        .to_string(),
    );

    let rows = result_rows(&repo, &cache);
    println!("search-denominator rows: {rows:?}");
    assert_eq!(rows.len(), 3, "one row per search and no more: {rows:?}");

    let row = |tool: &str| {
        rows.iter()
            .find(|(name, ..)| name == tool)
            .unwrap_or_else(|| panic!("{tool} was measured: {rows:?}"))
            .clone()
    };
    let glob_row = row("Glob");
    assert_eq!(glob_row.1, "search");
    assert_eq!(glob_row.2, "in_process", "a glob never runs a program");
    assert!(
        glob_row.3 > 0,
        "and the paths it would have listed are tokens: {glob_row:?}"
    );

    let grep_row = row("Grep");
    assert_eq!(
        grep_row.2, "in_process",
        "a literal pattern is matched by Rift's own matcher: {rows:?}"
    );
    assert!(grep_row.3 > 0, "and its lines are tokens: {grep_row:?}");

    let piped = row("Bash");
    assert_eq!(
        piped.2, "unmeasured",
        "a composition no argv can run is counted, not measured: {rows:?}"
    );
    assert_eq!(piped.3, 0, "an unmeasured call contributes no tokens");

    // The figure a person is shown carries the channel breakdown and the
    // count of what nobody measured — and the search channel is the sum of
    // exactly the rows above.
    let savings = savings_json(&repo, &cache);
    assert_eq!(
        savings["overall"]["channels"]["search"].as_u64(),
        Some(u64::try_from(glob_row.3 + grep_row.3).unwrap()),
        "the search channel is the sum of the measured searches: {savings}"
    );
    assert_eq!(
        savings["overall"]["reads_unmeasured"].as_u64(),
        Some(1),
        "and the call nobody could measure is published rather than dropped: {savings}"
    );
    // The denominator grew by exactly the searches: the same rows, on the
    // read side of the share.
    let read_tokens = savings["overall"]["read_tokens"].as_u64().unwrap_or(0);
    let by_channel = savings["overall"]["channels"]["read_tool"]
        .as_u64()
        .unwrap_or(0)
        + savings["overall"]["channels"]["shell_read"]
            .as_u64()
            .unwrap_or(0)
        + savings["overall"]["channels"]["search"]
            .as_u64()
            .unwrap_or(0);
    assert_eq!(
        read_tokens, by_channel,
        "the breakdown sums to the denominator it breaks down: {savings}"
    );
}
