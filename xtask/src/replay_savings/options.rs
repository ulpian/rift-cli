//! What one run was asked to do, and the entry point that does it.

use std::path::{Path, PathBuf};

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
}

/// Parse this command's arguments.
///
/// `default_repo` is used when `--repo` is absent, so a caller that knows the
/// workspace root does not have to spell it.
///
/// # Errors
/// An unknown argument, a flag with no value, or a missing `--transcripts`.
pub fn parse_args(args: &[String], default_repo: &Path) -> Result<Options, String> {
    const USAGE: &str = "usage: xtask replay-savings --transcripts <dir> [--repo <dir>] [--json]";
    let mut transcripts = None;
    let mut repo = None;
    let mut json = false;
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
            other => {
                return Err(format!(
                    "replay-savings: unknown argument `{other}`\n{USAGE}"
                ))
            }
        }
    }
    Ok(Options {
        transcripts: transcripts
            .ok_or_else(|| format!("replay-savings: --transcripts is required\n{USAGE}"))?,
        repo: repo.unwrap_or_else(|| default_repo.to_path_buf()),
        json,
    })
}

/// Replay the transcripts and print the report.
///
/// # Errors
/// If the transcripts directory cannot be walked.
pub fn run(opts: &Options) -> Result<(), String> {
    let report = replay(opts)?;
    if opts.json {
        println!("{}", render_json(&report));
    } else {
        println!("{}", render_table(&report));
    }
    Ok(())
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
