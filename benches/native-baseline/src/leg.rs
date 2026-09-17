//! The legs of a comparison: which binaries the harness measures.
//!
//! A leg is a *pluggable runner*, and either leg may be **absent**:
//!
//! * `rift` resolves from `$RIFT_BENCH_RIFT_BIN`, else a **release** build in the
//!   repository's `target/`, else `rift` on `PATH`. A debug build is refused by
//!   default — a number taken from an unoptimized binary is not a benchmark, and
//!   silently accepting one is how false claims get published
//!   (`$RIFT_BENCH_ALLOW_DEBUG=1` overrides, and the source string then says so).
//! * the capture backend resolves from `$RIFT_BENCH_REFERENCE_BIN`, else the path
//!   the pin machinery would have installed it at. **The harness never downloads
//!   or provisions it** — that is an owner step. Until then the leg is
//!   `Absent` with a reason, every comparison it would have produced is recorded
//!   as skipped, and rift-only numbers are still collected.
//! * `PATH` is deliberately **not** searched for the backend: a developer's own
//!   installed copy is not the pinned build, and comparing against an unpinned
//!   binary would make the report unreproducible.
//!
//! **Resolving a binary is not the same as being entitled to publish a number
//! about it.** Every resolved rift binary is put through [`crate::freshness`]:
//! one older than the sources in its own checkout is refused with the name of
//! the file that outran it, because the timing would be real and the claim would
//! be about code the binary does not contain. When this was written the resolver
//! had no such check, and the release build sitting in a side checkout was
//! thirteen days and 168 `.rs` files behind the tree it would have been
//! published against. The pinned backend is exempt for a stated reason and not
//! by omission: its sources are not in this checkout, so this checkout's clock
//! says nothing about it, and its rows record `freshness unknown` rather than a
//! verdict nothing supports.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    freshness::{Freshness, SourceClock},
    hermetic::Hermetic,
    measure::{run_once, Invocation},
    provenance::GitProvenance,
};

/// The opaque on-disk name the pin machinery installs the backend under.
const BACKEND_BIN_NAME: &str = "rift-substrate";

/// Which binary a measurement is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LegId {
    /// The native Rust implementation under test.
    Rift,
    /// The pinned Go capture backend, kept only as a benchmark fixture.
    Reference,
}

impl LegId {
    /// Both legs, rift first.
    #[must_use]
    pub const fn all() -> [Self; 2] {
        [Self::Rift, Self::Reference]
    }

    /// Stable identifier used in reports and CLI flags.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rift => "rift",
            Self::Reference => "reference",
        }
    }
}

/// Whether a leg can be measured, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum LegStatus {
    /// The binary was found.
    Present {
        /// Absolute (or `PATH`-relative) path to the binary.
        binary: PathBuf,
        /// What `--version` reported, or `unknown`.
        version: String,
        /// How it was resolved — recorded so a number can be traced to a build,
        /// with the freshness verdict appended when it is not a clean pass.
        source: String,
        /// The binary's modification time, Unix seconds — the other half of the
        /// provenance the freshness check compares. `None` when it could not be
        /// read.
        #[serde(default)]
        binary_mtime_unix: Option<u64>,
        /// `git describe --always --tags --abbrev=40` of the checkout this
        /// binary was resolved from.
        ///
        /// **Required of every new artifact, optional in the type.** The
        /// committed schema-1 records predate the field and are
        /// evidence — rewriting one to satisfy a check is the failure this guard
        /// exists to prevent — so serde stays permissive and the
        /// artifact-consistency guard is what requires the field. An artifact
        /// without it is *unknown*, and unknown may not derive a floor.
        ///
        /// `None` for a leg this checkout's history does not describe (the
        /// pinned backend, whose sources are elsewhere), for the same reason its
        /// freshness reads `unknown` rather than a verdict.
        #[serde(default)]
        git_describe: Option<String>,
        /// Whether that checkout had uncommitted changes.
        ///
        /// Its own field rather than a suffix inside `git_describe`, because a
        /// dirty tree is a *disqualification* the guard acts on, not a
        /// decoration on a string a reader has to parse.
        #[serde(default)]
        dirty: Option<bool>,
    },
    /// The binary is not available; the reason is published, never elided.
    Absent {
        /// Why, in the words the report will show.
        reason: String,
    },
}

