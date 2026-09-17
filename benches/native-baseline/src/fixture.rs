//! Deterministic fixture repositories.
//!
//! The measurement plan calls for three benchmark repositories: *small*, a
//! *10k-checkpoint synthetic*, and a *large worktree*. They are **generated, never
//! cloned** — a fixture is a pure function of its [`FixtureSpec`], so the same spec
//! produces the same file bytes and therefore the same commit oids on every
//! machine. That is what lets two runs on two machines be compared at all, and it
//! keeps the harness free of any network dependency.
//!
//! A spec has two halves:
//!
//! * the **substrate** (`dirs`, `files_per_dir`, `lines_per_file`, `commits`,
//!   `files_per_commit`) — the git repository the legs operate on;
//! * the **workload** (`session_turns`) — how many scripted agent turns the
//!   scenario driver replays against a leg. Checkpoints are *not* pre-generated:
//!   each leg writes its own, in its own format, which is precisely the thing
//!   being measured. `medium` is the 10k-checkpoint fixture in that sense.
//!
//! Fixtures are cached under a root directory keyed by a digest of the spec, so
//! re-running the harness does not rebuild them.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    hermetic::{git_date, Hermetic, Hooks},
    rng::{fnv1a64_hex, SplitMix64},
    BenchError, Result,
};

/// Schema version of the on-disk fixture manifest.
pub const FIXTURE_MANIFEST_SCHEMA: u32 = 1;

/// Timestamp of the first generated commit (2025-01-01T00:00:00Z), fixed so that
/// commit oids do not depend on when the fixture was built.
const FIXTURE_EPOCH: i64 = 1_735_689_600;

/// Seconds between consecutive generated commits.
const COMMIT_STRIDE_SECS: i64 = 60;

/// The shape of one benchmark repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureSpec {
    /// Stable short name (`small` | `medium` | `large`, or a custom one).
    pub name: String,
    /// Seed for all generated content.
    pub seed: u64,
    /// Number of source directories.
    pub dirs: usize,
    /// Files per directory.
    pub files_per_dir: usize,
    /// Generated lines per file.
    pub lines_per_file: usize,
    /// Commits in the generated history (including the initial import).
    pub commits: usize,
    /// Files rewritten per follow-up commit.
    pub files_per_commit: usize,
    /// Scripted agent turns the scenario driver replays against a leg.
    pub session_turns: usize,
}

impl FixtureSpec {
    /// A handful of files and commits: the fast fixture for hot-path latency
    /// (hook round-trip, cold status) where repository size must not dominate.
    #[must_use]
    pub fn small() -> Self {
        Self {
            name: "small".into(),
            seed: 0x5EED_0001,
            dirs: 3,
            files_per_dir: 4,
            lines_per_file: 40,
            commits: 8,
            files_per_commit: 2,
            session_turns: 20,
        }
    }

    /// The checkpoint-pressure fixture: a modest tree with a long history and
    /// 10 000 scripted turns, so each leg's checkpoint storage is what scales.
    #[must_use]
    pub fn medium() -> Self {
        Self {
            name: "medium".into(),
            seed: 0x5EED_0002,
            dirs: 20,
            files_per_dir: 25,
            lines_per_file: 120,
            commits: 200,
            files_per_commit: 6,
            session_turns: 10_000,
        }
    }

    /// The large-worktree fixture: 10 000 files, so worktree hashing, status and
    /// tree surgery are what scales.
    #[must_use]
    pub fn large() -> Self {
        Self {
            name: "large".into(),
            seed: 0x5EED_0003,
            dirs: 400,
            files_per_dir: 25,
            lines_per_file: 120,
            commits: 40,
            files_per_commit: 25,
            session_turns: 200,
        }
    }

    /// The three presets the plan names, in size order.
    #[must_use]
    pub fn presets() -> [Self; 3] {
        [Self::small(), Self::medium(), Self::large()]
    }

    /// Look a preset up by name.
    #[must_use]
    pub fn preset(name: &str) -> Option<Self> {
        Self::presets().into_iter().find(|s| s.name == name)
    }

    /// Total worktree files the spec generates.
    #[must_use]
    pub fn total_files(&self) -> usize {
        self.dirs.saturating_mul(self.files_per_dir)
    }

