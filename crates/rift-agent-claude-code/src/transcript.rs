//! Token usage from the Claude Code session transcript, read from offsets.
//!
//! Claude Code's native hook stdin carries **no** `usage` fields; the
//! provider-reported numbers live in the session transcript (JSONL) the hook
//! names via `transcript_path`. The readers here are **pure over bytes** (the
//! caller does the bounded file read):
//!
//! * [`turn_window_since_offset`] walks FORWARD from a recorded byte offset and
//!   sums one closed turn's `message.usage` — up to the next real user prompt
//!   (a `user` line that is not a tool result, not meta, and not a sidechain),
//!   returning the offset the next window starts at. It is what the hook that
//!   fires AFTER a turn ends uses to settle that turn's spend: the
//!   Stop hook never reads its own turn, so there is no settle wait and no
//!   truncated turn. Sidechain (subagent) assistant calls inside the window are
//!   part of the turn's spend and are summed; the reported model is the last
//!   main-chain assistant's.
//! * [`latest_main_context`] reads a bounded TAIL for the freshest main-chain
//!   context figure (the nic-gauge / System panel), on PreToolUse.
//! * [`usage_since_offset`] is the checkpoint-scoped window:
//!   checkpoint spend and session spend are kept distinct by construction.
//!
//! Total over arbitrary bytes: an unparsable line is skipped; no usage found
//! is `None` — never a fabricated zero (the adapter honesty rule). The
//! backward tail reader this module began with (an 8 MiB window) was
//! retired on 2026-08-29 once the deferred settle left it without a caller.

use rift_agent::TokenUsage;

/// One main-chain model context observed at a transcript boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainContextUsage {
    /// Usage from exactly one non-sidechain assistant message.
    pub usage: TokenUsage,
    /// Model named by that assistant message.
    pub model: Option<String>,
}

fn usage_at(value: Option<&serde_json::Value>) -> TokenUsage {
    let Some(value) = value else {
        return TokenUsage::default();
    };
    TokenUsage {
        input: value
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64),
        output: value
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64),
        cache_read: value
            .get("cache_read_input_tokens")
            .and_then(serde_json::Value::as_u64),
        cache_write: value
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_u64),
    }
}

/// Whether a `user` line is a real prompt (the turn boundary): tool-result
/// lines carry a `toolUseResult` key, meta lines set `isMeta`, and sidechain
/// prompts are agent-authored — none of those end the walk.
fn is_real_prompt(v: &serde_json::Value) -> bool {
    let has_tool_result = v
        .as_object()
        .is_some_and(|o| o.contains_key("toolUseResult"));
    let is_meta = v.get("isMeta").and_then(serde_json::Value::as_bool) == Some(true);
    !has_tool_result && !is_meta && !is_sidechain(v)
}

fn is_sidechain(v: &serde_json::Value) -> bool {
    v.get("isSidechain").and_then(serde_json::Value::as_bool) == Some(true)
}

/// Token usage measured across a **transcript offset boundary**.
///
/// The two scopes are kept distinct on purpose, because collapsing them is the
/// defect this exists to prevent. A checkpoint's spend is the transcript bytes written
/// since the previous checkpoint recorded its offset; the session's spend is
/// everything the transcript holds. Reporting one as the other over-attributes
/// a whole session to whichever commit happened to condense it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OffsetUsage {
    /// Spend attributable to this checkpoint: the window after the recorded
    /// offset.
    pub checkpoint: TokenUsage,
    /// Spend over the whole transcript, offset ignored.
    pub session: TokenUsage,
    /// The part of `checkpoint` that came from subagent (`isSidechain`) work.
    ///
    /// It is **already inside** `checkpoint` — folded in, per the item, because
    /// a subagent's tokens are really spent and a checkpoint that omitted them
    /// would under-report. It is reported separately so "folded in" is a
    /// visible number rather than an assumption a reader has to take on trust.
    pub subagent: TokenUsage,
    /// The offset the caller records for the NEXT checkpoint: the length of the
    /// transcript this call measured. Handing it back is what keeps successive
    /// windows adjacent and non-overlapping without the caller doing arithmetic.
    pub next_offset: u64,
}