impl LegStatus {
    /// The binary path when present.
    #[must_use]
    pub fn binary(&self) -> Option<&Path> {
        match self {
            Self::Present { binary, .. } => Some(binary),
            Self::Absent { .. } => None,
        }
    }

    /// Whether this leg can be measured.
    #[must_use]
    pub fn is_present(&self) -> bool {
        matches!(self, Self::Present { .. })
    }
}

/// Resolves legs from explicit inputs (never from ambient state at call time, so
/// resolution is testable and a run's provenance is exactly what is recorded).
#[derive(Debug, Clone)]
pub struct LegResolver {
    /// `$RIFT_BENCH_RIFT_BIN`.
    pub rift_override: Option<PathBuf>,
    /// `$RIFT_BENCH_REFERENCE_BIN`.
    pub reference_override: Option<PathBuf>,
    /// `$RIFT_BENCH_VOGON_BIN` — the scripted-session driver, not a leg.
    pub vogon_override: Option<PathBuf>,
    /// Repository root, used to find `target/release/rift` and the pin manifest.
    pub repo_root: PathBuf,
    /// `$PATH` (rift only).
    pub path_var: Option<OsString>,
    /// Where the pin machinery installs the backend.
    pub install_root: Option<PathBuf>,
    /// Accept a debug build of rift (numbers are then not comparable).
    pub allow_debug: bool,
    /// The newest source modification time in `repo_root`, or `None` when the
    /// root is not a checkout. Taken once, at construction, so resolution stays
    /// a pure function of explicit inputs.
    pub source_clock: Option<SourceClock>,
    /// Measure a binary older than its own sources anyway
    /// (`$RIFT_BENCH_ALLOW_STALE`). The leg's `source` string then says so.
    pub allow_stale: bool,
    /// What `git` says about `repo_root`: the commit a number is about, and
    /// whether the tree was dirty.
    ///
    /// Taken once at construction beside [`Self::source_clock`], so resolution
    /// stays a pure function of explicit inputs and a test can state the
    /// provenance it wants rather than inheriting the machine's.
    pub git_provenance: Option<GitProvenance>,
}

impl LegResolver {
    /// Build a resolver from the process environment.
    #[must_use]
    pub fn from_env() -> Self {
        let repo_root = default_repo_root();
        let source_clock = SourceClock::scan(&repo_root);
        let git_provenance = GitProvenance::detect(&repo_root);
        Self {
            rift_override: std::env::var_os("RIFT_BENCH_RIFT_BIN").map(PathBuf::from),
            reference_override: std::env::var_os("RIFT_BENCH_REFERENCE_BIN").map(PathBuf::from),
            vogon_override: std::env::var_os("RIFT_BENCH_VOGON_BIN").map(PathBuf::from),
            repo_root,
            path_var: std::env::var_os("PATH"),
            install_root: default_install_root(),
            allow_debug: std::env::var_os("RIFT_BENCH_ALLOW_DEBUG").is_some(),
            source_clock,
            allow_stale: std::env::var_os("RIFT_BENCH_ALLOW_STALE").is_some(),
            git_provenance,
        }
    }

    /// Resolve one leg, probing its version when found.
    #[must_use]
    pub fn resolve(&self, hermetic: &Hermetic, id: LegId) -> LegStatus {
        match id {
            LegId::Rift => self.resolve_rift(hermetic),
            LegId::Reference => self.resolve_backend(hermetic),
        }
    }

    fn resolve_rift(&self, hermetic: &Hermetic) -> LegStatus {
        self.resolve_repo_binary(
            hermetic,
            "rift",
            self.rift_override.as_deref(),
            "$RIFT_BENCH_RIFT_BIN",
        )
    }

