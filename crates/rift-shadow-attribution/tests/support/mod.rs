//! Deterministic git **rewrite** fixtures for trailer resolution.
//!
//! A rewrite fixture is a real git repository built by real `git` invocations,
//! exercising the four operations that move a commit's sha or fuse several
//! commits into one: **squash**, **rebase**, **cherry-pick**, **amend**. It
//! exists so the trailer-survival claim ("content-addressed checkpoint ids
//! survive sha changes") is measured against git's actual behaviour rather than
//! asserted about a hand-written string.
//!
//! Determinism follows the house pattern established by `benches/native-baseline`
//! (`rng.rs` / `hermetic.rs`) — reproduced here rather than depended on, because
//! the bench harness is a separate workspace and this crate must not grow an
//! edge to it:
//!
//! * content comes from **`SplitMix64`**, never the OS RNG;
//! * every commit gets a **fixed epoch** author *and* committer date, so oids do
//!   not depend on when the fixture was built (rebase and cherry-pick rewrite
//!   the committer date, which is exactly why pinning it matters here);
//! * `git` is run **hermetically**: stub config files this module writes, an
//!   empty `core.hooksPath` (the developer's own installed Rift hooks must never
//!   fire inside a fixture), inherited `GIT_*` re-targeting variables scrubbed.
//!
//! The pay-off is [`GitFixture::build`]-level reproducibility: the same scenario
//! built twice, in two different temp directories, yields byte-identical commit
//! oids. `rewrite_resilience.rs` asserts that, which is what makes every other
//! assertion in this file about *git* rather than about *this machine*.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::TempDir;

/// First commit timestamp (2025-01-01T00:00:00Z), fixed so oids are stable.
pub(crate) const FIXTURE_EPOCH: i64 = 1_735_689_600;

/// Seconds between consecutive git invocations that can mint a commit.
const STRIDE_SECS: i64 = 60;

/// The identity every fixture commit is authored and committed by.
const IDENTITY: (&str, &str) = ("Rift Fixture", "fixture@rift.invalid");

/// Inherited `GIT_*` variables that silently re-target or re-configure a child
/// git. A developer running the suite from inside a repository (or from another
/// agent's hook) must not be able to change what a fixture builds.
///
/// `GIT_CONFIG_PARAMETERS` and `GIT_CONFIG_COUNT` are here because they inject
/// config that **outranks every file**, so redirecting `GIT_CONFIG_GLOBAL` and
/// `GIT_CONFIG_SYSTEM` at a stub — which is all this fixture used to do — does
/// not defend against them at all. `GIT_TEMPLATE_DIR` is here because
/// `git init` copies hooks out of a template.
const SCRUBBED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_TEMPLATE_DIR",
    "GIT_EXTERNAL_DIFF",
    "GIT_SSH_COMMAND",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_DATE",
    // Not config, and not content: it changes the repository FORMAT, so every
    // oid a fixture records is 64 characters instead of 40 (measured). Scrubbed
    // here *and* overridden on `git init` below, because scrubbing alone would
    // leave the fixture at the mercy of git's own default changing later.
    "GIT_DEFAULT_HASH",
];

