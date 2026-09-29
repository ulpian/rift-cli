//! The repository as it stood when each window began.
//!
//! A replay against today's worktree has nothing to say about a file that was
//! deleted or split since the session read it, and on a corpus months old that
//! is most of the content pool. Given the repository's history, each window's
//! path facts come from the newest commit at or before the window's first
//! event instead. Git is reached through its plumbing only, argv and no shell:
//! one `log` per ref to list the commits, and one long-lived
//! `git cat-file --batch` for every blob.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// What a commit's tree holds at a path.
pub(super) enum Blob {
    /// The file's bytes.
    Present(Vec<u8>),
    /// Nothing at that path, or something other than a file.
    Absent,
}

/// The commits of the given refs' first-parent chains, and the one process
/// every blob is read through.
pub(super) struct History {
    /// Every commit, by committer time and then id, oldest first.
    commits: Vec<(i64, String)>,
    /// The batch reader's input.
    stdin: ChildStdin,
    /// The batch reader's output.
    stdout: BufReader<ChildStdout>,
    /// The batch reader itself, waited for on drop.
    child: Child,
    /// The first failure talking to the batch reader, which ends the run.
    failure: Option<String>,
}

impl History {
    /// List the commits of every ref's first-parent chain and start the batch
    /// reader.
    ///
    /// # Errors
    /// If git cannot be run, a ref does not resolve, or a line of its log
    /// cannot be read.
    pub(super) fn open(git_dir: &Path, refs: &[String]) -> Result<Self, String> {
        let mut commits = Vec::new();
        for reference in refs {
            commits.extend(first_parent_chain(git_dir, reference)?);
        }
        commits.sort();
        commits.dedup();
        let mut child = Command::new("git")
            .arg("--git-dir")
            .arg(git_dir)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("replay-savings: start git cat-file: {e}"))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("replay-savings: git cat-file has no pipes".to_owned());
        };
        Ok(Self {
            commits,
            stdin,
            stdout: BufReader::new(stdout),
            child,
            failure: None,
        })
    }

    /// The newest commit whose committer time is at or before `at`, as an
    /// index this history reads blobs by.
    pub(super) fn commit_at(&self, at: i64) -> Option<usize> {
        self.commits
            .partition_point(|(time, _)| *time <= at)
            .checked_sub(1)
    }

    /// What commit `commit`'s tree holds at `path`.
    ///
    /// A path git would read relative to a working directory, or one that
    /// cannot be written on one line, is absent rather than guessed at.
    pub(super) fn blob(&mut self, commit: usize, path: &str) -> Blob {
        let Some((_, id)) = self.commits.get(commit) else {
            return Blob::Absent;
        };
        let plain = path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
        if !plain || path.contains('\n') || self.failure.is_some() {
            return Blob::Absent;
        }
        let request = format!("{id}:{path}\n");
        match self.exchange(&request) {
            Ok(blob) => blob,
            Err(err) => {
                self.failure = Some(err);
                Blob::Absent
            }
        }
    }

    /// The first failure talking to the batch reader, if there was one.
    pub(super) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// One request and its answer, in the batch protocol: a header naming the
    /// object's type and size, that many bytes, and a newline.
    fn exchange(&mut self, request: &str) -> Result<Blob, String> {
        let broken = |e: std::io::Error| format!("replay-savings: git cat-file: {e}");
        self.stdin.write_all(request.as_bytes()).map_err(broken)?;
        self.stdin.flush().map_err(broken)?;
        let mut header = String::new();
        self.stdout.read_line(&mut header).map_err(broken)?;
        let header = header.trim_end();
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            return Ok(Blob::Absent);
        }
        let fields: Vec<&str> = header.split(' ').collect();
        let [_, kind, size] = fields.as_slice() else {
            return Err("replay-savings: git cat-file: a header of an unknown shape".to_owned());
        };
        let size: usize = size
            .parse()
            .map_err(|_| "replay-savings: git cat-file: a header with no size".to_owned())?;
        let mut body = vec![0_u8; size.saturating_add(1)];
        self.stdout.read_exact(&mut body).map_err(broken)?;
        body.truncate(size);
        if *kind != "blob" {
            return Ok(Blob::Absent);
        }
        Ok(Blob::Present(body))
    }
}

impl Drop for History {
    fn drop(&mut self) {
        // The batch reader is this run's own child: ending it and reaping it
        // leaves nothing behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Every commit of one ref's first-parent chain, with its committer time.
fn first_parent_chain(git_dir: &Path, reference: &str) -> Result<Vec<(i64, String)>, String> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["log", "--first-parent", "--format=%ct %H", reference, "--"])
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("replay-savings: run git log: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "replay-savings: --history-ref `{reference}` does not resolve"
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in text.lines() {
        let parsed = line
            .split_once(' ')
            .and_then(|(time, id)| Some((time.parse::<i64>().ok()?, id.to_owned())));
        let Some(commit) = parsed else {
            return Err("replay-savings: git log printed a line with no time".to_owned());
        };
        out.push(commit);
    }
    Ok(out)
}

/// The Unix time a transcript stamp names, to the second.
///
/// Only the form the client writes is read — `YYYY-MM-DDTHH:MM:SS`, a
/// fraction or not, in UTC. Anything else is no time at all rather than a
/// guess, and a window with no time has no commit.
pub(super) fn unix_seconds(stamp: &str) -> Option<i64> {
    let number = |from: usize, to: usize| stamp.get(from..to)?.parse::<i64>().ok();
    let separators = [(4, '-'), (7, '-'), (10, 'T'), (13, ':'), (16, ':')];
    let shaped = separators
        .iter()
        .all(|(at, want)| stamp.get(*at..).and_then(|rest| rest.chars().next()) == Some(*want));
    let utc = stamp.ends_with('Z');
    if !shaped || !utc {
        return None;
    }
    let days = days_from_civil(number(0, 4)?, number(5, 7)?, number(8, 10)?);
    Some(days * 86_400 + number(11, 13)? * 3_600 + number(14, 16)? * 60 + number(17, 19)?)
}

/// A Unix time as a UTC stamp, spelled the way the client spells one.
pub(super) fn utc_stamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let era = days.saturating_add(719_468).div_euclid(146_097);
    let day_of_era = days.saturating_add(719_468) - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::{unix_seconds, utc_stamp};

    #[test]
    fn a_transcript_stamp_is_read_to_the_second_and_written_back() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            unix_seconds("2026-09-16T22:22:01.373Z"),
            Some(1_789_597_321)
        );
        assert_eq!(
            unix_seconds("2026-09-16 22:22:01"),
            None,
            "not the client's form"
        );
        assert_eq!(unix_seconds("2026-09-16T22:22:01+02:00"), None, "not UTC");
        assert_eq!(unix_seconds(""), None);
        assert_eq!(utc_stamp(1_789_597_321), "2026-09-16T22:22:01Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
    }
}
