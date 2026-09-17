//! Was the machine quiet? — the attestation, mechanized.
//!
//! `benchmarks/hook-latency.md` attests idleness in **prose**: *"one machine,
//! one session, idle — load1 1.65 at run 2's arms"*, *"nothing else running,
//! the machine quiet by arrangement"*. Every one of those sentences was typed
//! by the person who also wanted the numbers to be good, and none of them
//! reddens anything. Meanwhile the same document records what a busy box does
//! to its own conclusions: §11's ladder ran at load **12.0–16.7** against
//! §10's **5.7–8.8**, and at the higher load *"every rung's spread overlaps
//! every other rung's, and 32 KB measures faster than 24 KB"* — the corpus
//! disappeared under the machine.
//!
//! So the load is **read per arm, into the artifact**, and a ceiling is stated
//! rather than felt.
//!
//! # The ceiling, and where it comes from
//!
//! [`VOID_ABOVE_LOAD1`] is **8.0**, and it is derived from those two recorded
//! populations rather than chosen: 5.7–8.8 produced a curve this project could
//! read and rule on, 12.0–16.7 produced one it explicitly could not
//! (`hook-latency.md` §11.2). Eight is also this host's logical CPU count,
//! which is the point at which the run stops having a core to itself.
//!
//! Above it a run is **VOID for floor derivation** — not deleted, not hidden,
//! and still published: it is a real measurement of a machine that was busy.
//! What it may not do is set a floor other runs are then judged against.

use serde::{Deserialize, Serialize};

/// Load average above which a run may not derive a floor.
///
/// See the module docs: derived from the two recorded populations in
/// `benchmarks/hook-latency.md`, not chosen.
pub const VOID_ABOVE_LOAD1: f64 = 8.0;

/// What the machine was doing while an arm ran.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    /// One-minute load average when the arm started, or `None` where the
    /// platform did not answer.
    ///
    /// `None` is **unknown**, never quiet: a reading the harness could not take
    /// is not evidence that there was nothing to read.
    pub load1: Option<f64>,
    /// The ceiling in force, carried with the reading so an artifact can be
    /// judged by the rule that was current when it was written.
    pub void_above: f64,
}

impl Attestation {
    /// Take a reading now.
    #[must_use]
    pub fn take() -> Self {
        Self {
            load1: read_load1(),
            void_above: VOID_ABOVE_LOAD1,
        }
    }

    /// Whether this arm may back a derived floor.
    ///
    /// An unknown reading is void too, and deliberately: the whole point is
    /// that "the box was quiet" stops being an assertion, and an absent
    /// measurement is an assertion.
    #[must_use]
    pub fn void(&self) -> bool {
        self.load1.is_none_or(|l| l > self.void_above)
    }

    /// The sentence the report prints beside the arm.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.load1 {
            Some(l) if l > self.void_above => format!(
                "load1 {l:.2} — **VOID for floor derivation** (ceiling {:.1})",
                self.void_above
            ),
            Some(l) => format!("load1 {l:.2}"),
            None => "load1 unknown — VOID: an unread machine is not a quiet one".to_owned(),
        }
    }
}

/// The one-minute load average.
///
/// Read through `uptime`, whose trailing three numbers are the load averages on
/// both platforms this harness runs on (macOS prints `load averages: a b c`,
/// Linux `load average: a, b, c`). Parsing the tail rather than the label is
/// what makes one reader work for both.
#[must_use]
pub fn read_load1() -> Option<f64> {
    let out = std::process::Command::new("uptime")
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    parse_uptime(&String::from_utf8_lossy(&out.stdout))
}

/// Pull the one-minute average out of an `uptime` line.
#[must_use]
pub fn parse_uptime(line: &str) -> Option<f64> {
    let (_, tail) = line.rsplit_once("average")?;
    // `s: 1.65 2.13 2.59` on macOS, `: 1.65, 2.13, 2.59` on Linux.
    tail.trim_start_matches(['s', ':', ' '])
        .split([',', ' '])
        .map(str::trim)
        .find(|t| !t.is_empty())
        .and_then(|t| t.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_platforms_uptime_shapes_parse() {
        let mac = " 7:04  up 24 days, 20:43, 1 user, load averages: 11.47 43.87 52.44";
        let linux = " 07:04:01 up 3 days,  4:11,  2 users,  load average: 0.52, 0.58, 0.59";
        assert_eq!(parse_uptime(mac), Some(11.47));
        assert_eq!(parse_uptime(linux), Some(0.52));
        // A comma decimal separator (some locales) is not a number this parser
        // pretends to understand: unknown, never a wrong reading.
        assert_eq!(parse_uptime("load average: nope"), None);
        assert_eq!(parse_uptime("no such line"), None);
        assert_eq!(parse_uptime(""), None);
    }

    /// The ceiling's two sides, and the third state that is neither.
    #[test]
    fn a_busy_box_and_an_unread_one_are_both_void_and_a_quiet_one_is_not() {
        let quiet = Attestation {
            load1: Some(1.65),
            void_above: VOID_ABOVE_LOAD1,
        };
        assert!(!quiet.void());
        assert_eq!(quiet.describe(), "load1 1.65");

        let busy = Attestation {
            load1: Some(16.7),
            void_above: VOID_ABOVE_LOAD1,
        };
        assert!(busy.void());
        assert!(busy.describe().contains("VOID for floor derivation"));

        // Exactly at the ceiling is admitted: the bound is "above".
        let edge = Attestation {
            load1: Some(VOID_ABOVE_LOAD1),
            void_above: VOID_ABOVE_LOAD1,
        };
        assert!(!edge.void());

        let unread = Attestation {
            load1: None,
            void_above: VOID_ABOVE_LOAD1,
        };
        assert!(unread.void(), "an unread machine is not a quiet one");
        assert!(unread.describe().contains("unknown"));
    }

    /// The reading is taken from the real machine, and the ceiling travels with
    /// it so an artifact can be judged by the rule current when it was written.
    #[test]
    fn a_reading_taken_here_carries_its_own_ceiling() {
        let a = Attestation::take();
        assert!((a.void_above - 8.0).abs() < f64::EPSILON);
        if let Some(load) = a.load1 {
            assert!(load >= 0.0, "a load average is never negative: {load}");
        }
        // Round trip, because it is a field of a durable record.
        let json = serde_json::to_string(&a).unwrap();
        let back: Attestation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }
}