/// Variables that must carry a **value**, not merely be absent.
///
/// `GIT_ATTR_NOSYSTEM` is the trap: it was in [`SCRUBBED`], which is backwards.
/// Removing it *re-enables* `$(prefix)/etc/gitattributes`; the protective state
/// is `1`. `GIT_CONFIG_COUNT=0` is the total form of scrubbing the
/// `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` pairs, whose count is unbounded and
/// so cannot be enumerated in [`SCRUBBED`]: git reads pairs `0..count`, so a
/// count of zero makes every pair inert whatever it holds.
const PINNED_OFF: &[(&str, &str)] = &[
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_ATTR_NOSYSTEM", "1"),
    ("GIT_CONFIG_COUNT", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
];

/// A path as a gitconfig value: `/`-separated and quoted.
///
/// git's config parser treats `\` as an escape, so an unquoted Windows path
/// (`C:\Users\…`) is "bad config line N"; forward slashes are accepted by git
/// on every platform, and the quotes keep a temp directory with a space in
/// its name one value.
pub(crate) fn config_path(path: &Path) -> String {
    format!("\"{}\"", path.display().to_string().replace('\\', "/"))
}

/// The same isolation [`GitFixture::command`] applies, for a `git` that has to
/// run **outside** a fixture.
///
/// A test that demonstrates a vector before the fixture defeats it has to build
/// its own repository, and such a command inherits the ambient environment
/// unless something stops it — a git nobody isolated, in the suite whose whole
/// subject is hostile-environment isolation. A hostile `GIT_TEMPLATE_DIR` alone
/// would execute hooks during its `init` and `commit`.
///
/// There are **two** such commands in `rewrite_resilience.rs` and both call
/// this: the closure in
/// `a_hostile_default_hash_cannot_change_what_a_fixture_builds`, and the
/// `legacy_commit` closure that shows the config vectors working. Each plants
/// its own vector back *after* calling this, so the demonstration is about the
/// one variable under test rather than about the environment it inherited; the
/// vacuity assertions in each test are what prove the plant survived the scrub.
///
/// `home` is the throwaway root this command may treat as `$HOME`; the config
/// files under it need not exist, and git ignores the ones that do not.
pub(crate) fn hermetic_env(cmd: &mut Command, home: &Path) {
    let cfg = home.join("gitconfig");
    let (name, email) = IDENTITY;
    for key in SCRUBBED {
        cmd.env_remove(key);
    }
    for (k, v) in PINNED_OFF {
        cmd.env(k, v);
    }
    cmd.env("GIT_CONFIG_GLOBAL", &cfg)
        .env("GIT_CONFIG_SYSTEM", &cfg)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("GIT_ASKPASS", "")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email)
        .env("TZ", "UTC")
        .env("LC_ALL", "C");
}

/// A `SplitMix64` generator (Steele/Lea/Flood, public-domain algorithm).
///
/// Statistical quality is irrelevant; reproducibility is the whole point.
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Seed the generator.
    pub(crate) const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next value in the stream.
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Four draws rendered as a 64-character lowercase hex string — the shape of
    /// a real Rift checkpoint id (`sha256_hex`), without pretending to *be* one:
    /// a fixture never runs condensation, so its ids stand for "some
    /// content-addressed id", and the property under test is that the id does not
    /// change when the commit's sha does.
    pub(crate) fn checkpoint_id(&mut self) -> String {
        let mut s = String::with_capacity(64);
        for _ in 0..4 {
            s.push_str(&format!("{:016x}", self.next_u64()));
        }
        s
    }
}

/// One commit as the fixture recorded it at creation time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FixtureCommit {
    /// The commit oid at the moment it was written (pre-rewrite).
    pub(crate) oid: String,
    /// The commit subject line.
    pub(crate) subject: String,
    /// The checkpoint id its `Rift-Checkpoint` trailer carries.
    pub(crate) checkpoint: String,
}

/// A hermetic, deterministic git repository under a temp root.
pub(crate) struct GitFixture {
    root: TempDir,
    repo: PathBuf,
    rng: SplitMix64,
    step: i64,
    poison: Vec<(String, String)>,
}

impl GitFixture {
    /// Create an initialized repository (branch `main`, one root commit that
    /// carries no Rift trailer — the "before Rift" history every scenario
    /// starts from).
    pub(crate) fn new(seed: u64) -> Self {
        Self::poisoned(seed, &[])
    }

