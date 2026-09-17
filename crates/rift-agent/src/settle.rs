//! The transcript **flush settle protocol**: the discipline that keeps
//! a turn-end read from condensing a *truncated* transcript.
//!
//! Agents write their session transcript asynchronously, so the hook that fires
//! at a turn boundary can observe the file mid-write. Reading then yields a
//! short transcript whose condensed form is durable and wrong. The protocol
//! decides *when the file may be read*, in this order:
//!
//! 1. **Absent** — nothing to wait for; the caller degrades honestly.
//! 2. **Stale** — untouched for [`STALE_AFTER_MS`]: the writer is gone (a
//!    crashed session), so waiting can only burn the cap.
//! 3. **Sentinel** — the agent's own stop-hook marker record appeared *in bytes
//!    written after this wait began*. Authoritative: return at once. The match
//!    is **structural** ([`StopSentinel`]): a parsed record the adapter itself
//!    recognizes, never a substring of a line — transcripts quote hook command
//!    lines back at us in tool results, attachments and queued prompts, and
//!    accepting one of those would end the wait on a *truncated* transcript.
//! 4. **Quiescent** — the modification time is already older than the quiet
//!    window, which is retroactively the very evidence the window would
//!    gather. Rift-specific; it is what keeps a healthy turn end near-free.
//! 5. **Stable** — the size held steady across a full [`QUIET_WINDOW_MS`] of
//!    wall clock. Any growth resets the window, so a mid-write pause (GC, disk
//!    contention, a large tool result flushed in pieces) is never mistaken for
//!    completion.
//! 6. **Capped** — [`MAX_WAIT_MS`] elapsed; proceed anyway rather than hang.
//!
//! [`Settle`] is a pure state machine — no clock, no filesystem — and
//! [`settle_transcript`] drives it through a [`FlushProbe`]. Tests therefore
//! simulate a slow-writing agent on a virtual clock.
//!
//! **Status (2026-08-29).** No production hook
//! drives this protocol any more. A turn's spend is settled by the *next*
//! hook for the session (or the doctor sweep) from a recorded transcript byte
//! offset, reading only complete lines — so a mid-write read is impossible by
//! construction rather than avoided by waiting, and the `Stop` hook no longer
//! pays the quiet window. The `std::fs` probe that drove this from the hook
//! path was retired with that change. The machine, [`StopSentinel`] and the
//! `a_slow_writer_is_never_read_truncated` property stay:
//! they are the specification an agent-side "transcript is flushed" signal is
//! measured against, and an adapter may still publish a sentinel.

/// How long the transcript must hold still before stability is trusted. It has
/// to comfortably exceed a plausible mid-write pause while still returning well
/// under [`MAX_WAIT_MS`] on a genuinely settled file.
pub const QUIET_WINDOW_MS: u64 = 500;
/// Safety bound on the whole wait: capture proceeds after this, settled or not.
pub const MAX_WAIT_MS: u64 = 3_000;
/// Poll cadence while waiting.
pub const POLL_INTERVAL_MS: u64 = 50;
/// A transcript untouched this long belongs to a session whose agent is gone;
/// waiting on it would cost the full cap for nothing.
pub const STALE_AFTER_MS: u64 = 120_000;
/// Bytes of tail read when scanning for the stop-hook sentinel.
pub const SENTINEL_TAIL_BYTES: u64 = 4096;
/// Hard iteration bound, independent of any clock. Elapsed time alone cannot
/// bound the wait: a wall clock that steps backwards (NTP, a suspend/resume,
/// a hand-set clock) keeps `now − started` small forever, and a hook that
/// never returns would violate the never-block contract. Two polls of slack
/// over the time-based cap keep this from ever being the *normal* ending.
pub const MAX_POLLS: u32 = (MAX_WAIT_MS / POLL_INTERVAL_MS) as u32 + 2;