/// Sum a transcript's usage on both sides of `from_offset`.
///
/// **The window rule, stated because a boundary needs one.** A usage line
/// belongs to the checkpoint window when the line *starts* at or after
/// `from_offset`. An offset is recorded at a line boundary by whoever wrote it,
/// so a mid-line offset means the transcript was truncated or the offset was
/// stale; in that case the partial line belongs to the window before it and is
/// never double-counted into this one.
///
/// Total over arbitrary bytes: an unparsable
/// line is skipped, and a class the provider never reported stays `None` rather
/// than becoming a fabricated zero. The four classes are carried through
/// unchanged — this function re-scopes usage, it never re-derives it.
#[must_use]
pub fn usage_since_offset(transcript: &[u8], from_offset: u64) -> OffsetUsage {
    let mut out = OffsetUsage {
        next_offset: transcript.len() as u64,
        ..OffsetUsage::default()
    };
    let mut at: u64 = 0;
    for line in transcript.split(|b| *b == b'\n') {
        let start = at;
        // +1 for the separator; the final line may not have one, which only
        // matters for `next_offset`, and that is taken from the slice length.
        at = at.saturating_add(line.len() as u64).saturating_add(1);
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(serde_json::Value::as_str) != Some("assistant") {
            continue;
        }
        let usage = usage_at(v.pointer("/message/usage"));
        if !usage.is_present() {
            continue;
        }
        accumulate(&mut out.session, &usage);
        if start >= from_offset {
            accumulate(&mut out.checkpoint, &usage);
            if is_sidechain(&v) {
                accumulate(&mut out.subagent, &usage);
            }
        }
    }
    out
}

impl OffsetUsage {
    /// Fold a subagent transcript's window into this one.
    ///
    /// Claude Code writes sidechain work into the parent transcript, so
    /// [`usage_since_offset`] already folds it in. An agent whose subagents
    /// write their own files needs this instead: the child's spend joins both
    /// `checkpoint` and `subagent`, and `session` too, because it is the same
    /// session's spend by another path.
    ///
    /// `next_offset` is deliberately NOT merged — it belongs to the transcript
    /// it was measured from, and the caller records one per file.
    pub fn fold_subagent(&mut self, child: &Self) {
        accumulate(&mut self.checkpoint, &child.checkpoint);
        accumulate(&mut self.subagent, &child.checkpoint);
        accumulate(&mut self.session, &child.session);
    }
}

/// The bounded tail a **context** sample is read from: one main-chain call's
/// provider-reported usage is the last assistant record, so a quarter-MiB of
/// tail is generous even under heavy tool traffic, and it is all the mid-turn
/// hot path (`PreToolUse`) pays for.
pub const CONTEXT_TAIL_BYTES: u64 = 256 * 1024;

/// One closed turn's spend, read **forward from a byte offset** rather than
/// backward from the tail — the deferred-settle contract: a `Stop` hook
/// records nothing about spend, and the next
/// hook for the session (or the doctor sweep) settles the turn the agent has
/// finished writing by then.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnWindow {
    /// Summed usage across every assistant record in the window, sidechain
    /// (subagent) calls included — they are real spend.
    pub usage: TokenUsage,
    /// The part of `usage` that came from sidechain records, reported so
    /// "folded in" is a number rather than a promise.
    pub subagent: TokenUsage,
    /// The model of the window's last main-chain assistant record.
    pub model: Option<String>,
    /// The freshest main-chain usage sample in the window.
    pub main_context: Option<MainContextUsage>,
    /// Where the next window starts: the start of the prompt that ended this
    /// one, else the end of the last **complete** line. A trailing line with
    /// no newline is never consumed — the agent may still be writing it — so
    /// the next settle reads it whole.
    pub next_offset: u64,
}