    /// Reject a spec that cannot produce a repository.
    ///
    /// # Errors
    /// [`BenchError::Spec`] when any dimension is zero or the name is unusable as
    /// a directory component.
    pub fn validate(&self) -> Result<()> {
        let bad = |what: &str| Err(BenchError::Spec(format!("{what} must be non-zero")));
        if self.name.is_empty()
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(BenchError::Spec(
                "name must be non-empty ASCII alphanumeric/dash".into(),
            ));
        }
        if self.dirs == 0 {
            return bad("dirs");
        }
        if self.files_per_dir == 0 {
            return bad("files_per_dir");
        }
        if self.lines_per_file == 0 {
            return bad("lines_per_file");
        }
        if self.commits == 0 {
            return bad("commits");
        }
        Ok(())
    }

    /// A stable cache key for this spec.
    ///
    /// FNV-1a over the spec's canonical JSON: it names a directory, nothing more.
    ///
    /// # Errors
    /// [`BenchError::Json`] if the spec cannot be serialized.
    pub fn digest(&self) -> Result<String> {
        let json = serde_json::to_vec(self).map_err(|e| BenchError::json("fixture spec", e))?;
        Ok(fnv1a64_hex(&json))
    }

    /// `<name>-<digest>` — the on-disk directory name for this exact spec.
    ///
    /// # Errors
    /// As [`FixtureSpec::digest`].
    pub fn slug(&self) -> Result<String> {
        Ok(format!("{}-{}", self.name, self.digest()?))
    }
}

/// What a generated fixture actually contains — written beside the repository, so
/// a cached fixture can be validated without re-reading the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureManifest {
    /// Manifest schema version ([`FIXTURE_MANIFEST_SCHEMA`]).
    pub schema_version: u32,
    /// The spec this fixture was generated from.
    pub spec: FixtureSpec,
    /// The spec's cache digest.
    pub spec_digest: String,
    /// `HEAD` after generation — identical on every machine for a given spec.
    pub head_oid: String,
    /// Worktree files (excluding `.git`).
    pub worktree_files: usize,
    /// Worktree bytes (excluding `.git`).
    pub worktree_bytes: u64,
}

/// A materialized fixture on disk.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Manifest describing the repository.
    pub manifest: FixtureManifest,
    /// The repository root (a normal git worktree).
    pub repo: PathBuf,
    /// True when an existing cached fixture was reused rather than regenerated.
    pub reused: bool,
}

/// File/byte totals for a directory subtree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TreeStats {
    /// Regular files counted.
    pub files: usize,
    /// Their total byte length.
    pub bytes: u64,
}

/// Count regular files and bytes under `root`, skipping any directory whose file
/// name appears in `skip` (used to exclude `.git`).
///
/// # Errors
/// [`BenchError::Io`] if the tree cannot be walked.
pub fn tree_stats(root: &Path, skip: &[&str]) -> Result<TreeStats> {
    let mut stats = TreeStats::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| BenchError::io("walk fixture", e))?;
        for entry in entries {
            let entry = entry.map_err(|e| BenchError::io("walk fixture", e))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let meta = entry
                .metadata()
                .map_err(|e| BenchError::io("stat fixture entry", e))?;
            if meta.is_dir() {
                if !skip.contains(&name.as_ref()) {
                    stack.push(entry.path());
                }
            } else if meta.is_file() {
                stats.files += 1;
                stats.bytes += meta.len();
            }
        }
    }
    Ok(stats)
}

/// A cache of generated fixtures rooted at one directory.
#[derive(Debug, Clone)]
pub struct FixtureSet {
    root: PathBuf,
}

impl FixtureSet {
    /// Use `root` as the fixture cache directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default cache root: `$RIFT_BENCH_FIXTURE_DIR`, else `.fixtures/` beside
    /// this package (git-ignored).
    #[must_use]
    pub fn default_root() -> PathBuf {
        std::env::var_os("RIFT_BENCH_FIXTURE_DIR").map_or_else(
            || Path::new(env!("CARGO_MANIFEST_DIR")).join(".fixtures"),
            PathBuf::from,
        )
    }

    /// The cache root in use.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory this spec's fixture lives in.
    ///
    /// # Errors
    /// As [`FixtureSpec::slug`].
    pub fn dir_for(&self, spec: &FixtureSpec) -> Result<PathBuf> {
        Ok(self.root.join(spec.slug()?))
    }