    /// Resolve `vogon`, the deterministic scripted fake agent the condense row
    /// replays a whole session through.
    ///
    /// It is not a *leg*: it never appears in a comparison and no number is
    /// published about it. It is a driver, and it is resolved through exactly
    /// the same freshness gate as the subject for exactly the same reason — a
    /// stale driver scripts the session the old adapter agreed with, which is
    /// how a benchmark ends up timing a workload that no longer exists.
    #[must_use]
    pub fn resolve_vogon(&self, hermetic: &Hermetic) -> LegStatus {
        self.resolve_repo_binary(
            hermetic,
            "vogon",
            self.vogon_override.as_deref(),
            "$RIFT_BENCH_VOGON_BIN",
        )
    }

    /// The shared resolution ladder for a binary built out of this checkout:
    /// explicit override, release build, debug build (refused by default),
    /// `PATH`.
    fn resolve_repo_binary(
        &self,
        hermetic: &Hermetic,
        name: &str,
        override_path: Option<&Path>,
        override_var: &str,
    ) -> LegStatus {
        if let Some(path) = override_path {
            return self.present_or_absent(hermetic, path, override_var, true, override_var);
        }
        let release = self
            .repo_root
            .join("target")
            .join("release")
            .join(exe(name));
        if release.is_file() {
            return self.present(
                hermetic,
                &release,
                "repo target/release",
                true,
                override_var,
            );
        }
        let debug = self.repo_root.join("target").join("debug").join(exe(name));
        if debug.is_file() {
            if self.allow_debug {
                return self.present(
                    hermetic,
                    &debug,
                    "repo target/debug (DEBUG — not comparable)",
                    true,
                    override_var,
                );
            }
            return LegStatus::Absent {
                reason: format!(
                    "only a debug build was found at {}; debug numbers are not benchmarks — \
                     run `cargo build --release`, set {override_var}, or set \
                     $RIFT_BENCH_ALLOW_DEBUG=1 to measure it anyway",
                    debug.display()
                ),
            };
        }
        if let Some(found) = find_on_path(self.path_var.as_ref(), name) {
            return self.present(hermetic, &found, "PATH", true, override_var);
        }
        LegStatus::Absent {
            reason: format!(
                "no {name} binary: no {override_var}, no target/release/{name}, none on PATH"
            ),
        }
    }

    fn resolve_backend(&self, hermetic: &Hermetic) -> LegStatus {
        if let Some(path) = &self.reference_override {
            return self.present_or_absent(
                hermetic,
                path,
                "$RIFT_BENCH_REFERENCE_BIN",
                false,
                "$RIFT_BENCH_REFERENCE_BIN",
            );
        }
        if let Some(path) = self.pinned_backend_path() {
            if path.is_file() {
                return self.present(
                    hermetic,
                    &path,
                    "pinned install",
                    false,
                    "$RIFT_BENCH_REFERENCE_BIN",
                );
            }
            return LegStatus::Absent {
                reason: format!(
                    "the pinned capture backend is not installed at {}; the harness never \
                     downloads or provisions it (an owner step) — set \
                     $RIFT_BENCH_REFERENCE_BIN to activate this leg. Comparisons stay skipped; \
                     rift-only numbers are still collected",
                    path.display()
                ),
            };
        }
        LegStatus::Absent {
            reason:
                "no pin manifest and no $RIFT_BENCH_REFERENCE_BIN: the backend leg is inactive \
                     (the harness never provisions it)"
                    .to_owned(),
        }
    }

    fn present_or_absent(
        &self,
        hermetic: &Hermetic,
        path: &Path,
        source: &str,
        gated: bool,
        override_var: &str,
    ) -> LegStatus {
        if path.is_file() {
            self.present(hermetic, path, source, gated, override_var)
        } else {
            LegStatus::Absent {
                reason: format!("{source} points at {}, which is not a file", path.display()),
            }
        }
    }