/// Read one turn's window from `bytes`, starting at `base_offset`.
///
/// **The window rule.** Records are consumed from the first line that starts
/// at or after `base_offset`. A real prompt (see `is_real_prompt`) that is
/// the window's *first* record is the turn's own prompt and is consumed; any
/// later real prompt is the **next** turn's and ends the window, so a settle
/// is correct at any later hook — prompt-submit, a pre-tool of the next
/// turn, session end, the sweep — not only at the very next one. A window
/// holding a prompt and nothing else (a prompt-only turn) therefore ends at
/// its successor's prompt and settles nothing, rather than swallowing the
/// successor's spend.
///
/// Total over arbitrary bytes: an unparsable
/// line is skipped and a class the provider never reported stays `None`. The
/// four classes are carried through, never re-derived.
#[must_use]
pub fn turn_window_since_offset(bytes: &[u8], base_offset: u64) -> TurnWindow {
    let mut out = TurnWindow::default();
    let mut seen_record = false;
    let mut at: u64 = 0;
    let mut consumed: u64 = base_offset.min(bytes.len() as u64);
    let mut rest = bytes;
    // An unterminated final line ends the walk: it is left for the next window.
    while let Some(nl) = rest.iter().position(|b| *b == b'\n') {
        let (line, tail) = rest.split_at(nl);
        let start = at;
        at = at.saturating_add(line.len() as u64).saturating_add(1);
        rest = tail.get(1..).unwrap_or_default();
        if start < base_offset {
            // A line the offset landed inside started before the window and
            // belongs to the one before it; it is behind us either way.
            consumed = at;
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            consumed = at;
            continue;
        };
        match v.get("type").and_then(serde_json::Value::as_str) {
            Some("user") if is_real_prompt(&v) => {
                if seen_record {
                    // The next turn's prompt: it belongs to the next window.
                    consumed = start;
                    break;
                }
                seen_record = true;
            }
            Some("assistant") => {
                seen_record = true;
                let line_usage = usage_at(v.pointer("/message/usage"));
                if line_usage.is_present() {
                    accumulate(&mut out.usage, &line_usage);
                    if is_sidechain(&v) {
                        accumulate(&mut out.subagent, &line_usage);
                    }
                }
                if !is_sidechain(&v) {
                    let line_model = v
                        .pointer("/message/model")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    if line_model.is_some() {
                        out.model.clone_from(&line_model);
                    }
                    if line_usage.is_present() {
                        out.main_context = Some(MainContextUsage {
                            usage: line_usage,
                            model: line_model,
                        });
                    }
                }
            }
            _ => {}
        }
        consumed = at;
    }
    out.next_offset = consumed;
    out
}

