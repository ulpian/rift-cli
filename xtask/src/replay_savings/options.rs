//! What one run was asked to do, and the entry point that does it.
//!
//! The entry point is the command's edge: the only place the clock is read and
//! the only place the build's own revision is asked of git. Everything under
//! it takes both as values, so a test can inject them.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use super::artifact::{write_artifact, Provenance};
use super::render::{render_json, render_table};
use super::replay::replay;

/// What one run was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The client project directory holding `<session>.jsonl` files and their
    /// `<session>/subagents/` windows.
    pub transcripts: PathBuf,
    /// The checkout the recorded paths are resolved against, for line counts
    /// and slice sizes.
    pub repo: PathBuf,
    /// Write the machine-readable document instead of the table.
    pub json: bool,
    /// A repository's git directory: when given, each window's path facts
    /// come from the newest commit at or before the window began.
    pub history: Option<PathBuf>,
    /// The refs whose first-parent chains the commits are taken from.
    pub history_refs: Vec<String>,
    /// A directory to write the table and the document into, each with its
    /// provenance.
    pub artifact: Option<PathBuf>,
}

/// Parse this command's arguments.
///
/// `default_repo` is used when `--repo` is absent, so a caller that knows the
/// workspace root does not have to spell it.
///
/// # Errors
/// An unknown argument, a flag with no value, a missing `--transcripts`, or a
/// `--history-ref` with no `--history`.
pub fn parse_args(args: &[String], default_repo: &Path) -> Result<Options, String> {
    const USAGE: &str = "usage: xtask replay-savings --transcripts <dir> [--repo <dir>] [--json] \
                         [--history <git-dir> [--history-ref <ref>]...] [--artifact <dir>]";
    let mut transcripts = None;
    let mut repo = None;
    let mut json = false;
    let mut history = None;
    let mut history_refs = Vec::new();
    let mut artifact = None;
    let mut at = 0usize;
    while let Some(arg) = args.get(at) {
        let value = |name: &str| -> Result<String, String> {
            args.get(at + 1)
                .cloned()
                .ok_or_else(|| format!("replay-savings: {name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--transcripts" => {
                transcripts = Some(PathBuf::from(value("--transcripts")?));
                at += 2;
            }
            "--repo" => {
                repo = Some(PathBuf::from(value("--repo")?));
                at += 2;
            }
            "--json" => {
                json = true;
                at += 1;
            }
            "--history" => {
                history = Some(PathBuf::from(value("--history")?));
                at += 2;
            }
            "--history-ref" => {
                history_refs.push(value("--history-ref")?);
                at += 2;
            }
            "--artifact" => {
                artifact = Some(PathBuf::from(value("--artifact")?));
                at += 2;
            }
            other => {
                return Err(format!(
                    "replay-savings: unknown argument `{other}`\n{USAGE}"
                ))
            }
        }
    }
    if history.is_none() && !history_refs.is_empty() {
        return Err(format!(
            "replay-savings: --history-ref needs --history\n{USAGE}"
        ));
    }
    if history_refs.is_empty() {
        history_refs.push("HEAD".to_owned());
    }
    Ok(Options {
        transcripts: transcripts
            .ok_or_else(|| format!("replay-savings: --transcripts is required\n{USAGE}"))?,
        repo: repo.unwrap_or_else(|| default_repo.to_path_buf()),
        json,
        history,
        history_refs,
        artifact,
    })
}

/// Replay the transcripts and print the report, and write the artifact when
/// one was asked for.
///
/// # Errors
/// If the transcripts directory cannot be walked, the history cannot be read,
/// or the artifact cannot be written.
pub fn run(opts: &Options) -> Result<(), String> {
    let started = unix_now();
    let report = replay(opts)?;
    let finished = unix_now();
    if opts.json {
        println!("{}", render_json(&report));
    } else {
        println!("{}", render_table(&report));
    }
    let Some(dir) = &opts.artifact else {
        return Ok(());
    };
    let provenance = Provenance {
        describe: build_git(&["describe", "--always", "--dirty"]),
        head: build_git(&["rev-parse", "--short", "HEAD"]),
        corpus_files: count_files(&opts.transcripts),
        history_refs: match opts.history {
            Some(_) => opts.history_refs.clone(),
            None => Vec::new(),
        },
        started,
        finished,
    };
    let (table, document) = write_artifact(dir, &report, &provenance)?;
    eprintln!(
        "replay-savings: wrote {} and {}",
        table.display(),
        document.display()
    );
    Ok(())
}

/// How many files sit under `dir`, at any depth.
fn count_files(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .map(|path| if path.is_dir() { count_files(&path) } else { 1 })
        .sum()
}

/// The wall clock, in whole seconds since the Unix epoch.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// One line git prints about the source tree this command was built from, or
/// `unknown` when git cannot say.
fn build_git(args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output();
    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        _ => "unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::parse_args;

    #[test]
    fn arguments_are_parsed_and_checked() {
        let root = Path::new("/root");
        let args: Vec<String> = ["--transcripts", "/t", "--json"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let opts = parse_args(&args, root).unwrap();
        assert!(opts.json);
        assert_eq!(opts.repo, root);
        assert_eq!(opts.history, None);
        assert_eq!(opts.history_refs, ["HEAD"], "the default ref");
        let history: Vec<String> = [
            "--transcripts",
            "/t",
            "--history",
            "/g",
            "--history-ref",
            "main",
            "--history-ref",
            "current",
            "--artifact",
            "/a",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let opts = parse_args(&history, root).unwrap();
        assert_eq!(opts.history.as_deref(), Some(Path::new("/g")));
        assert_eq!(opts.history_refs, ["main", "current"]);
        assert_eq!(opts.artifact.as_deref(), Some(Path::new("/a")));
        let orphan: Vec<String> = ["--transcripts", "/t", "--history-ref", "main"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert!(parse_args(&orphan, root).is_err(), "a ref with no history");
        assert!(parse_args(&[], root).is_err(), "transcripts is required");
        assert!(
            parse_args(&["--nope".to_owned()], root).is_err(),
            "unknown argument"
        );
        assert!(
            parse_args(&["--repo".to_owned()], root).is_err(),
            "a flag with no value"
        );
    }
}