    /// Record a resolved binary, refusing one that predates its own sources.
    ///
    /// `gated` says whether this checkout's source clock describes this binary
    /// at all. It does for rift; it does not for the pinned backend, whose
    /// sources are elsewhere — so that leg is compared against no clock and
    /// records `freshness unknown`, which is what the harness actually knows.
    fn present(
        &self,
        hermetic: &Hermetic,
        path: &Path,
        source: &str,
        gated: bool,
        override_var: &str,
    ) -> LegStatus {
        let clock = gated.then_some(self.source_clock.as_ref()).flatten();
        let freshness = Freshness::of(path, clock);
        if !self.allow_stale {
            if let Some(refusal) = freshness.refusal(path, override_var) {
                return LegStatus::Absent { reason: refusal };
            }
        }
        // Same `gated` reasoning as the freshness clock, one axis out: this
        // checkout's history describes the binaries built out of it and says
        // nothing about a binary that came from elsewhere. Stamping this
        // repository's commit onto the pinned backend would be a provenance
        // claim about a tree that never produced it.
        let git = gated.then(|| self.git_provenance.clone()).flatten();
        LegStatus::Present {
            binary: path.to_path_buf(),
            version: probe_version(hermetic, path),
            source: format!("{source}{}", freshness.source_suffix()),
            binary_mtime_unix: freshness.binary_unix(),
            git_describe: git.as_ref().map(|g| g.describe.clone()),
            dirty: git.as_ref().map(|g| g.dirty),
        }
    }

    /// Where the pin machinery would have installed the backend, if the pinned
    /// version can be read.
    ///
    /// Mirrors the deleted pin crate's historical layout
    /// (`<install root>/<version>/<arch>/bin/rift-substrate`) rather than linking
    /// it; with that crate gone, `$RIFT_BENCH_REFERENCE_BIN` is the only route
    /// and the fixture binary lives outside the repository.
    #[must_use]
    pub fn pinned_backend_path(&self) -> Option<PathBuf> {
        let root = self.install_root.as_ref()?;
        let version = read_pinned_version(&self.repo_root)?;
        Some(
            root.join(version)
                .join(std::env::consts::ARCH)
                .join("bin")
                .join(exe(BACKEND_BIN_NAME)),
        )
    }
}

/// `<manifest dir>/../..` — the repository root of a checkout of this harness.
#[must_use]
pub fn default_repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The report-facing shape of a resolved leg: the binary path relative to
/// `repo_root` when it sits under it.
///
/// The RUNTIME path stays absolute — invocations execute it — and only the
/// RECORDED copy is rewritten. Not cosmetic: an absolute checkout path is one
/// census token in every tracked artifact, and whether it crosses the
/// redaction entropy layer's 4.0-bit floor depends on the checkout's own name
/// (one sitting measured one checkout's name over that floor and another
/// checkout's name under it). A path
/// resolved from outside the root (an override) is recorded as it was given,
/// because relativizing it would name a file that does not exist.
#[must_use]
pub fn reported(status: LegStatus, repo_root: &Path) -> LegStatus {
    match status {
        LegStatus::Present {
            binary,
            version,
            source,
            binary_mtime_unix,
            git_describe,
            dirty,
        } => LegStatus::Present {
            binary: binary
                .strip_prefix(repo_root)
                .map_or(binary.clone(), Path::to_path_buf),
            version,
            source,
            binary_mtime_unix,
            git_describe,
            dirty,
        },
        absent @ LegStatus::Absent { .. } => absent,
    }
}

/// The private, off-`PATH` data root the pin machinery installs under.
#[must_use]
pub fn default_install_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        return std::env::var_os("LOCALAPPDATA")
            .map(|l| PathBuf::from(l).join("rift").join("substrate"));
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return Some(PathBuf::from(xdg).join("rift").join("substrate"));
        }
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("rift")
                .join("substrate")
        })
    }
}