/// An adapter's **structured** stop-hook sentinel: the test that decides
/// whether one freshly written transcript record is that agent's own "my stop
/// hook ran" marker.
///
/// Structural on purpose. A transcript is a log of everything the session saw,
/// including *quotations* of the hook command line — a `Read` of the settings
/// file that configures the hook, an error attachment reporting the hook's
/// exit, a queued prompt naming it. On this machine's real Claude Code
/// transcripts, 40 such records quote Rift's own Stop command while only the
/// `system`/`stop_hook_summary` records are genuine markers. A substring test
/// accepts all of them, ends the wait early, and condenses a truncated turn —
/// understated spend, chained durably. So the adapter, which alone knows its
/// own record schema, supplies the predicate and the protocol applies it to a
/// **parsed** record.
#[derive(Clone, Copy, Debug)]
pub struct StopSentinel {
    name: &'static str,
    predicate: fn(&serde_json::Value) -> bool,
}

impl StopSentinel {
    /// Define a sentinel. `name` identifies the record shape for logs and
    /// tests (e.g. `"claude-code:stop_hook_summary"`); `predicate` answers
    /// whether one parsed transcript record *is* that marker.
    #[must_use]
    pub const fn new(name: &'static str, predicate: fn(&serde_json::Value) -> bool) -> Self {
        Self { name, predicate }
    }

    /// The record shape this recognizes.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Whether `record` — one parsed transcript line — is this agent's marker.
    #[must_use]
    pub fn matches(self, record: &serde_json::Value) -> bool {
        (self.predicate)(record)
    }
}

/// What one stat of the transcript observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptStat {
    /// File length in bytes.
    pub size: u64,
    /// Modification time in epoch milliseconds.
    pub mtime_ms: u64,
}

/// One poll's evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    /// The observer's clock, in epoch milliseconds (same base as `mtime_ms`).
    pub now_ms: u64,
    /// The transcript's stat, or `None` when it is absent/unreadable.
    pub stat: Option<TranscriptStat>,
    /// Whether a *fresh* stop-hook sentinel is visible. A driver may report
    /// `false` conservatively (e.g. it skipped the tail read).
    pub sentinel: bool,
}

/// Why the wait ended. Every variant is a normal, honest ending — the protocol
/// never errors, because a hook must never fail the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleOutcome {
    /// The transcript is absent or unreadable.
    Absent,
    /// Untouched for [`STALE_AFTER_MS`]; the writer is gone.
    Stale,
    /// The agent's stop-hook sentinel was written during the wait.
    Sentinel,
    /// Already unmodified for a full quiet window when the wait began.
    Quiescent,
    /// The size held steady through a quiet window.
    Stable,
    /// [`MAX_WAIT_MS`] elapsed with the file still moving.
    Capped,
}

impl SettleOutcome {
    /// Stable token for logs and capture facts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Stale => "stale",
            Self::Sentinel => "sentinel",
            Self::Quiescent => "quiescent",
            Self::Stable => "stable",
            Self::Capped => "capped",
        }
    }

    /// Whether the transcript is believed complete. `Capped` is the one ending
    /// that proceeds *without* evidence — callers may record lower confidence.
    #[must_use]
    pub const fn is_settled(self) -> bool {
        !matches!(self, Self::Capped)
    }
}

/// The next thing the driver should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleStep {
    /// Stop waiting.
    Done(SettleOutcome),
    /// Sleep this long, then observe again.
    Poll {
        /// Milliseconds to sleep before the next observation.
        sleep_ms: u64,
    },
}

/// The pure settle state machine: `(state, observation) → step`.
#[derive(Clone, Copy, Debug)]
pub struct Settle {
    started_ms: u64,
    last_size: Option<u64>,
    stable_since_ms: u64,
    polls: u32,
}

impl Settle {
    /// Start a wait at `started_ms`.
    #[must_use]
    pub const fn new(started_ms: u64) -> Self {
        Self {
            started_ms,
            last_size: None,
            stable_since_ms: started_ms,
            polls: 0,
        }
    }

