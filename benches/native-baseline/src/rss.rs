//! Peak resident-set measurement.
//!
//! Rust's standard library exposes no portable peak-RSS accessor, and this
//! workspace forbids `unsafe`, so `getrusage` is off the table. The harness
//! instead delegates to the platform `time(1)` utility and parses its report:
//!
//! * BSD/macOS `/usr/bin/time -l` → `"  532480  maximum resident set size"` (bytes);
//! * GNU `/usr/bin/time -v` → `"Maximum resident set size (kbytes): 520"`.
//!
//! Two rules keep this honest. The probe runs in its **own untimed pass**, never
//! inside a latency sample — the wrapper process would otherwise be measured
//! along with the leg. And when no usable `time` exists (Windows, a stripped
//! container), RSS is reported as *unavailable*: it is never estimated, and never
//! silently reported as zero.

use std::path::{Path, PathBuf};

use crate::{
    hermetic::Hermetic,
    measure::{run_once, Invocation},
};

/// Where the platform `time` utility lives.
const TIME_BIN: &str = "/usr/bin/time";

/// Which `time(1)` dialect is available, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RssProbe {
    /// BSD/macOS `time -l`, reporting bytes.
    Bsd(PathBuf),
    /// GNU `time -v`, reporting kbytes.
    Gnu(PathBuf),
    /// No usable utility — RSS is not measurable on this host.
    Unavailable,
}

impl RssProbe {
    /// Detect the dialect by running the utility over a trivial command.
    ///
    /// Detection is a runtime probe rather than a `cfg!` guess: GNU coreutils'
    /// `time` is frequently absent on Linux images, and being wrong here would
    /// silently produce no RSS numbers at all.
    #[must_use]
    pub fn detect(hermetic: &Hermetic, cwd: &Path) -> Self {
        if !Path::new(TIME_BIN).exists() {
            return Self::Unavailable;
        }
        for (flag, make) in [
            ("-l", (|p| Self::Bsd(p)) as fn(PathBuf) -> Self),
            ("-v", (|p| Self::Gnu(p)) as fn(PathBuf) -> Self),
        ] {
            let inv = Invocation::new(TIME_BIN, cwd).args([flag, "git", "--version"]);
            if let Ok(exec) = run_once(hermetic, &inv) {
                if exec.sample.ok() && parse_peak_rss(&exec.stderr).is_some() {
                    return make(PathBuf::from(TIME_BIN));
                }
            }
        }
        Self::Unavailable
    }

    /// Measure the peak RSS of `inv`, or `None` when unavailable / unparseable.
    #[must_use]
    pub fn peak_rss_bytes(&self, hermetic: &Hermetic, inv: &Invocation) -> Option<u64> {
        let (bin, flag) = match self {
            Self::Bsd(p) => (p, "-l"),
            Self::Gnu(p) => (p, "-v"),
            Self::Unavailable => return None,
        };
        let mut wrapped = Invocation::new(bin, &inv.cwd);
        wrapped.hooks = inv.hooks;
        wrapped.env.clone_from(&inv.env);
        wrapped.stdin.clone_from(&inv.stdin);
        wrapped.args.push(flag.to_owned());
        wrapped.args.push(inv.program.display().to_string());
        wrapped.args.extend(inv.args.iter().cloned());
        let exec = run_once(hermetic, &wrapped).ok()?;
        parse_peak_rss(&exec.stderr)
    }

    /// Human-readable state for the report.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Bsd(p) => format!("{} -l (bytes)", p.display()),
            Self::Gnu(p) => format!("{} -v (kbytes)", p.display()),
            Self::Unavailable => "unavailable".to_owned(),
        }
    }
}

/// Parse a peak RSS in **bytes** out of either dialect's report.
///
/// Returns `None` rather than guessing when no line matches.
#[must_use]
pub fn parse_peak_rss(report: &str) -> Option<u64> {
    for line in report.lines() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("maximum resident set size") {
            continue;
        }
        // GNU: "Maximum resident set size (kbytes): 520"
        if let Some((_, value)) = lower.split_once("(kbytes):") {
            return value.trim().parse::<u64>().ok().map(|kb| kb * 1024);
        }
        // BSD: "  532480  maximum resident set size"
        let first = lower.split_whitespace().next()?;
        if let Ok(bytes) = first.parse::<u64>() {
            return Some(bytes);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const BSD: &str = "        0.01 real         0.00 user         0.00 sys\n\
                              532480  maximum resident set size\n\
                                   0  average shared memory size\n\
                                 390  page reclaims\n";

    const GNU: &str = "\tCommand being timed: \"git --version\"\n\
                       \tUser time (seconds): 0.00\n\
                       \tMaximum resident set size (kbytes): 520\n\
                       \tExit status: 0\n";

    #[test]
    fn parses_both_dialects_into_bytes() {
        assert_eq!(parse_peak_rss(BSD), Some(532_480));
        assert_eq!(parse_peak_rss(GNU), Some(520 * 1024));
    }

    #[test]
    fn unparseable_reports_yield_none_not_zero() {
        assert_eq!(parse_peak_rss(""), None);
        assert_eq!(parse_peak_rss("real 0.01\nuser 0.00\n"), None);
        assert_eq!(parse_peak_rss("maximum resident set size\n"), None);
        assert_eq!(
            parse_peak_rss("Maximum resident set size (kbytes): huge\n"),
            None
        );
    }

    #[test]
    fn an_unavailable_probe_measures_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path()).unwrap();
        let probe = RssProbe::Unavailable;
        let inv = Invocation::new("git", tmp.path()).args(["--version"]);
        assert_eq!(probe.peak_rss_bytes(&hermetic, &inv), None);
        assert_eq!(probe.describe(), "unavailable");
    }

    #[test]
    fn a_detected_probe_reports_a_plausible_peak() {
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path()).unwrap();
        let probe = RssProbe::detect(&hermetic, tmp.path());
        let inv = Invocation::new("git", tmp.path()).args(["--version"]);
        match probe.peak_rss_bytes(&hermetic, &inv) {
            // A real process always occupies more than a page and less than 8 GiB.
            Some(bytes) => assert!(
                (4096..8 << 30).contains(&bytes),
                "implausible peak rss: {bytes}"
            ),
            // Hosts without a usable `time(1)` are a supported, recorded state.
            None => assert_eq!(probe, RssProbe::Unavailable),
        }
    }
}