    /// Return the fixture for `spec`, generating it if it is missing or stale.
    ///
    /// Regeneration is all-or-nothing: a partial directory is removed and rebuilt,
    /// so an interrupted run never leaves a half-repository to be measured.
    ///
    /// # Errors
    /// [`BenchError::Spec`] for an invalid spec, [`BenchError::Io`] /
    /// [`BenchError::Subprocess`] if generation fails.
    pub fn materialize(&self, spec: &FixtureSpec) -> Result<Fixture> {
        spec.validate()?;
        let dir = self.dir_for(spec)?;
        let repo = dir.join("repo");
        let manifest_path = dir.join("manifest.json");

        if repo.is_dir() {
            if let Some(manifest) = read_manifest(&manifest_path) {
                if manifest.schema_version == FIXTURE_MANIFEST_SCHEMA
                    && &manifest.spec == spec
                    && head_matches(&self.root, &repo, &manifest.head_oid)
                {
                    return Ok(Fixture {
                        manifest,
                        repo,
                        reused: true,
                    });
                }
            }
        }

        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| BenchError::io("clear stale fixture", e))?;
        }
        fs::create_dir_all(&repo).map_err(|e| BenchError::io("create fixture dir", e))?;
        let manifest = self.generate(spec, &repo)?;
        let body =
            serde_json::to_vec_pretty(&manifest).map_err(|e| BenchError::json("manifest", e))?;
        fs::write(&manifest_path, body).map_err(|e| BenchError::io("write manifest", e))?;
        Ok(Fixture {
            manifest,
            repo,
            reused: false,
        })
    }

    /// Generate the repository: initial import, then `commits - 1` edit commits.
    fn generate(&self, spec: &FixtureSpec, repo: &Path) -> Result<FixtureManifest> {
        let hermetic = Hermetic::prepare(self.root.join(".hermetic"))?;
        let template = hermetic.template_dir().display().to_string();
        run_git(
            &hermetic,
            repo,
            &["init", "--quiet", &format!("--template={template}")],
            &[],
        )?;

        for ix in 0..spec.total_files() {
            write_generated_file(spec, repo, ix, 0)?;
        }
        commit(&hermetic, repo, 0, "fixture: initial import")?;

        for c in 1..spec.commits {
            let mut pick = SplitMix64::mixed(&[spec.seed, 0x00C0_FFEE, c as u64]);
            for _ in 0..spec.files_per_commit {
                let ix = pick.below(spec.total_files());
                write_generated_file(spec, repo, ix, c)?;
            }
            commit(&hermetic, repo, c, &format!("fixture: edit {c}"))?;
        }

        let head_oid = run_git(&hermetic, repo, &["rev-parse", "HEAD"], &[])?
            .trim()
            .to_owned();
        let stats = tree_stats(repo, &[".git"])?;
        Ok(FixtureManifest {
            schema_version: FIXTURE_MANIFEST_SCHEMA,
            spec: spec.clone(),
            spec_digest: spec.digest()?,
            head_oid,
            worktree_files: stats.files,
            worktree_bytes: stats.bytes,
        })
    }
}

fn read_manifest(path: &Path) -> Option<FixtureManifest> {
    let body = fs::read(path).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Re-derive the cached repository's `HEAD` and check it against the manifest.
///
/// A manifest is a *claim* about a directory, not proof of it. Between two runs
/// the directory can change underneath the claim — a cloud-sync daemon evicting
/// or truncating objects (this harness's fixture caches deliberately live off
/// such a tree for exactly that reason), an interrupted `rm -rf`, a stray edit.
/// Measuring a mutilated repository while reporting the manifest's `head_oid`
/// would publish numbers for a repository that no longer exists, so the ~10 ms
/// `rev-parse` is paid on every reuse and any disagreement (including a `git`
/// that now fails outright) regenerates instead of measuring.
fn head_matches(cache_root: &Path, repo: &Path, expected: &str) -> bool {
    let Ok(hermetic) = Hermetic::prepare(cache_root.join(".hermetic")) else {
        return false;
    };
    run_git(&hermetic, repo, &["rev-parse", "HEAD"], &[]).is_ok_and(|head| head.trim() == expected)
}

/// The repo-relative path of generated file `ix`.
pub(crate) fn generated_path(spec: &FixtureSpec, ix: usize) -> PathBuf {
    let dir = ix / spec.files_per_dir;
    let file = ix % spec.files_per_dir;
    Path::new("src")
        .join(format!("mod_{dir:04}"))
        .join(format!("file_{file:04}.rs"))
}

/// The body of generated file `ix` at `revision` — pure in (seed, ix, revision).
fn generated_body(spec: &FixtureSpec, ix: usize, revision: usize) -> String {
    let mut rng = SplitMix64::mixed(&[spec.seed, ix as u64, revision as u64]);
    let mut body = String::with_capacity(spec.lines_per_file * 56);
    body.push_str(&format!(
        "//! generated fixture file {ix}, revision {revision} — do not edit\n"
    ));
    for line in 0..spec.lines_per_file {
        let mul = rng.next_u64() | 1;
        let modulo = rng.below(97) + 3;
        body.push_str(&format!(
            "pub fn f{ix}_{line}(x: u64) -> u64 {{ x.wrapping_mul({mul:#018x}) % {modulo} }}\n"
        ));
    }
    body
}

fn write_generated_file(spec: &FixtureSpec, repo: &Path, ix: usize, revision: usize) -> Result<()> {
    let path = repo.join(generated_path(spec, ix));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| BenchError::io("create fixture subdir", e))?;
    }
    fs::write(&path, generated_body(spec, ix, revision))
        .map_err(|e| BenchError::io("write fixture file", e))
}

