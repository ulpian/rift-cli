//! The generated repository every search suite measures over, and the driver
//! that puts a real hook call against it: the corpus, the hermetic checkout,
//! the policy file that switches the levers, the one-process-per-hook spawn,
//! and the readers that look at what a call left behind.
//!
//! **Everything here is generated, and generated in Rust.** The repository is
//! a pure function of this file: five thousand paths with the depth and the
//! extensions a real checkout has, the same on every machine and every
//! platform, with nothing committed for a reviewer to take on trust. A search
//! fixture has to be big or it measures nothing — an answer that names eight
//! paths out of ten is not a saving, and the gate this fixture feeds stands an
//! answer down whenever the frame would cost more than the listing it stands
//! in for.
//!
//! Each search suite includes this file by `#[path]` as its own module and
//! uses a subset of the builders, which is why each declaration carries
//! `#[allow(dead_code)]`: the allow names a fact about the include, not a
//! suppression.

use std::path::{Path, PathBuf};

use super::common::rift_cmd;

// ----------------------------------------------------------- the generator ---

/// A tiny deterministic source of variety, so two files are not the same file
/// and no run is a different corpus from the last.
///
/// A linear congruential generator with the constants Numerical Recipes
/// publishes: nothing here is cryptography, and the only property that matters
/// is that it is a pure function of its seed on every platform.
pub(crate) struct Noise(u64);

impl Noise {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
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
pub(crate) const REVIEW_MARKER: &str = "needs_review";

/// One generated file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GenFile {
    /// Repository-relative, `/`-spelled.
    pub(crate) path: String,
    /// A small plausible body. Never large: this fixture prices path listings,
    /// and a corpus whose bytes dominated its own generation would cost the
    /// suite minutes to say nothing.
    pub(crate) body: String,
    /// Whether this repository's own sensitive-path rules keep it out of the
    /// index altogether — a fact asserted below rather than assumed.
    pub(crate) sensitive: bool,
}

/// The generated repository: five thousand paths with the shape of a checkout
/// somebody works in.
pub(crate) fn corpus() -> Vec<GenFile> {
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
pub(crate) fn is_rust(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

/// How many files the generated repository holds.
///
/// Stated as a constant rather than counted, so a change to the generator that
/// moved the corpus has to move this number too and say why in its commit.
pub(crate) const CORPUS_FILES: usize = 5_000;

/// The paths of the corpus that this repository's index will actually hold.
///
/// The walk drops what the repository calls sensitive before anything ranks,
/// so an expectation built from the whole corpus would be an expectation about
/// a listing nobody would have received.
pub(crate) fn indexable(files: &[GenFile]) -> Vec<String> {
    files
        .iter()
        .filter(|file| !file.sensitive)
        .map(|file| file.path.clone())
        .collect()
}

// ------------------------------------------------------------- the session ---

/// The subtree this repository's policy refuses to serve, so the screen has
/// something real to keep out of the frame and out of the pool.
pub(crate) const DENIED_GLOB: &str = "internal/**";

/// The session the levers are switched off for, and the one they are on for.
///
/// Two sessions over one repository and one index: the off arm establishes
/// what the native tool would have put in front of the model, the on arm what
/// Rift put there instead, and a second checkout would have measured a second
/// corpus.
pub(crate) const SESSION_OFF: &str = "search-off";
pub(crate) const SESSION_ON: &str = "search-on";

/// A hermetic repository holding the whole corpus, committed, with a live
/// session for each arm.
pub(crate) fn seed_repo(tmp: &Path, files: &[GenFile]) -> PathBuf {
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
    let sessions = super::common::seed_sessions_dir(&repo.join(".git"));
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
pub(crate) fn write_policy(repo: &Path, mode: &str) {
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
pub(crate) fn hook(
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
    let mut child = rift_cmd(repo)
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
pub(crate) fn session_start(repo: &Path, session: &str, cache: &Path) {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = rift_cmd(repo)
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

/// A Claude Code glob, with the one argument that family's pinned row names.
pub(crate) fn glob(pattern: &str) -> String {
    serde_json::json!({ "tool_name": "Glob", "tool_input": { "pattern": pattern } }).to_string()
}

/// The decision one response carries, on whichever envelope its family's own
/// pinned row spells it.
///
/// Three families, three shapes, one question: Claude Code nests a decision and
/// its reason under `hookSpecificOutput`, and the other two state them at the
/// top level. Reading only the first would have read every answer on the other
/// two families as no answer at all.
pub(crate) fn declined(out: &serde_json::Value) -> Option<String> {
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
pub(crate) struct Audit {
    pub(crate) id: String,
    pub(crate) verb: String,
    pub(crate) reason: String,
    pub(crate) avoided: u32,
    pub(crate) returned: u32,
}

/// The family this checkout belongs to, resolved the way the product resolves
/// it — through the family manifest, never through a path.
pub(crate) fn family_of(repo: &Path, cache: &Path) -> rift_git::family::Family {
    let paths = rift_git::discover(repo).expect("the fixture is a repository");
    let cache_dir = cache.display().to_string();
    let lookup = |key: &str| (key == "RIFT_CACHE_DIR").then(|| cache_dir.clone());
    let rift_git::FamilyState::Minted(family) =
        rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
            .unwrap()
    else {
        panic!("the session above should have minted the family");
    };
    family
}

/// Every row of the family's own audit relation, read out of the one recall
/// projection the family keeps.
pub(crate) fn audits(repo: &Path, cache: &Path) -> Vec<Audit> {
    use rift_store::CanonicalStore as _;
    let family = family_of(repo, cache);
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
pub(crate) fn added(before: &[Audit], after: &[Audit]) -> Vec<Audit> {
    after
        .iter()
        .filter(|row| !before.iter().any(|seen| seen.id == row.id))
        .cloned()
        .collect()
}

/// The savings figure as a person is shown it.
pub(crate) fn savings_json(repo: &Path, cache: &Path) -> serde_json::Value {
    let out = rift_cmd(repo)
        .args(["savings", "--json"])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one object: {text} {e}"))
}