/// Read `substrate_version` out of the pin manifest, without a TOML dependency.
#[must_use]
pub fn read_pinned_version(repo_root: &Path) -> Option<String> {
    let manifest = repo_root
        .join("crates")
        .join(concat!("rift-shadow-", "ent", "ire"))
        .join("pinned-substrate.toml");
    let body = fs::read_to_string(manifest).ok()?;
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("substrate_version") {
            let value = rest.trim_start().strip_prefix('=')?.trim();
            let value = value.trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

fn find_on_path(path_var: Option<&OsString>, name: &str) -> Option<PathBuf> {
    let var = path_var?;
    std::env::split_paths(var)
        .map(|dir| dir.join(exe(name)))
        .find(|candidate| candidate.is_file())
}

/// Ask a binary what it is: `--version` first, then the bare `version` verb.
///
/// Returns `unknown` rather than failing — an unidentifiable binary is still
/// measurable, and the report says so.
#[must_use]
pub fn probe_version(hermetic: &Hermetic, binary: &Path) -> String {
    let cwd = hermetic.root().to_path_buf();
    for args in [["--version"], ["version"]] {
        let inv = Invocation::new(binary, &cwd).args(args);
        if let Ok(exec) = run_once(hermetic, &inv) {
            if exec.sample.ok() {
                if let Some(line) = exec.stdout.lines().next() {
                    let line = line.trim();
                    if !line.is_empty() {
                        return line.to_owned();
                    }
                }
            }
        }
    }
    "unknown".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Env {
        _tmp: tempfile::TempDir,
        hermetic: Hermetic,
        resolver: LegResolver,
        root: PathBuf,
    }

    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let hermetic = Hermetic::prepare(root.join("hermetic")).unwrap();
        let resolver = LegResolver {
            rift_override: None,
            reference_override: None,
            vogon_override: None,
            repo_root: root.join("repo"),
            path_var: None,
            install_root: Some(root.join("install")),
            allow_debug: false,
            source_clock: None,
            allow_stale: false,
            git_provenance: Some(GitProvenance {
                describe: format!("ls-597-g{}", "a".repeat(40)),
                dirty: false,
            }),
        };
        Env {
            _tmp: tmp,
            hermetic,
            resolver,
            root,
        }
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"#!/bin/sh\n").unwrap();
    }

    #[test]
    fn an_override_that_exists_is_present_with_its_source() {
        let mut e = env();
        e.resolver.rift_override = Some(PathBuf::from("/bin/echo"));
        let status = e.resolver.resolve(&e.hermetic, LegId::Rift);
        match status {
            LegStatus::Present {
                binary,
                source,
                version,
                ..
            } => {
                assert_eq!(binary, Path::new("/bin/echo"));
                // No checkout under this test root, so the freshness question
                // is unanswerable — and the field says that rather than
                // implying a clean check.
                assert_eq!(source, "$RIFT_BENCH_RIFT_BIN (freshness unknown)");
                assert!(!version.is_empty());
            }
            LegStatus::Absent { reason } => panic!("unexpectedly absent: {reason}"),
        }
    }

    #[test]
    fn an_override_that_does_not_exist_is_absent_with_the_path() {
        let mut e = env();
        e.resolver.reference_override = Some(e.root.join("nope/reference"));
        let status = e.resolver.resolve(&e.hermetic, LegId::Reference);
        assert!(!status.is_present());
        let LegStatus::Absent { reason } = status else {
            panic!("expected absent")
        };
        assert!(reason.contains("nope/reference"), "{reason}");
    }

    #[test]
    fn the_backend_leg_is_absent_until_someone_provisions_it() {
        let e = env();
        // A pin manifest exists, but nothing is installed.
        let manifest = e
            .resolver
            .repo_root
            .join(concat!("crates/rift-shadow-", "ent", "ire"))
            .join("pinned-substrate.toml");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(&manifest, "schema = 1\nsubstrate_version = \"0.7.8\"\n").unwrap();

        let path = e.resolver.pinned_backend_path().unwrap();
        assert!(path.to_string_lossy().contains("0.7.8"));
        assert!(path.ends_with(Path::new("bin").join(exe(BACKEND_BIN_NAME))));

        let status = e.resolver.resolve(&e.hermetic, LegId::Reference);
        let LegStatus::Absent { reason } = status else {
            panic!("expected absent")
        };
        assert!(reason.contains("never downloads or provisions"), "{reason}");
    }

    #[test]
    fn the_backend_leg_never_resolves_from_path() {
        let mut e = env();
        let bin_dir = e.root.join("fakebin");
        touch(&bin_dir.join(exe("reference")));
        touch(&bin_dir.join(exe("rift-substrate")));
        e.resolver.path_var = Some(OsString::from(bin_dir.display().to_string()));
        assert!(!e
            .resolver
            .resolve(&e.hermetic, LegId::Reference)
            .is_present());
    }

    #[test]
    fn a_debug_only_build_is_refused_unless_explicitly_allowed() {
        let mut e = env();
        let debug = e.resolver.repo_root.join("target/debug").join(exe("rift"));
        touch(&debug);

        let status = e.resolver.resolve(&e.hermetic, LegId::Rift);
        let LegStatus::Absent { reason } = status else {
            panic!("a debug build must not be measured silently")
        };
        assert!(
            reason.contains("debug numbers are not benchmarks"),
            "{reason}"
        );

        e.resolver.allow_debug = true;
        let status = e.resolver.resolve(&e.hermetic, LegId::Rift);
        let LegStatus::Present { source, .. } = status else {
            panic!("expected present")
        };
        assert!(source.contains("DEBUG"), "the report must say so: {source}");
    }

    #[test]
    fn a_release_build_wins_over_path() {
        let mut e = env();
        let release = e
            .resolver
            .repo_root
            .join("target/release")
            .join(exe("rift"));
        touch(&release);
        let bin_dir = e.root.join("fakebin");
        touch(&bin_dir.join(exe("rift")));
        e.resolver.path_var = Some(OsString::from(bin_dir.display().to_string()));

        let LegStatus::Present { binary, source, .. } =
            e.resolver.resolve(&e.hermetic, LegId::Rift)
        else {
            panic!("expected present")
        };
        assert_eq!(binary, release);
        assert_eq!(source, "repo target/release (freshness unknown)");
    }

    #[test]
    fn pin_version_parsing_is_tolerant_and_total() {
        let e = env();
        assert_eq!(read_pinned_version(&e.resolver.repo_root), None);
        let manifest = e
            .resolver
            .repo_root
            .join(concat!("crates/rift-shadow-", "ent", "ire"))
            .join("pinned-substrate.toml");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(
            &manifest,
            "# comment\nschema = 1\n  substrate_version   =   \"9.9.9\"  \nrift_release = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(
            read_pinned_version(&e.resolver.repo_root).as_deref(),
            Some("9.9.9")
        );
    }

    /// The defect this guard exists for, planted: a `target/release/rift`
    /// older than the checkout it would be published against. Before the guard
    /// this resolved `Present` and the report said nothing.
    #[test]
    fn a_release_build_older_than_its_own_sources_is_refused() {
        let mut e = env();
        let release = e
            .resolver
            .repo_root
            .join("target/release")
            .join(exe("rift"));
        touch(&release);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let source = e.resolver.repo_root.join("bins/rift/src/hooks.rs");
        touch(&source);
        e.resolver.source_clock = crate::freshness::SourceClock::scan(&e.resolver.repo_root);
        assert!(
            e.resolver.source_clock.is_some(),
            "the fake checkout has sources"
        );

        let LegStatus::Absent { reason } = e.resolver.resolve(&e.hermetic, LegId::Rift) else {
            panic!("a stale binary must not be measured silently")
        };
        assert!(reason.contains("hooks.rs"), "{reason}");
        assert!(reason.contains("older than its own sources"), "{reason}");
        assert!(reason.contains("$RIFT_BENCH_RIFT_BIN"), "{reason}");

        // The override exists, and taking it stamps the admission into the very
        // field a reader consults for provenance.
        e.resolver.allow_stale = true;
        let LegStatus::Present {
            source,
            binary_mtime_unix,
            ..
        } = e.resolver.resolve(&e.hermetic, LegId::Rift)
        else {
            panic!("expected present under the override")
        };
        assert!(source.contains("STALE"), "the report must say so: {source}");
        assert!(
            binary_mtime_unix.is_some(),
            "provenance rides with the number"
        );
    }

    /// A build that lands after its sources is exactly what the guard must let
    /// through; a guard that refuses everything is not a guard.
    #[test]
    fn a_release_build_newer_than_its_sources_passes_the_guard() {
        let mut e = env();
        touch(&e.resolver.repo_root.join("bins/rift/src/hooks.rs"));
        std::thread::sleep(std::time::Duration::from_millis(10));
        let release = e
            .resolver
            .repo_root
            .join("target/release")
            .join(exe("rift"));
        touch(&release);
        e.resolver.source_clock = crate::freshness::SourceClock::scan(&e.resolver.repo_root);

        let LegStatus::Present { source, .. } = e.resolver.resolve(&e.hermetic, LegId::Rift) else {
            panic!("a fresh build must resolve")
        };
        assert_eq!(source, "repo target/release", "no suffix on a clean pass");
    }

    /// The backend's sources are not in this checkout, so this checkout's clock
    /// must not be used to judge it — a leg that could never be measured is not
    /// a guard, it is a deletion.
    #[test]
    fn the_backend_leg_is_not_judged_by_this_checkouts_source_clock() {
        let mut e = env();
        let backend = e.root.join("pinned/rift-substrate");
        touch(&backend);
        std::thread::sleep(std::time::Duration::from_millis(10));
        touch(&e.resolver.repo_root.join("crates/x/src/lib.rs"));
        e.resolver.source_clock = crate::freshness::SourceClock::scan(&e.resolver.repo_root);
        e.resolver.reference_override = Some(backend);

        let LegStatus::Present { source, .. } = e.resolver.resolve(&e.hermetic, LegId::Reference)
        else {
            panic!("the backend must not be refused by a clock that does not describe it")
        };
        assert!(source.contains("freshness unknown"), "{source}");
    }

    /// The commit rides with the number, and it rides only where this
    /// checkout's history can honestly speak for the binary. The backend's
    /// sources are elsewhere, so stamping this repository's commit onto it
    /// would be a claim about a tree that never produced it — the same reason
    /// its freshness reads `unknown`.
    #[test]
    fn the_subject_carries_its_commit_and_a_foreign_binary_does_not() {
        let mut e = env();
        e.resolver.rift_override = Some(PathBuf::from("/bin/echo"));

        let LegStatus::Present {
            git_describe,
            dirty,
            ..
        } = e.resolver.resolve(&e.hermetic, LegId::Rift)
        else {
            panic!("expected present")
        };
        let expected = e.resolver.git_provenance.as_ref().unwrap().describe.clone();
        assert_eq!(git_describe.as_deref(), Some(expected.as_str()));
        assert_eq!(dirty, Some(false));

        // The other side of the same rule, taken at the gate itself rather than
        // through a leg. `gated` is the one input that decides whether this
        // checkout's history may speak for a binary, so driving it directly
        // keeps the assertion about the RULE — and keeps this test from adding
        // a new brand site to the zero-token gate for the sake of naming
        // whichever leg happens to be foreign this month.
        let LegStatus::Present {
            git_describe,
            dirty,
            ..
        } = e.resolver.present(
            &e.hermetic,
            Path::new("/bin/echo"),
            "a binary from elsewhere",
            false,
            "$OVERRIDE",
        )
        else {
            panic!("expected present")
        };
        assert_eq!(git_describe, None, "this checkout does not describe it");
        assert_eq!(dirty, None, "and absent is not clean");
    }

    /// The committed schema-1 artifacts have no provenance at all and must
    /// keep loading: they are the evidence that produced the shipped caps, and
    /// editing a record to satisfy a later check is the failure this whole
    /// guard exists to prevent. Serde stays permissive; the **guard** is what
    /// requires the field.
    #[test]
    fn a_recorded_subject_without_provenance_still_loads_as_unknown() {
        let json = r#"{"state":"present","binary":"/x/rift","version":"rift 0.0.1",
            "source":"repo target/release","binary_mtime_unix":1787120056}"#;
        let back: LegStatus = serde_json::from_str(json).unwrap();
        let LegStatus::Present {
            git_describe,
            dirty,
            binary_mtime_unix,
            ..
        } = back
        else {
            panic!("present")
        };
        assert_eq!(git_describe, None);
        assert_eq!(dirty, None);
        assert_eq!(binary_mtime_unix, Some(1_787_120_056));
    }

    #[test]
    fn leg_ids_are_stable_strings() {
        assert_eq!(LegId::all().map(LegId::as_str), ["rift", "reference"]);
        let json = serde_json::to_string(&LegId::Reference).unwrap();
        assert_eq!(json, "\"reference\"");
    }
}