/// Stage everything and commit with a deterministic author/committer date.
fn commit(hermetic: &Hermetic, repo: &Path, seq: usize, message: &str) -> Result<()> {
    run_git(hermetic, repo, &["add", "-A"], &[])?;
    let offset = i64::try_from(seq)
        .unwrap_or_default()
        .saturating_mul(COMMIT_STRIDE_SECS);
    let date = git_date(FIXTURE_EPOCH.saturating_add(offset));
    run_git(
        hermetic,
        repo,
        &["commit", "--quiet", "--no-verify", "-m", message],
        &[
            ("GIT_AUTHOR_DATE", date.as_str()),
            ("GIT_COMMITTER_DATE", date.as_str()),
        ],
    )
    .map(|_| ())
}

/// Run `git` in the hermetic environment, returning stdout.
fn run_git(
    hermetic: &Hermetic,
    cwd: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> Result<String> {
    let mut cmd = hermetic.command("git", cwd, Hooks::Disabled);
    cmd.args(args);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().map_err(|e| BenchError::Spawn {
        program: "git".into(),
        source: e,
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(BenchError::Subprocess {
            program: "git".into(),
            args: args.join(" "),
            status: out.status.to_string(),
            stderr: stderr.trim().chars().take(400).collect(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small enough to generate in milliseconds; shape is what matters.
    fn tiny() -> FixtureSpec {
        FixtureSpec {
            name: "tiny".into(),
            seed: 99,
            dirs: 2,
            files_per_dir: 2,
            lines_per_file: 4,
            commits: 3,
            files_per_commit: 1,
            session_turns: 2,
        }
    }

    #[test]
    fn presets_are_valid_and_match_the_declared_shapes() {
        for spec in FixtureSpec::presets() {
            spec.validate().unwrap();
        }
        assert_eq!(FixtureSpec::medium().session_turns, 10_000);
        assert_eq!(FixtureSpec::large().total_files(), 10_000);
        assert!(FixtureSpec::preset("small").is_some());
        assert!(FixtureSpec::preset("nope").is_none());
    }

    #[test]
    fn invalid_specs_are_rejected_before_any_io() {
        let mut spec = tiny();
        spec.commits = 0;
        assert!(spec.validate().is_err());
        let mut spec = tiny();
        spec.name = "../escape".into();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn digest_changes_with_every_dimension() {
        let base = tiny().digest().unwrap();
        let mut other = tiny();
        other.lines_per_file += 1;
        assert_ne!(base, other.digest().unwrap());
        assert_eq!(base, tiny().digest().unwrap());
        assert!(tiny().slug().unwrap().starts_with("tiny-"));
    }

    #[test]
    fn generated_content_is_a_pure_function_of_the_spec() {
        let spec = tiny();
        assert_eq!(generated_body(&spec, 3, 1), generated_body(&spec, 3, 1));
        assert_ne!(generated_body(&spec, 3, 1), generated_body(&spec, 3, 2));
        assert_ne!(generated_body(&spec, 3, 1), generated_body(&spec, 4, 1));
        assert_eq!(
            generated_path(&spec, 3),
            Path::new("src").join("mod_0001").join("file_0001.rs")
        );
    }

    #[test]
    fn two_independent_builds_agree_on_head_oid() {
        // The load-bearing property: a fixture is reproducible across roots (and
        // therefore across machines), so numbers taken on different hosts describe
        // the same repository.
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let one = FixtureSet::new(a.path()).materialize(&tiny()).unwrap();
        let two = FixtureSet::new(b.path()).materialize(&tiny()).unwrap();
        assert_eq!(one.manifest.head_oid, two.manifest.head_oid);
        assert_eq!(one.manifest.head_oid.len(), 40);
        assert_eq!(one.manifest.worktree_files, 4);
        assert_eq!(one.manifest.worktree_bytes, two.manifest.worktree_bytes);
        assert!(!one.reused && !two.reused);
    }

    #[test]
    fn a_cached_fixture_is_reused_and_a_changed_spec_is_not() {
        let root = tempfile::tempdir().unwrap();
        let set = FixtureSet::new(root.path());
        let first = set.materialize(&tiny()).unwrap();
        let second = set.materialize(&tiny()).unwrap();
        assert!(!first.reused && second.reused);
        assert_eq!(first.manifest.head_oid, second.manifest.head_oid);

        let mut changed = tiny();
        changed.lines_per_file = 7;
        let third = set.materialize(&changed).unwrap();
        assert!(!third.reused);
        assert_ne!(third.repo, first.repo, "a new spec gets its own directory");
        assert_ne!(third.manifest.head_oid, first.manifest.head_oid);
    }

    #[test]
    fn a_cached_repository_that_drifted_from_its_manifest_is_rebuilt() {
        // The manifest is a claim about the directory; the directory is the
        // truth. A cache that lost its history (cloud-sync eviction, an
        // interrupted delete) must never be measured while the report quotes the
        // manifest's head oid.
        let root = tempfile::tempdir().unwrap();
        let set = FixtureSet::new(root.path());
        let first = set.materialize(&tiny()).unwrap();
        assert!(!first.reused);

        let hermetic = Hermetic::prepare(root.path().join(".hermetic")).unwrap();
        run_git(
            &hermetic,
            &first.repo,
            &["reset", "--quiet", "--hard", "HEAD~1"],
            &[],
        )
        .unwrap();
        let drifted = run_git(&hermetic, &first.repo, &["rev-parse", "HEAD"], &[]).unwrap();
        assert_ne!(drifted.trim(), first.manifest.head_oid);

        let again = set.materialize(&tiny()).unwrap();
        assert!(!again.reused, "a drifted cache is regenerated, not trusted");
        assert_eq!(again.manifest.head_oid, first.manifest.head_oid);
        assert!(
            set.materialize(&tiny()).unwrap().reused,
            "and reusable again"
        );
    }

    #[test]
    fn a_cached_repository_whose_git_dir_vanished_is_rebuilt() {
        let root = tempfile::tempdir().unwrap();
        let set = FixtureSet::new(root.path());
        let first = set.materialize(&tiny()).unwrap();
        fs::remove_dir_all(first.repo.join(".git")).unwrap();
        // `rev-parse` now fails outright: an unverifiable cache is not a cache.
        assert!(!set.materialize(&tiny()).unwrap().reused);
    }

    #[test]
    fn history_depth_and_edits_land_in_the_repository() {
        let root = tempfile::tempdir().unwrap();
        let spec = tiny();
        let fixture = FixtureSet::new(root.path()).materialize(&spec).unwrap();
        let hermetic = Hermetic::prepare(root.path().join(".hermetic")).unwrap();
        let log = run_git(
            &hermetic,
            &fixture.repo,
            &["log", "--format=%s %ad", "--date=unix"],
            &[],
        )
        .unwrap();
        let lines: Vec<_> = log.lines().collect();
        assert_eq!(lines.len(), spec.commits);
        assert!(lines[lines.len() - 1].starts_with("fixture: initial import"));
        // Dates are generated, not wall-clock.
        assert!(lines[lines.len() - 1].contains(&FIXTURE_EPOCH.to_string()));
        // The tree is clean after generation: nothing untracked was left behind.
        let status = run_git(&hermetic, &fixture.repo, &["status", "--porcelain"], &[]).unwrap();
        assert!(status.trim().is_empty(), "dirty fixture: {status}");
    }
}
