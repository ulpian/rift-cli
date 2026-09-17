//! Does the binary about to be measured contain the code being claimed about?
//!
//! A benchmark that times a binary it did not build is not a benchmark of the
//! tree it is published against, and nothing about the timing says so: the
//! number is real, reproducible and about the wrong bytes. This program has the
//! defect on the record twice. A test suite drove a `target/debug/rift` that
//! `cargo test -p rift-shadow-hooks` never relinks and stayed green with a
//! planted defect in it — proven by mtime, not argued, when the planted run left
//! `target/debug/rift` unchanged while the suite's own build directory advanced.
//! And when this module was written, [`crate::leg::LegResolver`] resolved the
//! rift leg to `<repo>/target/release/rift` with no freshness check at all: in
//! one side checkout that file was **thirteen days and 168 `.rs` files** behind
//! the tree it would have been published against.
//!
//! So the harness answers the question mechanically. [`SourceClock::scan`] takes
//! the newest modification time over the files a `cargo build -p rift` would
//! recompile from; [`Freshness::of`] compares a resolved binary against it. A
//! binary older than its own sources is **refused**, in the words of the file
//! that outran it, exactly as a debug build already was — and for the same
//! reason: silently accepting one is how false claims get published.
//!
//! The check is deliberately *conservative in one direction only*. Touching a
//! source without rebuilding (a rebase, a `cargo fmt`, a checkout) marks the
//! binary stale, which costs a rebuild; a build that lands after every source is
//! never marked stale, which is the property that matters. Setting
//! `$RIFT_BENCH_ALLOW_STALE=1` overrides the refusal, and the leg's recorded
//! `source` string then says `STALE`, so a report taken that way carries the
//! admission rather than hiding it.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

/// Directory names never descended into: build output, VCS metadata, and the
/// harness's own generated fixture/sandbox trees (which contain whole
/// repositories and would make the walk unbounded).
const SKIPPED_DIRS: &[&str] = &["target", ".git", ".fixtures", ".runs", "node_modules"];

/// Top-level entries a `cargo build` of the product reads.
///
/// `crates` and `bins` hold the code; the manifests and the lockfile decide
/// which dependency versions are compiled into it, and the toolchain file
/// decides which compiler does it. A change to any of them changes the binary,
/// so any of them being newer than the binary means the binary is not it.
const SOURCE_ROOTS: &[&str] = &[
    "crates",
    "bins",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
];

/// File extensions that participate in the clock.
const SOURCE_EXTENSIONS: &[&str] = &["rs", "toml", "lock"];

/// The newest source modification time in a checkout, with the file that
/// carried it.
///
/// The path is part of the value on purpose: a refusal that says only "stale"
/// sends the reader looking, while one that names `bins/rift/src/hooks.rs` is
/// actionable in one line. `files` is the corroboration probe on the instrument
/// itself — a clock that walked eleven files and found nothing new is not
/// evidence that nothing is new, and the count is what distinguishes the two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceClock {
    /// Newest modification time seen, Unix seconds.
    pub newest_unix: u64,
    /// The file that carried it, relative to the scanned root.
    pub newest_path: PathBuf,
    /// How many files the walk examined.
    pub files: usize,
    /// Sub-second part of `newest_unix`.
    ///
    /// Carried separately rather than folded into a float because the
    /// comparison against a binary's own timestamp has to be exact: without it
    /// a source written at `T.900` and a binary written at `T.100` would both
    /// truncate to `T` and the stale one would pass. `#[serde(default)]` so a
    /// schema-3 report still deserializes, at whole-second resolution.
    #[serde(default)]
    pub newest_subsec_nanos: u32,
}

impl SourceClock {
    /// Walk `repo_root`'s source set and take the newest modification time.
    ///
    /// `None` when the walk saw no source file at all — a root that is not a
    /// checkout. That is reported as "unknown", never as "fresh": a clock that
    /// could not read the tree has nothing to say about a binary.
    #[must_use]
    pub fn scan(repo_root: &Path) -> Option<Self> {
        let mut newest: Option<(SystemTime, PathBuf)> = None;
        let mut files = 0usize;
        let mut stack: Vec<PathBuf> = SOURCE_ROOTS.iter().map(|r| repo_root.join(r)).collect();
        while let Some(path) = stack.pop() {
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_symlink() {
                // A symlinked source tree is followed by cargo but its target may
                // sit outside the checkout fully; walking it would make the
                // clock depend on something the report cannot name. Skipped, and
                // the `files` count is what reveals a tree that went missing.
                continue;
            }
            if meta.is_dir() {
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| SKIPPED_DIRS.contains(&n))
                {
                    continue;
                }
                let Ok(entries) = fs::read_dir(&path) else {
                    continue;
                };
                stack.extend(entries.flatten().map(|e| e.path()));
                continue;
            }
            if !is_source(&path) {
                continue;
            }
            files += 1;
            let Ok(mtime) = meta.modified() else {
                continue;
            };
            if newest.as_ref().is_none_or(|(seen, _)| mtime > *seen) {
                let rel = path.strip_prefix(repo_root).unwrap_or(&path).to_path_buf();
                newest = Some((mtime, rel));
            }
        }
        let (mtime, newest_path) = newest?;
        let since = mtime.duration_since(UNIX_EPOCH).unwrap_or_default();
        Some(Self {
            newest_unix: since.as_secs(),
            newest_path,
            files,
            newest_subsec_nanos: since.subsec_nanos(),
        })
    }

    /// The clock as a `SystemTime`, for comparison at full precision.
    #[must_use]
    pub fn as_system_time(&self) -> SystemTime {
        UNIX_EPOCH
            + std::time::Duration::new(self.newest_unix, self.newest_subsec_nanos.min(999_999_999))
    }
}