/// The freshest main-chain context sample in a transcript tail: the last
/// non-sidechain assistant record carrying usage. Walks backward and stops at
/// the first such record, so the cost is one record's parse in the common
/// case.
#[must_use]
pub fn latest_main_context(tail: &[u8]) -> Option<MainContextUsage> {
    for line in tail.split(|b| *b == b'\n').rev() {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(serde_json::Value::as_str) != Some("assistant")
            || is_sidechain(&v)
        {
            continue;
        }
        let usage = usage_at(v.pointer("/message/usage"));
        if usage.is_present() {
            return Some(MainContextUsage {
                usage,
                model: v
                    .pointer("/message/model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            });
        }
    }
    None
}

/// Add every reported class of `from` into `into`, saturating, leaving a class
/// neither side reported as `None`.
fn accumulate(into: &mut TokenUsage, from: &TokenUsage) {
    for (acc, add) in [
        (&mut into.input, from.input),
        (&mut into.output, from.output),
        (&mut into.cache_read, from.cache_read),
        (&mut into.cache_write, from.cache_write),
    ] {
        if let Some(n) = add {
            *acc = Some(acc.unwrap_or(0).saturating_add(n));
        }
    }
}

#[cfg(test)]
mod offset_tests {
    use super::*;

    /// A fixture transcript: a prompt, two main-chain assistant turns, one
    /// sidechain (subagent) assistant call, and an unparsable line.
    fn fixture() -> (Vec<u8>, u64) {
        fn push(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(s.as_bytes());
            out.push(b'\n');
        }
        let mut out = Vec::new();
        push(
            &mut out,
            &serde_json::json!({"type": "user", "message": {"content": "one"}}).to_string(),
        );
        push(&mut out, &assistant_line(10, 1, false));
        let boundary = out.len() as u64;
        push(&mut out, "{ not json");
        push(
            &mut out,
            &serde_json::json!({"type": "user", "message": {"content": "two"}}).to_string(),
        );
        push(&mut out, &assistant_line(200, 20, false));
        push(&mut out, &assistant_line(3000, 300, true));
        (out, boundary)
    }

    fn assistant_line(input: u64, output: u64, sidechain: bool) -> String {
        serde_json::json!({
            "type": "assistant",
            "isSidechain": sidechain,
            "message": {
                "model": "claude-x",
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_input_tokens": input,
                }
            }
        })
        .to_string()
    }

    /// The whole point of item 6: the checkpoint's window and the session's
    /// total are different numbers, and the boundary decides which.
    #[test]
    fn the_checkpoint_window_and_the_session_total_are_kept_distinct() {
        let (transcript, boundary) = fixture();
        let usage = usage_since_offset(&transcript, boundary);

        // After the boundary: 200 + 3000 in, 20 + 300 out.
        assert_eq!(usage.checkpoint.input, Some(3200));
        assert_eq!(usage.checkpoint.output, Some(320));
        // The whole transcript also carries the first turn's 10 / 1.
        assert_eq!(usage.session.input, Some(3210));
        assert_eq!(usage.session.output, Some(321));
        assert_ne!(
            usage.checkpoint, usage.session,
            "collapsing the two scopes is the defect item 6 names"
        );
        assert_eq!(usage.next_offset, transcript.len() as u64);
    }

    /// Subagent spend is REAL spend, so it is inside the checkpoint — and
    /// visible on its own, so "folded in" is a number rather than a promise.
    #[test]
    fn subagent_spend_is_folded_in_and_still_reported_separately() {
        let (transcript, boundary) = fixture();
        let usage = usage_since_offset(&transcript, boundary);
        assert_eq!(usage.subagent.input, Some(3000));
        assert_eq!(usage.subagent.output, Some(300));
        assert!(
            usage.subagent.input.unwrap() < usage.checkpoint.input.unwrap(),
            "the subagent subtotal must sit INSIDE the checkpoint, not beside it"
        );
    }

    /// The four classes are carried through, never re-derived — and a class the
    /// provider never reported stays absent instead of becoming a zero.
    #[test]
    fn the_four_class_mapping_survives_and_absence_stays_absence() {
        let (transcript, boundary) = fixture();
        let usage = usage_since_offset(&transcript, boundary);
        assert_eq!(usage.checkpoint.cache_read, Some(3200));
        assert_eq!(
            usage.checkpoint.cache_write, None,
            "no line reported cache creation; a fabricated zero would be a lie"
        );
    }

    /// Successive windows tile the transcript exactly: feeding one call's
    /// `next_offset` to the next leaves nothing counted twice and nothing lost.
    #[test]
    fn successive_windows_tile_the_transcript_without_overlap() {
        let (transcript, boundary) = fixture();
        let first = usage_since_offset(&transcript[..boundary as usize], 0);
        let second = usage_since_offset(&transcript, first.next_offset);
        assert_eq!(first.next_offset, boundary);
        assert_eq!(first.checkpoint.input, Some(10));
        assert_eq!(second.checkpoint.input, Some(3200));
        assert_eq!(
            first.checkpoint.input.unwrap() + second.checkpoint.input.unwrap(),
            second.session.input.unwrap(),
            "the windows must sum to the session, or a checkpoint is losing spend"
        );
    }

    /// An offset past the end, or one landing mid-line, never double-counts.
    #[test]
    fn a_stale_or_mid_line_offset_counts_nothing_twice() {
        let (transcript, _) = fixture();
        let past = usage_since_offset(&transcript, transcript.len() as u64 + 999);
        assert_eq!(past.checkpoint, TokenUsage::default(), "{past:?}");
        assert_eq!(
            past.session.input,
            Some(3210),
            "the session total does not depend on the offset"
        );

        // Mid-line: the line the offset lands inside started before it, so it
        // belongs to the window BEFORE this one.
        let last_line_start =
            transcript.len() as u64 - assistant_line(3000, 300, true).len() as u64 - 1;
        let mid = usage_since_offset(&transcript, last_line_start + 5);
        assert_eq!(mid.checkpoint, TokenUsage::default());
    }

    /// A subagent that writes its OWN transcript folds in through the same two
    /// scopes, and `next_offset` is per-file so it is not merged.
    #[test]
    fn a_separate_subagent_transcript_folds_into_both_scopes() {
        let (transcript, boundary) = fixture();
        let mut parent = usage_since_offset(&transcript[..boundary as usize], 0);
        let before = parent.next_offset;
        let child = usage_since_offset(format!("{}\n", assistant_line(7, 5, false)).as_bytes(), 0);
        parent.fold_subagent(&child);
        assert_eq!(parent.checkpoint.input, Some(17));
        assert_eq!(parent.subagent.input, Some(7));
        assert_eq!(parent.session.input, Some(17));
        assert_eq!(
            parent.next_offset, before,
            "next_offset belongs to the transcript it was measured from"
        );
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;

    fn prompt(text: &str) -> String {
        serde_json::json!({"type": "user", "message": {"content": text}}).to_string()
    }

    fn tool_result() -> String {
        serde_json::json!({"type": "user", "toolUseResult": {"stdout": "ok"}, "message": {}})
            .to_string()
    }

    fn assistant(model: &str, input: u64, output: u64, sidechain: bool) -> String {
        serde_json::json!({
            "type": "assistant",
            "isSidechain": sidechain,
            "message": {
                "model": model,
                "usage": {"input_tokens": input, "output_tokens": output, "cache_read_input_tokens": 5}
            }
        })
        .to_string()
    }

    fn joined(lines: &[String]) -> Vec<u8> {
        let mut out = Vec::new();
        for l in lines {
            out.extend_from_slice(l.as_bytes());
            out.push(b'\n');
        }
        out
    }

    /// The deferred-settle contract, end to end over bytes: turn 1's window
    /// ends at turn 2's prompt, turn 2's window starts exactly there, and the
    /// two tile the transcript — nothing counted twice, nothing lost.
    #[test]
    fn a_window_ends_at_the_next_prompt_and_successive_windows_tile() {
        let turn_one = [
            prompt("one"),
            assistant("m-old", 10, 1, false),
            tool_result(),
            assistant("m-side", 100, 10, true),
            assistant("m-new", 20, 2, false),
        ];
        let turn_two = [prompt("two"), assistant("m-two", 1000, 100, false)];
        let mut bytes = joined(&turn_one);
        let boundary = bytes.len() as u64;
        bytes.extend(joined(&turn_two));

        let first = turn_window_since_offset(&bytes, 0);
        assert_eq!(first.usage.input, Some(130));
        assert_eq!(first.usage.output, Some(13));
        assert_eq!(first.usage.cache_read, Some(15));
        assert_eq!(first.usage.cache_write, None, "never a fabricated zero");
        assert_eq!(first.subagent.input, Some(100));
        assert_eq!(
            first.model.as_deref(),
            Some("m-new"),
            "last main-chain model"
        );
        let ctx = first.main_context.as_ref().expect("a main-chain sample");
        assert_eq!(ctx.usage.input, Some(20), "the freshest main-chain sample");
        assert_eq!(
            first.next_offset, boundary,
            "the window ends where the next prompt starts"
        );

        let second = turn_window_since_offset(&bytes, first.next_offset);
        assert_eq!(second.usage.input, Some(1000));
        assert_eq!(second.next_offset, bytes.len() as u64);
        let whole = usage_since_offset(&bytes, 0).session;
        assert_eq!(
            first.usage.input.unwrap() + second.usage.input.unwrap(),
            whole.input.unwrap(),
            "the windows sum to the session total"
        );
    }

    /// A prompt at the head of a window is the turn's own and does not end
    /// it; a prompt-only turn settles nothing and hands its successor's
    /// prompt to the next window instead of swallowing the successor.
    #[test]
    fn a_leading_prompt_does_not_end_an_empty_window() {
        let bytes = joined(&[prompt("one"), assistant("m", 7, 1, false)]);
        let w = turn_window_since_offset(&bytes, 0);
        assert_eq!(w.usage.input, Some(7));
        assert_eq!(w.next_offset, bytes.len() as u64);

        let only = joined(&[prompt("one")]);
        let boundary = only.len() as u64;
        let mut two = only.clone();
        two.extend(joined(&[prompt("two"), assistant("m", 9, 1, false)]));
        let w = turn_window_since_offset(&two, 0);
        assert!(!w.usage.is_present(), "a prompt-only turn has no spend");
        assert_eq!(
            w.next_offset, boundary,
            "its successor's prompt is left alone"
        );
    }

    /// The agent may still be writing the last line: it is never consumed,
    /// so the next window reads it whole, and a settle taken mid-write can
    /// never durably record a truncated record.
    #[test]
    fn an_unterminated_final_line_is_left_for_the_next_window() {
        let mut bytes = joined(&[prompt("one"), assistant("m", 3, 1, false)]);
        let complete = bytes.len() as u64;
        let partial = assistant("m", 4000, 400, false);
        bytes.extend_from_slice(&partial.as_bytes()[..partial.len() / 2]);
        let w = turn_window_since_offset(&bytes, 0);
        assert_eq!(
            w.usage.input,
            Some(3),
            "the half-written record is not counted"
        );
        assert_eq!(w.next_offset, complete);

        bytes.truncate(complete as usize);
        bytes.extend_from_slice(partial.as_bytes());
        bytes.push(b'\n');
        let w = turn_window_since_offset(&bytes, complete);
        assert_eq!(
            w.usage.input,
            Some(4000),
            "…and is read whole once terminated"
        );
    }

    /// A mid-line or past-the-end offset counts nothing twice: the line an
    /// offset lands inside started before it and belongs to the window before.
    #[test]
    fn a_window_offset_past_the_end_or_mid_line_counts_nothing_twice() {
        let bytes = joined(&[prompt("one"), assistant("m", 3, 1, false)]);
        let past = turn_window_since_offset(&bytes, bytes.len() as u64 + 99);
        assert!(!past.usage.is_present());
        assert_eq!(past.next_offset, bytes.len() as u64);
        let mid = turn_window_since_offset(&bytes, bytes.len() as u64 - 4);
        assert!(!mid.usage.is_present());
        assert_eq!(mid.next_offset, bytes.len() as u64);
    }

    /// The context sample is the LAST main-chain call: sidechain records and
    /// usage-free records after it do not displace it.
    #[test]
    fn latest_main_context_stops_at_the_last_main_chain_call() {
        let bytes = joined(&[
            assistant("m-a", 1, 1, false),
            assistant("m-b", 50, 5, false),
            assistant("m-side", 900, 90, true),
            serde_json::json!({"type": "assistant", "message": {"model": "m-c"}}).to_string(),
            tool_result(),
        ]);
        let ctx = latest_main_context(&bytes).expect("a main-chain sample");
        assert_eq!(ctx.usage.input, Some(50));
        assert_eq!(ctx.model.as_deref(), Some("m-b"));
        assert!(latest_main_context(b"").is_none());
        assert!(latest_main_context(&joined(&[assistant("s", 1, 1, true)])).is_none());
    }

    proptest::proptest! {
        /// Total over arbitrary bytes and offsets: never panics, and the next
        /// offset never runs past the bytes it was measured from.
        #[test]
        fn turn_window_is_total(
            bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096),
            offset in 0u64..8192,
        ) {
            let w = turn_window_since_offset(&bytes, offset);
            proptest::prop_assert!(w.next_offset <= bytes.len() as u64);
            let _ = latest_main_context(&bytes);
        }
    }
}