    /// [`GitFixture::new`], with `poison` planted in the child environment
    /// **before** the scrub/pin pass runs — i.e. exactly as if the developer,
    /// their shell, or another agent's hook had exported it.
    ///
    /// This is how the hermetic claim is tested rather than asserted. It cannot
    /// be done by setting the variable in this process: `std::env::set_var` is
    /// `unsafe` in this edition and the workspace forbids `unsafe`. What it
    /// models is faithful in the only way that matters — `Command`'s
    /// environment is an ordered map, so a name planted here and then
    /// `env_remove`d or overwritten reaches the child in precisely the state a
    /// truly inherited one would.
    pub(crate) fn poisoned(seed: u64, poison: &[(&str, &str)]) -> Self {
        let root = TempDir::new().expect("temp root");
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        fs::create_dir_all(root.path().join("no-hooks")).expect("hooks dir");
        fs::create_dir_all(root.path().join("empty-template")).expect("template dir");
        // `core.excludesFile` and `core.attributesFile` default to
        // `$XDG_CONFIG_HOME/git/{ignore,attributes}` — paths `GIT_CONFIG_GLOBAL`
        // does NOT redirect, because they are not config files. Named
        // explicitly (and `XDG_CONFIG_HOME`/`HOME` re-pointed in `command`) so
        // a developer's own ignore rules cannot decide what a fixture commits.
        fs::write(root.path().join("empty-excludes"), "").expect("excludes stub");
        fs::write(root.path().join("empty-attributes"), "").expect("attributes stub");
        let (name, email) = IDENTITY;
        fs::write(
            root.path().join("gitconfig"),
            format!(
                "[user]\n\tname = {name}\n\temail = {email}\n\
                 [core]\n\tautocrlf = false\n\tignorecase = false\n\tsymlinks = false\n\
                 \thooksPath = {hooks}\n\texcludesFile = {excludes}\n\
                 \tattributesFile = {attributes}\n\
                 [commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n\
                 [gc]\n\tauto = 0\n[init]\n\tdefaultBranch = main\n\
                 \ttemplateDir = {template}\n\
                 [protocol]\n\tallow = never\n[advice]\n\tdetachedHead = false\n\
                 [merge]\n\tconflictStyle = merge\n",
                hooks = config_path(&root.path().join("no-hooks")),
                excludes = config_path(&root.path().join("empty-excludes")),
                attributes = config_path(&root.path().join("empty-attributes")),
                template = config_path(&root.path().join("empty-template")),
            ),
        )
        .expect("gitconfig stub");

        let mut me = Self {
            root,
            repo,
            rng: SplitMix64::new(seed),
            step: 0,
            poison: poison
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        };
        // `--object-format` is stated rather than inherited: these fixtures pin
        // 40-character oids in their expectations, and both `GIT_DEFAULT_HASH`
        // and a future change to git's default would otherwise decide it.
        me.git(&["init", "--quiet", "--object-format=sha1"]);
        // Portable across git versions that predate `init.defaultBranch`.
        me.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        me.write_file("README.md", "fixture\n");
        me.git(&["add", "-A"]);
        me.git(&["commit", "-q", "-m", "chore: import"]);
        me
    }

    /// The repository's working-tree root.
    pub(crate) fn repo(&self) -> &Path {
        &self.repo
    }

    /// Run `git args` in the repository, advancing the fixed clock first.
    /// Panics with git's stderr on failure — a fixture that cannot build is a
    /// broken test, not a skipped one.
    pub(crate) fn git(&mut self, args: &[&str]) -> String {
        self.step += 1;
        let date = format!("{} +0000", FIXTURE_EPOCH + self.step * STRIDE_SECS);
        self.run(args, Some(&date))
    }

    /// Run a read-only `git args`. Does **not** advance the clock, so adding a
    /// read to a scenario can never move a later commit's oid.
    pub(crate) fn git_read(&self, args: &[&str]) -> String {
        self.run(args, None)
    }

    fn run(&self, args: &[&str], date: Option<&str>) -> String {
        let mut cmd = self.command("git");
        cmd.args(args);
        if let Some(d) = date {
            cmd.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
        }
        let out = cmd.output().expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed ({}):\n{}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
    }