/// Whether a resolved binary can be published a number against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// The binary is at least as new as every source file the clock saw.
    Fresh {
        /// The binary's modification time, Unix seconds.
        binary_unix: u64,
    },
    /// Source is newer than the binary: the number would be about code the
    /// binary does not contain.
    Stale {
        /// The binary's modification time, Unix seconds.
        binary_unix: u64,
        /// The clock that outran it.
        clock: SourceClock,
    },
    /// The comparison could not be made — no clock, or an unreadable binary.
    /// Distinct from `Fresh` because "we did not check" is not "we checked".
    Unknown {
        /// Why, in the words the report will show.
        reason: String,
    },
}

impl Freshness {
    /// Compare `binary` against `clock`.
    ///
    /// Comparison is at the platform's own timestamp precision, not truncated
    /// seconds: a build that lands in the same second as the edit it contains
    /// must not be judged stale on a rounding artifact.
    #[must_use]
    pub fn of(binary: &Path, clock: Option<&SourceClock>) -> Self {
        let Ok(meta) = fs::metadata(binary) else {
            return Self::Unknown {
                reason: format!("{} could not be stat'ed", binary.display()),
            };
        };
        let Ok(mtime) = meta.modified() else {
            return Self::Unknown {
                reason: format!(
                    "{} has no modification time on this platform",
                    binary.display()
                ),
            };
        };
        let binary_unix = unix_secs(mtime);
        let Some(clock) = clock else {
            return Self::Unknown {
                reason: "no source clock: the harness could not find a checkout to compare \
                         the binary against"
                    .to_owned(),
            };
        };
        if clock.as_system_time() > mtime {
            return Self::Stale {
                binary_unix,
                clock: clock.clone(),
            };
        }
        Self::Fresh { binary_unix }
    }

    /// The binary's modification time, when it could be read.
    #[must_use]
    pub fn binary_unix(&self) -> Option<u64> {
        match self {
            Self::Fresh { binary_unix } | Self::Stale { binary_unix, .. } => Some(*binary_unix),
            Self::Unknown { .. } => None,
        }
    }

    /// The refusal a stale binary earns, naming the file that outran it and the
    /// escape hatch — or `None` when there is nothing to refuse.
    ///
    /// `override_var` is the environment variable that points at *this* binary.
    /// It is a parameter rather than a constant because it was a constant, and
    /// the message then told a reader whose `vogon` was stale to set
    /// `$RIFT_BENCH_RIFT_BIN` — an instruction that would have resolved a
    /// different binary and left the stale one exactly where it was.
    #[must_use]
    pub fn refusal(&self, binary: &Path, override_var: &str) -> Option<String> {
        let Self::Stale { binary_unix, clock } = self else {
            return None;
        };
        Some(format!(
            "{} was built at {binary_unix} but {} was modified at {} ({} source files \
             scanned): a benchmark of a binary older than its own sources publishes numbers \
             for code that is not in it — rebuild it, point {override_var} at the build you \
             mean, or set $RIFT_BENCH_ALLOW_STALE=1 to measure it anyway",
            binary.display(),
            clock.newest_path.display(),
            clock.newest_unix,
            clock.files,
        ))
    }

    /// The suffix a leg's recorded `source` string carries when a stale binary
    /// was measured anyway. A report taken with the override says so in the same
    /// field that says where the binary came from.
    #[must_use]
    pub fn source_suffix(&self) -> &'static str {
        match self {
            Self::Fresh { .. } => "",
            Self::Stale { .. } => " (STALE — older than its own sources)",
            Self::Unknown { .. } => " (freshness unknown)",
        }
    }
}

fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOURCE_EXTENSIONS.contains(&e))
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A checkout-shaped tree: `crates/x/src/lib.rs` plus the manifests.
    fn checkout(root: &Path) {
        fs::create_dir_all(root.join("crates/x/src")).unwrap();
        fs::create_dir_all(root.join("bins/rift/src")).unwrap();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.join("crates/x/src/lib.rs"), "pub fn a() {}\n").unwrap();
        fs::write(root.join("bins/rift/src/main.rs"), "fn main() {}\n").unwrap();
    }

    #[test]
    fn the_clock_names_the_newest_file_and_counts_what_it_walked() {
        let tmp = tempfile::tempdir().unwrap();
        checkout(tmp.path());
        let first = SourceClock::scan(tmp.path()).unwrap();
        assert_eq!(first.files, 3, "two sources plus the manifest");

        // Written last, so it is the newest.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(tmp.path().join("bins/rift/src/hooks.rs"), "// later\n").unwrap();
        let second = SourceClock::scan(tmp.path()).unwrap();
        assert_eq!(second.newest_path, Path::new("bins/rift/src/hooks.rs"));
        assert_eq!(second.files, 4);
    }

    #[test]
    fn build_output_and_vcs_metadata_are_never_walked() {
        let tmp = tempfile::tempdir().unwrap();
        checkout(tmp.path());
        // A `target/` full of `.rs` and `.toml` under `crates/` — cargo's own
        // output, which is always newer than the sources and would pin the clock
        // to the future forever if it were walked.
        fs::create_dir_all(tmp.path().join("crates/x/target/debug/build")).unwrap();
        fs::write(
            tmp.path().join("crates/x/target/debug/build/out.rs"),
            "//\n",
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join(".git")).unwrap();
        fs::write(tmp.path().join(".git/config.toml"), "x=1\n").unwrap();
        assert_eq!(SourceClock::scan(tmp.path()).unwrap().files, 3);
    }

    #[test]
    fn a_root_that_is_not_a_checkout_has_no_clock_rather_than_an_empty_one() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(SourceClock::scan(tmp.path()).is_none());
    }

    #[test]
    fn a_binary_older_than_its_sources_is_stale_and_the_refusal_names_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("rift");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        checkout(tmp.path());
        fs::write(tmp.path().join("crates/x/src/late.rs"), "// after\n").unwrap();

        let clock = SourceClock::scan(tmp.path()).unwrap();
        let freshness = Freshness::of(&bin, Some(&clock));
        assert!(
            matches!(freshness, Freshness::Stale { .. }),
            "{freshness:?} — the binary predates every source file here"
        );
        let refusal = freshness.refusal(&bin, "$RIFT_BENCH_VOGON_BIN").unwrap();
        assert!(refusal.contains("late.rs"), "{refusal}");
        assert!(refusal.contains("RIFT_BENCH_ALLOW_STALE"), "{refusal}");
        // The variable named must be the one that points at THIS binary: a
        // refusal that names another binary's override would resolve something
        // else and leave the stale one exactly where it was.
        assert!(refusal.contains("$RIFT_BENCH_VOGON_BIN"), "{refusal}");
        assert!(!refusal.contains("$RIFT_BENCH_RIFT_BIN"), "{refusal}");
        assert!(freshness.source_suffix().contains("STALE"));
    }

    #[test]
    fn a_binary_written_after_its_sources_is_fresh_and_earns_no_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        checkout(tmp.path());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let bin = tmp.path().join("rift");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();

        let clock = SourceClock::scan(tmp.path()).unwrap();
        let freshness = Freshness::of(&bin, Some(&clock));
        assert!(
            matches!(freshness, Freshness::Fresh { .. }),
            "{freshness:?}"
        );
        assert!(freshness.refusal(&bin, "$RIFT_BENCH_RIFT_BIN").is_none());
        assert_eq!(freshness.source_suffix(), "");
        assert!(freshness.binary_unix().unwrap() > 0);
    }

    /// "We did not check" must never render as "we checked and it was fine":
    /// the two would be indistinguishable in the report, which is the whole
    /// failure mode this module exists to close.
    #[test]
    fn an_unmeasurable_pair_is_unknown_and_never_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("rift");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let no_clock = Freshness::of(&bin, None);
        assert!(
            matches!(no_clock, Freshness::Unknown { .. }),
            "{no_clock:?}"
        );
        assert!(no_clock.source_suffix().contains("unknown"));
        assert!(
            no_clock.refusal(&bin, "$RIFT_BENCH_RIFT_BIN").is_none(),
            "unknown is not a refusal"
        );

        let missing = Freshness::of(&tmp.path().join("nope"), None);
        assert!(matches!(missing, Freshness::Unknown { .. }), "{missing:?}");
        assert_eq!(missing.binary_unix(), None);
    }
}