    /// Fold one observation in and decide. Total: never panics, always either
    /// finishes or asks for a bounded sleep, and always finishes by the cap.
    pub fn observe(&mut self, obs: Observation) -> SettleStep {
        let Some(stat) = obs.stat else {
            return SettleStep::Done(SettleOutcome::Absent);
        };
        // A clock that runs behind the filesystem (skew, a file stamped in the
        // future) saturates to age 0 — never a spurious "quiet" verdict.
        let age_ms = obs.now_ms.saturating_sub(stat.mtime_ms);
        if age_ms >= STALE_AFTER_MS {
            return SettleStep::Done(SettleOutcome::Stale);
        }
        if obs.sentinel {
            return SettleStep::Done(SettleOutcome::Sentinel);
        }
        if age_ms >= QUIET_WINDOW_MS && mtime_is_precise(stat.mtime_ms) {
            return SettleStep::Done(SettleOutcome::Quiescent);
        }
        match self.last_size {
            Some(previous) if previous == stat.size => {
                if obs.now_ms.saturating_sub(self.stable_since_ms) >= QUIET_WINDOW_MS {
                    return SettleStep::Done(SettleOutcome::Stable);
                }
            }
            _ => {
                self.last_size = Some(stat.size);
                self.stable_since_ms = obs.now_ms;
            }
        }
        let remaining_ms = MAX_WAIT_MS.saturating_sub(obs.now_ms.saturating_sub(self.started_ms));
        if remaining_ms == 0 || self.polls >= MAX_POLLS {
            return SettleStep::Done(SettleOutcome::Capped);
        }
        self.polls += 1;
        SettleStep::Poll {
            sleep_ms: POLL_INTERVAL_MS.min(remaining_ms),
        }
    }
}

/// Whether a modification time carries sub-second precision, and so can be
/// trusted to date the *last write* rather than the second it fell in.
///
/// The quiescent fast path infers "no write for a quiet window" from the mtime
/// alone. On a filesystem that records whole seconds a write at `t = 1.9 s` is
/// stamped `1.0 s`, which would overstate the age by up to a second and could
/// declare a still-moving transcript settled. Sub-second precision in the value
/// is evidence the filesystem keeps it; a whole-second stamp is ambiguous, so
/// the fast path stands down and the quiet window is measured for real. The
/// cost is a rare (~1-in-1000) slow path on a precise filesystem, in the safe
/// direction.
const fn mtime_is_precise(mtime_ms: u64) -> bool {
    !mtime_ms.is_multiple_of(1000)
}

/// The environment a wait observes: a clock, the transcript's stat, a bounded
/// tail read, and a sleep. Implemented over `std::fs` in production and over a
/// simulated slow-writing agent in tests.
pub trait FlushProbe {
    /// Epoch milliseconds, on the same base as the transcript's mtime.
    fn now_ms(&self) -> u64;

    /// Stat the transcript; `None` if absent or unreadable.
    fn stat(&self) -> Option<TranscriptStat>;

    /// Read up to `cap` trailing bytes, with the absolute offset the returned
    /// slice starts at.
    fn tail(&self, cap: u64) -> Option<(u64, Vec<u8>)>;

    /// Sleep before the next observation.
    fn sleep_ms(&mut self, ms: u64);
}

/// Wait for the transcript to settle, then report why the wait ended.
///
/// `sentinel` is the agent's structured stop-hook marker (see
/// [`AgentAdapter::stop_sentinel`](crate::AgentAdapter::stop_sentinel));
/// `None` disables the fast path for agents that publish no such signal —
/// which is the *normal* case, and the wait then settles on the quiet window.
pub fn settle_transcript<P: FlushProbe + ?Sized>(
    probe: &mut P,
    sentinel: Option<StopSentinel>,
) -> SettleOutcome {
    let mut machine = Settle::new(probe.now_ms());
    // Bytes present when the wait began cannot prove *this* turn ended: a
    // transcript accumulates, so the previous turn's sentinel is still in the
    // tail. Only content appended past this baseline counts.
    let mut baseline: Option<u64> = None;
    loop {
        let now_ms = probe.now_ms();
        let stat = probe.stat();
        let baseline_size = *baseline.get_or_insert_with(|| stat.map_or(0, |s| s.size));
        let seen = probe_sentinel(probe, sentinel, stat, baseline_size);
        match machine.observe(Observation {
            now_ms,
            stat,
            sentinel: seen,
        }) {
            SettleStep::Done(outcome) => return outcome,
            SettleStep::Poll { sleep_ms } => probe.sleep_ms(sleep_ms),
        }
    }
}