    /// The hermetically configured `git` (or other) command this fixture runs.
    ///
    /// Order is the contract: anything planted by [`GitFixture::poisoned`] goes
    /// in **first**, so the scrub and pin passes below are what the child
    /// actually sees.
    pub(crate) fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let cfg = self.root.path().join("gitconfig").display().to_string();
        let home = self.root.path().display().to_string();
        let (name, email) = IDENTITY;
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.repo);
        for (k, v) in &self.poison {
            cmd.env(k, v);
        }
        for key in SCRUBBED {
            cmd.env_remove(key);
        }
        for (k, v) in PINNED_OFF {
            cmd.env(k, v);
        }
        for (k, v) in [
            ("GIT_CONFIG_GLOBAL", cfg.as_str()),
            ("GIT_CONFIG_SYSTEM", cfg.as_str()),
            // `core.excludesFile`/`core.attributesFile` fall back to
            // `$XDG_CONFIG_HOME/git/*` (then `$HOME/.config/git/*`), which no
            // `GIT_CONFIG_*` variable redirects.
            ("HOME", home.as_str()),
            ("XDG_CONFIG_HOME", home.as_str()),
            ("GIT_ASKPASS", ""),
            // A squash-merge commit takes its message from `.git/SQUASH_MSG`
            // through the editor; a no-op editor accepts it verbatim, which is
            // precisely what `git merge --squash && git commit` does for a
            // developer who saves without editing.
            ("GIT_EDITOR", "true"),
            // Same for the rebase todo list: pinned rather than merely
            // inherited, so a developer's `sequence.editor` cannot open an
            // interactive prompt inside a fixture and hang the suite.
            ("GIT_SEQUENCE_EDITOR", "true"),
            ("GIT_AUTHOR_NAME", name),
            ("GIT_AUTHOR_EMAIL", email),
            ("GIT_COMMITTER_NAME", name),
            ("GIT_COMMITTER_EMAIL", email),
            ("TZ", "UTC"),
            ("LC_ALL", "C"),
        ] {
            cmd.env(k, v);
        }
        cmd
    }

    fn write_file(&self, rel: &str, body: &str) {
        let path = self.repo.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir");
        }
        fs::write(path, body).expect("write fixture file");
    }

    /// Write deterministic content to `rel` and commit it with a fresh
    /// `Rift-Checkpoint` trailer. Returns the commit as written.
    pub(crate) fn commit_with_checkpoint(&mut self, subject: &str, rel: &str) -> FixtureCommit {
        let body = format!("// {:016x}\nfn generated() {{}}\n", self.rng.next_u64());
        self.write_file(rel, &body);
        let checkpoint = self.rng.checkpoint_id();
        let message = format!("{subject}\n\nRift-Checkpoint: {checkpoint}\n");
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", &message]);
        FixtureCommit {
            oid: self.head(),
            subject: subject.to_owned(),
            checkpoint,
        }
    }

    /// HEAD's oid.
    pub(crate) fn head(&self) -> String {
        self.git_read(&["rev-parse", "HEAD"])
    }

    /// The full commit message of `rev`, verbatim (`%B`).
    pub(crate) fn message_of(&self, rev: &str) -> String {
        self.git_read(&["log", "-1", "--format=%B", rev])
    }
}

/// One built rewrite scenario: what git produced, and what a resolver must be
/// able to recover from it.
#[derive(Debug)]
pub(crate) struct Rewrite {
    /// Stable scenario name.
    pub(crate) name: &'static str,
    /// HEAD after the rewrite.
    pub(crate) head: String,
    /// HEAD's full commit message after the rewrite.
    pub(crate) message: String,
    /// The commits as they existed *before* the rewrite, in creation order.
    pub(crate) before: Vec<FixtureCommit>,
    /// Checkpoint ids the rewritten commit must still yield, in creation order.
    pub(crate) expected: Vec<String>,
}

impl Rewrite {
    fn from(name: &'static str, fx: &GitFixture, before: Vec<FixtureCommit>) -> Self {
        let head = fx.head();
        Self {
            name,
            message: fx.message_of(&head),
            expected: before.iter().map(|c| c.checkpoint.clone()).collect(),
            before,
            head,
        }
    }
}

