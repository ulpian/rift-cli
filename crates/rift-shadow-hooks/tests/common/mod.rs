//! The one place the cross-process suites in this directory find the `rift`
//! binary they drive.
//!
//! `CARGO_BIN_EXE_*` names only the bins of *this* package, and the binary
//! under test belongs to `bins/rift`, so these suites once each ran an
//! unconditional nested `cargo build -p rift --bin rift` into
//! `<target>/vogon-e2e`. Under nextest every test is its own process, so one
//! full gate spawned seven of those cargo processes at once, all contending on
//! one target lock (observed 2026-10-17). The build stays — a suite that
//! merely *locates* a binary another package owns can drive a stale one and
//! stay green over a planted defect, which is how this suite's first draft
//! failed — but it is now the last resort, and it runs once per run.
//!
//! Resolution order, [`resolve_rift_bin`]:
//!
//! 1. `RIFT_E2E_BIN`, when set and naming a file. The gate scripts build the
//!    binary once from the tree under test and export this, so a whole gate
//!    builds the e2e binary exactly once. The exporter owns the freshness
//!    promise; this module cannot check it and does not pretend to.
//! 2. `CARGO_BIN_EXE_rift`, when the compiling package can see it — only
//!    `bins/rift`'s own tests can, so inside this crate the step is a
//!    compile-time `None`; it is kept so the order is one helper's, not two.
//! 3. The nested build into `<target>/vogon-e2e`, under an exclusive file lock
//!    so concurrent test processes build once and the rest wait and reuse the
//!    binary that run stamped (`NEXTEST_RUN_ID` keys the stamp; without it,
//!    every process builds, which is a cargo no-op after the first).

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The environment variable a gate exports after building the binary once.
pub(crate) const RIFT_E2E_BIN: &str = "RIFT_E2E_BIN";

/// Where a resolved binary came from, so a test can assert the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinarySource {
    /// `RIFT_E2E_BIN` named an existing file.
    Planted,
    /// Cargo exported `CARGO_BIN_EXE_rift` to the compiling package.
    OwnPackage,
    /// The nested, lock-guarded `cargo build` ran (or its stamped result was reused).
    Built,
}

/// The `rift` binary this suite drives, resolved once per process.
pub(crate) fn rift_bin() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let (exe, _) = resolve_rift_bin(
            &|key| std::env::var_os(key),
            option_env!("CARGO_BIN_EXE_rift"),
            &mut build_into_own_target,
        );
        exe
    })
    .clone()
}

/// The resolution order with its inputs injected: `env` stands for the process
/// environment, `own_package` for the compile-time `CARGO_BIN_EXE_rift`, and
/// `build` for the nested cargo build, so a test can prove which step answered
/// and that the build step was never reached.
pub(crate) fn resolve_rift_bin(
    env: &dyn Fn(&str) -> Option<OsString>,
    own_package: Option<&str>,
    build: &mut dyn FnMut() -> PathBuf,
) -> (PathBuf, BinarySource) {
    if let Some(planted) = env(RIFT_E2E_BIN).map(PathBuf::from) {
        if planted.is_file() {
            return (planted, BinarySource::Planted);
        }
    }
    if let Some(own) = own_package.map(PathBuf::from) {
        if own.is_file() {
            return (own, BinarySource::OwnPackage);
        }
    }
    (build(), BinarySource::Built)
}

/// `<target>/vogon-e2e` and the profile name, derived from where cargo put
/// `vogon` (`<target>/<profile>/vogon`) so a redirected `CARGO_TARGET_DIR` is
/// followed instead of guessing at the repository layout.
fn nested_target() -> (PathBuf, String) {
    let vogon = PathBuf::from(env!("CARGO_BIN_EXE_vogon"));
    let profile_dir = vogon.parent().expect("vogon has a parent dir");
    let profile = profile_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("debug")
        .to_owned();
    let target = profile_dir
        .parent()
        .expect("a target dir above the profile dir")
        .join("vogon-e2e");
    (target, profile)
}

/// The last resort: build `rift` into its own target directory, one process at
/// a time.
///
/// A separate directory is the second half of the staleness argument, not an
/// optimization: building into the shared `target/` was measured to recompile
/// `rift-core`, `rift-agent`, `rift-recall-types` and `rift-hashchain` during
/// the workspace test run, because the gate builds `--all-features` and a
/// package-scoped build does not, so the two fight over the same fingerprints.
///
/// The exclusive lock makes concurrent test processes serialize on one build.
/// Once the holder has built, it stamps the directory with this run's id; a
/// waiter that finds its own run's stamp beside the binary reuses it without
/// spawning cargo at all. The stamp is only trusted for the run that wrote it,
/// so a later run — possibly from changed source — always rebuilds.
fn build_into_own_target() -> PathBuf {
    let (target, profile) = nested_target();
    std::fs::create_dir_all(&target).expect("create the nested target dir");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(target.join("build.lock"))
        .expect("open the nested build lock");
    lock.lock().expect("take the nested build lock");
    let exe = target
        .join(&profile)
        .join(format!("rift{}", std::env::consts::EXE_SUFFIX));
    let stamp = target.join("built-for-run");
    let run_id = std::env::var_os("NEXTEST_RUN_ID");
    if stamped_by_this_run(&stamp, run_id.as_deref()) && exe.is_file() {
        return exe;
    }
    run_nested_build(&target, &profile);
    assert!(
        exe.is_file(),
        "the rift binary is not at {} after building it",
        exe.display()
    );
    if let Some(run_id) = run_id {
        std::fs::write(&stamp, run_id.as_encoded_bytes()).expect("write the build stamp");
    }
    drop(lock);
    exe
}

/// Whether `stamp` records this very run; never true without a run id, so a
/// plain `cargo test` keeps building (a no-op after the first).
fn stamped_by_this_run(stamp: &Path, run_id: Option<&std::ffi::OsStr>) -> bool {
    let Some(run_id) = run_id else {
        return false;
    };
    match std::fs::read(stamp) {
        Ok(content) => content == run_id.as_encoded_bytes(),
        Err(_) => false,
    }
}

/// `cargo build -p rift --bin rift` into `target`, in `profile`, without debug
/// info or incremental state: this directory only ever produces a subprocess
/// executable, never an `llvm-cov` input or a step-debugging target, and the
/// debug-info variant cost 10–15 GB per checkout (the 2026-10 hygiene audit).
fn run_nested_build(target: &Path, profile: &str) {
    let mut build = Command::new(env!("CARGO"));
    build.args(["build", "-p", "rift", "--bin", "rift"]);
    if profile == "release" {
        build.arg("--release");
    }
    let status = build
        .arg("--target-dir")
        .arg(target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("CARGO_PROFILE_DEV_STRIP", "debuginfo")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .expect("spawn cargo to build the binary under test");
    assert!(status.success(), "cargo build -p rift --bin rift failed");
}