/// Read the tail only when the file has actually grown past the baseline —
/// otherwise no fresh sentinel can exist and the read would be wasted IO.
fn probe_sentinel<P: FlushProbe + ?Sized>(
    probe: &P,
    sentinel: Option<StopSentinel>,
    stat: Option<TranscriptStat>,
    baseline: u64,
) -> bool {
    let (Some(sentinel), Some(stat)) = (sentinel, stat) else {
        return false;
    };
    if stat.size <= baseline {
        return false;
    }
    let Some((offset, tail)) = probe.tail(SENTINEL_TAIL_BYTES) else {
        return false;
    };
    fresh_sentinel(&tail, offset, sentinel, baseline)
}

/// Whether `tail` (whose first byte sits at absolute offset `tail_start`) holds
/// a **fresh** sentinel: a complete, JSON-parsable line that `sentinel`
/// recognizes as the agent's own stop-hook marker and that begins at or after
/// `baseline`.
///
/// Pure over arbitrary bytes. Two guards, both load-bearing:
///
/// - **Structural.** The line is parsed and handed to the adapter's predicate.
///   A record that merely quotes the hook command — a tool result reading the
///   settings file, a `hook_non_blocking_error` attachment, a queued prompt —
///   is not the marker and must not end the wait.
/// - **Positional.** Freshness is offset-based rather than timestamp-based: an
///   append-only transcript can only have grown past the baseline since the
///   wait began, so a match there is unambiguously this turn's — no clock skew,
///   no date parsing, and no chance of accepting a previous turn's sentinel
///   because two turns fell close together.
///
/// Requiring a newline-terminated, parsable line also means a marker caught
/// mid-write does not count as flushed.
#[must_use]
pub fn fresh_sentinel(tail: &[u8], tail_start: u64, sentinel: StopSentinel, baseline: u64) -> bool {
    let mut line_start = 0usize;
    for (i, byte) in tail.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let start = line_start;
        line_start = i + 1;
        // A window cut mid-line leaves a partial first fragment.
        if start == 0 && tail_start > 0 {
            continue;
        }
        if tail_start + start as u64 >= baseline
            && tail
                .get(start..i)
                .and_then(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
                .is_some_and(|record| sentinel.matches(&record))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic epoch-millisecond clock start, deliberately *not* on a
    /// second boundary (see [`mtime_is_precise`]).
    const START_MS: u64 = 1_754_000_000_123;
    /// The same instant a second earlier, truncated to a whole second — what a
    /// coarse filesystem would report.
    const COARSE_MTIME_MS: u64 = START_MS - (START_MS % 1000) - 1_000;

    /// A deterministic stand-in for an agent flushing its transcript in chunks:
    /// a virtual clock, scheduled appends, and a real byte buffer to read tails
    /// from. Time only moves when the driver sleeps.
    struct SlowWriter {
        now_ms: u64,
        content: Vec<u8>,
        /// `(at_ms, bytes)`, ascending.
        pending: Vec<(u64, Vec<u8>)>,
        cursor: usize,
        mtime_ms: u64,
        exists: bool,
        sleeps: u32,
    }

    impl SlowWriter {
        fn new(initial: &[u8], mtime_ms: u64) -> Self {
            Self {
                now_ms: START_MS,
                content: initial.to_vec(),
                pending: Vec::new(),
                cursor: 0,
                mtime_ms,
                exists: true,
                sleeps: 0,
            }
        }

        fn with_writes(mut self, writes: Vec<(u64, Vec<u8>)>) -> Self {
            self.pending = writes;
            self
        }

        fn apply_due_writes(&mut self) {
            while let Some((at_ms, bytes)) = self.pending.get(self.cursor) {
                if *at_ms > self.now_ms {
                    break;
                }
                self.content.extend_from_slice(bytes);
                self.mtime_ms = *at_ms;
                self.cursor += 1;
            }
        }

        fn elapsed(&self) -> u64 {
            self.now_ms - START_MS
        }
    }

    impl FlushProbe for SlowWriter {
        fn now_ms(&self) -> u64 {
            self.now_ms
        }

        fn stat(&self) -> Option<TranscriptStat> {
            self.exists.then_some(TranscriptStat {
                size: self.content.len() as u64,
                mtime_ms: self.mtime_ms,
            })
        }

        fn tail(&self, cap: u64) -> Option<(u64, Vec<u8>)> {
            let start = self.content.len().saturating_sub(cap as usize);
            Some((start as u64, self.content[start..].to_vec()))
        }

        fn sleep_ms(&mut self, ms: u64) {
            self.now_ms += ms;
            self.sleeps += 1;
            self.apply_due_writes();
        }
    }

    const MARKER: &str = "hooks claude-code Stop";

    /// A stand-in for an adapter's marker, modelled on the record Claude Code
    /// actually writes (`system`/`stop_hook_summary`, hook command lines under
    /// `hookInfos`), so the protocol's own tests need no adapter crate.
    const TEST_SENTINEL: StopSentinel = StopSentinel::new("test:stop_hook_summary", is_test_marker);

    fn is_test_marker(record: &serde_json::Value) -> bool {
        let field = |key: &str| record.get(key).and_then(serde_json::Value::as_str);
        field("type") == Some("system")
            && field("subtype") == Some("stop_hook_summary")
            && record
                .get("hookInfos")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|infos| {
                    infos.iter().any(|info| {
                        info.get("command")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|command| command.contains(MARKER))
                    })
                })
    }

    fn sentinel_line() -> Vec<u8> {
        format!(
            "{{\"type\":\"system\",\"subtype\":\"stop_hook_summary\",\"hookCount\":1,\
             \"hookInfos\":[{{\"command\":\"'/bin/rift' {MARKER}\",\"durationMs\":332}}]}}\n"
        )
        .into_bytes()
    }

    #[test]
    fn an_absent_transcript_never_waits() {
        let mut w = SlowWriter::new(b"", START_MS);
        w.exists = false;
        assert_eq!(settle_transcript(&mut w, None), SettleOutcome::Absent);
        assert_eq!(w.sleeps, 0, "nothing to wait for");
    }

    #[test]
    fn a_stale_transcript_exits_fast_instead_of_burning_the_cap() {
        let mut w = SlowWriter::new(b"old\n", START_MS - STALE_AFTER_MS - 1);
        assert_eq!(settle_transcript(&mut w, None), SettleOutcome::Stale);
        assert_eq!(w.sleeps, 0);
    }

    #[test]
    fn an_already_quiet_transcript_short_circuits_the_window() {
        let mut w = SlowWriter::new(b"done\n", START_MS - QUIET_WINDOW_MS - 1);
        assert_eq!(settle_transcript(&mut w, None), SettleOutcome::Quiescent);
        assert_eq!(w.sleeps, 0, "the window's evidence already exists");
    }

    #[test]
    fn a_whole_second_mtime_is_too_coarse_to_trust_and_falls_back() {
        // Age says 1 s of quiet, but a whole-second stamp may be truncated by a
        // coarse filesystem — so the window is measured for real.
        let mut w = SlowWriter::new(b"done\n", COARSE_MTIME_MS);
        assert_eq!(settle_transcript(&mut w, None), SettleOutcome::Stable);
        assert!(w.elapsed() >= QUIET_WINDOW_MS, "waited the window out");
    }

    #[test]
    fn a_transcript_that_keeps_growing_is_capped_not_hung() {
        let writes = (0..200)
            .map(|i| (START_MS + i * POLL_INTERVAL_MS, b"chunk\n".to_vec()))
            .collect();
        let mut w = SlowWriter::new(b"", START_MS - 1).with_writes(writes);
        assert_eq!(settle_transcript(&mut w, None), SettleOutcome::Capped);
        assert!(w.elapsed() >= MAX_WAIT_MS && w.elapsed() <= MAX_WAIT_MS + POLL_INTERVAL_MS);
    }

    #[test]
    fn a_clock_that_never_advances_still_ends_the_wait() {
        // A wall clock stepping backwards would keep the elapsed check happy
        // forever; the poll budget is what keeps a hook from hanging.
        let mut machine = Settle::new(START_MS);
        let obs = Observation {
            now_ms: START_MS,
            stat: Some(TranscriptStat {
                size: 1,
                mtime_ms: START_MS,
            }),
            sentinel: false,
        };
        let mut polls = 0u32;
        loop {
            match machine.observe(obs) {
                SettleStep::Done(outcome) => {
                    assert_eq!(outcome, SettleOutcome::Capped);
                    assert_eq!(polls, MAX_POLLS);
                    break;
                }
                SettleStep::Poll { sleep_ms } => {
                    assert_eq!(sleep_ms, POLL_INTERVAL_MS);
                    polls += 1;
                    assert!(polls <= MAX_POLLS, "the budget bounds the loop");
                }
            }
        }
    }

    #[test]
    fn a_sentinel_written_during_the_wait_returns_immediately() {
        let mut w = SlowWriter::new(b"line\n", START_MS - 1)
            .with_writes(vec![(START_MS + 100, sentinel_line())]);
        assert_eq!(
            settle_transcript(&mut w, Some(TEST_SENTINEL)),
            SettleOutcome::Sentinel
        );
        assert!(
            w.elapsed() < QUIET_WINDOW_MS,
            "the authoritative signal beats the window"
        );
    }

    #[test]
    fn a_previous_turns_sentinel_is_never_mistaken_for_this_turns() {
        // The marker is already in the file when the wait starts, and the agent
        // keeps writing for a while: trusting it would truncate the turn.
        let mut w = SlowWriter::new(&sentinel_line(), START_MS - 1).with_writes(vec![
            (START_MS + 100, b"more\n".to_vec()),
            (START_MS + 300, b"more\n".to_vec()),
        ]);
        let outcome = settle_transcript(&mut w, Some(TEST_SENTINEL));
        assert_ne!(
            outcome,
            SettleOutcome::Sentinel,
            "stale marker, not a signal"
        );
        assert!(
            w.elapsed() >= 300 + QUIET_WINDOW_MS,
            "waited past the writes"
        );
        assert_eq!(w.content.len(), sentinel_line().len() + 10);
    }

    #[test]
    fn a_sentinel_only_counts_on_a_complete_parsable_line() {
        let line = sentinel_line();
        assert!(fresh_sentinel(&line, 0, TEST_SENTINEL, 0));
        let unterminated = &line[..line.len() - 1];
        assert!(
            !fresh_sentinel(unterminated, 0, TEST_SENTINEL, 0),
            "a line still being written is not a flushed one"
        );
        let not_json = format!("{MARKER}\n").into_bytes();
        assert!(!fresh_sentinel(&not_json, 0, TEST_SENTINEL, 0));
        assert!(
            !fresh_sentinel(&line, 0, TEST_SENTINEL, 1),
            "below the baseline"
        );
        // The window's cut first fragment is never taken for a whole line.
        assert!(!fresh_sentinel(&line, 40, TEST_SENTINEL, 0));
    }

    /// The REJECT that reshaped this seam: a transcript logs everything the
    /// session saw, including *quotations* of the hook command line. Under a
    /// substring predicate each of these ends the wait early and the turn is
    /// condensed truncated. Shapes taken from real Claude Code transcripts.
    #[test]
    fn a_record_that_merely_quotes_the_hook_command_is_not_a_sentinel() {
        let quoted = format!("\\\"'/bin/rift' {MARKER}\\\"");
        for (shape, line) in [
            (
                "user tool_result reading the settings file",
                format!(
                    "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\
                     [{{\"type\":\"tool_result\",\"content\":\"\\\"command\\\": {quoted}\"}}]}}}}"
                ),
            ),
            (
                "assistant tool_use quoting the command",
                format!(
                    "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":\
                     [{{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":\
                     {{\"command\":\"grep {quoted} settings.json\"}}}}]}}}}"
                ),
            ),
            (
                "hook_non_blocking_error attachment",
                format!(
                    "{{\"type\":\"attachment\",\"attachment\":\
                     {{\"type\":\"hook_non_blocking_error\",\"hookEvent\":\"Stop\",\
                     \"command\":\"'/bin/rift' {MARKER}\",\"exitCode\":127,\"durationMs\":24}}}}"
                ),
            ),
            (
                "queued prompt naming the command",
                format!(
                    "{{\"type\":\"queue-operation\",\"operation\":\"enqueue\",\
                     \"content\":\"re-run {quoted} by hand\"}}"
                ),
            ),
            (
                "a summary for a different hook of the same agent",
                "{\"type\":\"system\",\"subtype\":\"stop_hook_summary\",\"hookInfos\":\
                 [{\"command\":\"'/bin/rift' hooks claude-code SubagentStop\"}]}"
                    .to_owned(),
            ),
        ] {
            let bytes = format!("{line}\n").into_bytes();
            assert!(
                serde_json::from_slice::<serde_json::Value>(line.as_bytes()).is_ok(),
                "{shape}: fixture must be a real transcript line"
            );
            assert!(
                !fresh_sentinel(&bytes, 0, TEST_SENTINEL, 0),
                "{shape}: quoting the command is not running it"
            );
        }
    }

    proptest::proptest! {
        /// Total over arbitrary bytes: never panics, never a false positive on
        /// garbage.
        #[test]
        fn fresh_sentinel_is_total(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512)) {
            let _ = fresh_sentinel(&bytes, 0, TEST_SENTINEL, 0);
        }

        /// A slow-writing agent — chunks separated by pauses shorter than the
        /// quiet window — is never read as finished mid-write. The wait always
        /// ends with the whole transcript on disk, having seen a full quiet
        /// window since the last byte, well inside the cap.
        #[test]
        fn a_slow_writer_is_never_read_truncated(
            gaps in proptest::collection::vec(0u64..=300, 1..=6),
            sizes in proptest::collection::vec(1usize..=64, 1..=6),
        ) {
            let mut at_ms = START_MS;
            let writes: Vec<(u64, Vec<u8>)> = gaps
                .iter()
                .zip(sizes.iter().cycle())
                .map(|(gap, size)| {
                    at_ms += gap;
                    (at_ms, vec![b'x'; *size])
                })
                .collect();
            let total: usize = writes.iter().map(|(_, b)| b.len()).sum();
            let last_write_ms = at_ms;

            let mut w = SlowWriter::new(b"", START_MS - 1).with_writes(writes);
            let outcome = settle_transcript(&mut w, Some(TEST_SENTINEL));

            proptest::prop_assert!(
                matches!(outcome, SettleOutcome::Stable | SettleOutcome::Quiescent),
                "a writer that finishes inside the cap settles: {outcome:?}"
            );
            proptest::prop_assert_eq!(
                w.content.len(),
                total,
                "every byte the agent wrote was on disk before the read"
            );
            proptest::prop_assert!(
                w.now_ms.saturating_sub(last_write_ms) >= QUIET_WINDOW_MS,
                "a full quiet window passed since the last write"
            );
            proptest::prop_assert!(
                w.elapsed() <= MAX_WAIT_MS,
                "and it finished inside the safety bound"
            );
        }
    }
}