/// `git merge --squash` of a three-commit feature branch, committed through the
/// editor so the message is git's own `SQUASH_MSG` — the form where each
/// constituent trailer arrives **indented four spaces** inside the
/// "Squashed commit of the following:" block.
pub(crate) fn squash_merge(fx: &mut GitFixture) -> Rewrite {
    fx.git(&["checkout", "-q", "-b", "feature"]);
    let before = vec![
        fx.commit_with_checkpoint("feat: parse", "src/parse.rs"),
        fx.commit_with_checkpoint("feat: lex", "src/lex.rs"),
        fx.commit_with_checkpoint("feat: emit", "src/emit.rs"),
    ];
    fx.git(&["checkout", "-q", "main"]);
    fx.git(&["merge", "--squash", "feature"]);
    fx.git(&["commit", "-q"]);
    Rewrite::from("squash_merge", fx, before)
}

/// The message shape a forge's "squash and merge" button lands in a repository:
/// a subject with a PR number, then each constituent commit as a `* subject`
/// bullet followed by its body — so the trailers arrive **unindented**, several
/// of them, oldest-first, in one message.
///
/// The *bullet layout* is a forge's own rendering, so it is composed here from
/// the real constituent messages and committed with `-F` — a forge is not
/// reachable from an offline test. The **shape that matters** — several column-0
/// trailers stacked oldest-first in one commit — is not hypothetical and is not
/// unique to forges: [`rebase_interactive_squash`] produces exactly it with
/// local git and nothing else, which is what makes the "trailer order is
/// anti-chronological half the time" finding a measurement rather than a
/// restatement of this fixture's own layout.
pub(crate) fn squash_forge(fx: &mut GitFixture) -> Rewrite {
    fx.git(&["checkout", "-q", "-b", "forge"]);
    let before = vec![
        fx.commit_with_checkpoint("feat: router", "src/router.rs"),
        fx.commit_with_checkpoint("feat: handler", "src/handler.rs"),
    ];
    let mut message = String::from("feat: add routing (#42)\n\n");
    for c in &before {
        message.push_str(&format!(
            "* {}\n\nRift-Checkpoint: {}\n\n",
            c.subject, c.checkpoint
        ));
    }
    fx.git(&["checkout", "-q", "main"]);
    fx.git(&["merge", "--squash", "forge"]);
    let path = fx.repo().join("forge-msg.txt");
    fs::write(&path, &message).expect("write forge message");
    let path_s = path.display().to_string();
    fx.git(&["commit", "-q", "-F", &path_s]);
    fs::remove_file(&path).expect("remove forge message");
    Rewrite::from("squash_forge", fx, before)
}

/// `git rebase -i` with `squash` — a developer tidying a branch before pushing.
///
/// This is the **local** producer of the stacked column-0 shape, and it exists
/// because a fixture that composes a message by hand can only restate what its
/// author believed; it cannot establish what git does.
///
/// The squash is driven by `--autosquash` over `squash!` subjects rather than by
/// a `GIT_SEQUENCE_EDITOR` script: autosquash rewrites the todo list *before*
/// the sequence editor runs, so the no-op editor pinned in
/// [`GitFixture::command`] performs a real `squash` action with no
/// platform-specific scripting — the technique `rift-shadow-hooks`'
/// `commit_msg_rewrites.rs` already runs on every CI leg. A hand-rewritten todo
/// (`pick` → `squash`) yields the same trailer shape with the constituent
/// subjects additionally retained; the trailers, which are what provenance
/// reads, are identical either way.
pub(crate) fn rebase_interactive_squash(fx: &mut GitFixture) -> Rewrite {
    fx.git(&["checkout", "-q", "-b", "tidy"]);
    let before = vec![
        fx.commit_with_checkpoint("feat: scan", "src/scan.rs"),
        // `squash!` targets the subject above, so autosquash folds these two
        // into it in their original order.
        fx.commit_with_checkpoint("squash! feat: scan", "src/token.rs"),
        fx.commit_with_checkpoint("squash! feat: scan", "src/render.rs"),
    ];
    fx.git(&["rebase", "-i", "--autosquash", "main"]);
    Rewrite::from("rebase_interactive_squash", fx, before)
}

/// `git merge --squash` finished with `git commit -m` instead of the editor.
///
/// The `-m` message **replaces** `.git/SQUASH_MSG` rather than seeding it, so
/// every constituent trailer is discarded and the fused commit claims nothing.
/// This is total write-side provenance loss with nothing left for any reader to
/// recover — not a parser bug, a limit of carrying provenance in the message —
/// and it is deliberately absent from [`all`], whose survival property it would
/// falsify by construction. `rewrite_resilience.rs::
/// squashing_with_an_explicit_message_discards_every_claim` states it plainly.
pub(crate) fn squash_merge_message_replaced(fx: &mut GitFixture) -> Rewrite {
    fx.git(&["checkout", "-q", "-b", "replaced"]);
    let before = vec![
        fx.commit_with_checkpoint("feat: intake", "src/intake.rs"),
        fx.commit_with_checkpoint("feat: drain", "src/drain.rs"),
    ];
    fx.git(&["checkout", "-q", "main"]);
    fx.git(&["merge", "--squash", "replaced"]);
    fx.git(&["commit", "-q", "-m", "feat: intake and drain"]);
    Rewrite::from("squash_merge_message_replaced", fx, before)
}

/// A two-commit feature branch rebased onto a moved `main`: both shas change,
/// both trailers must survive.
pub(crate) fn rebase(fx: &mut GitFixture) -> Rewrite {
    fx.git(&["checkout", "-q", "-b", "topic"]);
    let before = vec![
        fx.commit_with_checkpoint("feat: cache", "src/cache.rs"),
        fx.commit_with_checkpoint("feat: evict", "src/evict.rs"),
    ];
    fx.git(&["checkout", "-q", "main"]);
    fx.write_file("docs/notes.md", "moved on\n");
    fx.git(&["add", "-A"]);
    fx.git(&["commit", "-q", "-m", "docs: unrelated move"]);
    fx.git(&["checkout", "-q", "topic"]);
    fx.git(&["rebase", "main"]);
    Rewrite::from("rebase", fx, before)
}

/// One trailer-carrying commit cherry-picked onto another branch: new sha, same
/// trailer, and the original commit still present on its own branch.
pub(crate) fn cherry_pick(fx: &mut GitFixture) -> Rewrite {
    fx.git(&["checkout", "-q", "-b", "donor"]);
    let before = vec![fx.commit_with_checkpoint("fix: off-by-one", "src/bounds.rs")];
    fx.git(&["checkout", "-q", "main"]);
    fx.git(&["cherry-pick", &before[0].oid]);
    Rewrite::from("cherry_pick", fx, before)
}

/// A commit amended after a follow-up edit (`--no-edit`, so the message is
/// reused verbatim): new sha, trailer untouched.
pub(crate) fn amend(fx: &mut GitFixture) -> Rewrite {
    let before = vec![fx.commit_with_checkpoint("feat: config", "src/config.rs")];
    fx.write_file("src/config.rs", "fn generated() {}\n// amended\n");
    fx.git(&["add", "-A"]);
    fx.git(&["commit", "-q", "--amend", "--no-edit"]);
    Rewrite::from("amend", fx, before)
}

/// A scenario builder: turns a fresh repository into one rewritten history.
pub(crate) type Builder = fn(&mut GitFixture) -> Rewrite;

/// Build one fresh repository per scenario **whose provenance survives the
/// rewrite**, in a fixed order.
///
/// [`squash_merge_message_replaced`] is intentionally not here: it is the one
/// shape that loses every claim, so including it would falsify the survival
/// property rather than test it. It is driven directly by its own test.
pub(crate) fn all(seed: u64) -> Vec<(GitFixture, Rewrite)> {
    let builders: &[(Builder, u64)] = &[
        (squash_merge, 1),
        (squash_forge, 2),
        (rebase, 3),
        (cherry_pick, 4),
        (amend, 5),
        (rebase_interactive_squash, 6),
    ];
    builders
        .iter()
        .map(|(build, salt)| {
            let mut fx = GitFixture::new(seed ^ salt);
            let rw = build(&mut fx);
            (fx, rw)
        })
        .collect()
}
