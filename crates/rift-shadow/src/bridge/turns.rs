//! Turn ordinals and the settlement of a closed turn's spend: the ordinal a
//! hook rides derived from the event stream, the boundaries it closes, and
//! the deferred transcript read that lands provider-reported usage at the
//! turn's own ids one hook later.

use std::path::Path;

use rift_core::{AgentId, Result, Trust};
use rift_shadow_hooks::session_cache::{
    self, read_usage_progress, write_usage_progress, SessionCache, UsageProgress, SCHEMA,
};
use rift_shadow_hooks::transcript::{turn_window_since_offset, TurnWindow};
use rift_shadow_hooks::{CanonicalEvent, EventKind, ModelSource, TurnSignals};
use rift_store::{CanonicalStore, Value};

use super::IngestCtx;
use crate::project::{PayloadFacts, ProjectionCtx};
use crate::turn_boundary::Boundary;

/// When a closed turn's spend is settled from the transcript — the deferred
/// settle and the hook-stop budget share this one fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleMode {
    /// After an ordinary agent hook for the session. `turn_closed_here` says
    /// whether *this* hook closed a turn (its boundaries carry an `End`); then
    /// nothing is settled, because the agent is still flushing the transcript
    /// the Stop fired from — reading it now is the truncated-turn defect the
    /// old inline settle paid 500 ms to avoid.
    AfterHook {
        /// This hook's ingest closed a turn.
        turn_closed_here: bool,
    },
    /// The session will fire no more hooks (the doctor sweep, `session
    /// stop`): the current turn is settled on its last event whether or not
    /// it ever closed.
    Final,
}

/// What one settle did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Nothing to do: no cache, no transcript address, no closed turn past the
    /// last settled one, or this hook closed the turn itself.
    Skipped,
    /// The window for `turn` was consumed and the offset moved.
    Recorded {
        /// The turn whose window was consumed.
        turn: u32,
        /// Whether provider-reported usage was found and projected.
        usage_recorded: bool,
        /// The transcript offset the next window starts at.
        next_offset: u64,
    },
}

/// The most a settle reads in one window. A turn longer than this under-sums
/// honestly: the offset still advances past what was read, so nothing is ever
/// counted twice. Same figure as the retired tail read.
pub const SETTLE_WINDOW_BYTES: u64 = 8 * 1024 * 1024;

/// The turn one hook's events ride, as [`effective_turn`] derived it.
pub(super) struct TurnStamp {
    /// The effective turn ordinal, stamped on every event of the hook.
    pub(super) ordinal: u32,
    /// Whether a turn is open after this hook.
    pub(super) open: bool,
    /// Whether the opener was a Start the adapter did not send, so the caller
    /// records a `Start` boundary first.
    pub(super) synthesized_start: bool,
    /// The agent's own name for the turn this hook rode, when it named one.
    pub(super) native_turn: Option<String>,
}

/// What [`effective_turn`] reads besides the hook's events.
#[derive(Clone, Copy)]
pub(super) struct TurnInput<'a> {
    /// The ordinal an envelope carried, trusted verbatim.
    pub(super) envelope_turn: Option<u32>,
    /// The agent's own name for the turn this hook belongs to.
    pub(super) native_turn: Option<&'a str>,
    /// The session's cache before this hook.
    pub(super) cache: Option<&'a SessionCache>,
    /// The adapter's declared turn signals.
    pub(super) signals: TurnSignals,
}

/// What one settle addresses its rows by.
struct SettleCtx<'a> {
    git_dir: &'a Path,
    session: &'a str,
    owner: &'a str,
    repo_id: &'a str,
    now_ms: i64,
}

/// Settle a closed turn's provider-reported spend from the session's
/// transcript, reading forward from the hook-owned `.usage` sidecar's offset
/// (`session_cache::UsageProgress`).
///
/// **No new table, no new column.** The spend lands through the same
/// `project_usage` / `project_main_context` the Stop-time enrichment used to
/// drive, at the same content-addressed ids: it is anchored on the turn's own
/// `captured_event` row (`turn_end` first, else the turn's last event), so
/// every consumer that reads `turn → model_call → token_usage` sees exactly
/// what it saw before — one hook later. INSERT OR IGNORE plus `settled_turn`
/// make redelivery, a double settle and the sweep idempotent.
///
/// Which turn: after an ordinary hook the closed turn is the one before the
/// open turn (or the current one when no turn is open); under
/// [`SettleMode::Final`] it is the current turn, open or not. A turn at or
/// below `settled_turn`, or turn 0, settles nothing.
///
/// Offsets: no sidecar yet → seed `byte_offset` at the transcript's current
/// length (a late start never back-fills); offset past the file's length (a
/// relocated or rewritten transcript) → reset to the length and count
/// nothing; the read is capped at [`SETTLE_WINDOW_BYTES`].
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure or an unreadable sidecar;
/// the hook path swallows it.
pub async fn settle_closed_turn_usage(
    store: &impl CanonicalStore,
    git_dir: &Path,
    session: &str,
    owner: &str,
    repo_id: &str,
    mode: SettleMode,
    now_ms: i64,
) -> Result<Settled> {
    let Some(cache) = session_cache::read_cache(git_dir, session)? else {
        return Ok(Settled::Skipped);
    };
    let Some(path) = cache.transcript_path.as_deref() else {
        return Ok(Settled::Skipped);
    };
    let path = Path::new(path);
    let len = std::fs::metadata(path).map_or(0, |m| m.len());
    let ctx = SettleCtx {
        git_dir,
        session,
        owner,
        repo_id,
        now_ms,
    };
    let (mut progress, seeded) = usage_progress(&ctx, len)?;
    let Some(turn) = turn_to_settle(&cache, mode, progress.settled_turn) else {
        if seeded {
            write_usage_progress(git_dir, &progress)?;
        }
        return Ok(Settled::Skipped);
    };

    // A relocated or rewritten transcript: never re-count, start over at
    // its end.
    let offset = progress.byte_offset.min(len);
    let count = len.saturating_sub(offset).min(SETTLE_WINDOW_BYTES);
    let window = turn_window_since_offset(&read_span(path, offset, count), 0);
    let next_offset = offset.saturating_add(window.next_offset);
    let usage_recorded = settle_window(store, &ctx, turn, &window).await?;

    progress.byte_offset = next_offset;
    progress.settled_turn = turn;
    progress.updated_at_ms = now_ms;
    write_usage_progress(git_dir, &progress)?;
    Ok(Settled::Recorded {
        turn,
        usage_recorded,
        next_offset,
    })
}

/// Stamp every event of one hook with its effective turn ordinal.
///
/// An End-only adapter (Antigravity: `Stop` closes a turn nothing opened)
/// declares so, and the first activity of a closed turn opens the next
/// ordinal with a SYNTHESIZED Start — otherwise its ordinal never advanced
/// and `record_turn_boundary_snapshot` (insert-or-ignore on
/// `sha256(session ‖ ordinal ‖ boundary)`) dropped every Stop after the
/// first.
pub(super) fn stamp_turn(
    ctx: &IngestCtx<'_>,
    events: &mut [CanonicalEvent],
    cache: Option<&SessionCache>,
) -> TurnStamp {
    let signals = match ctx.registry.get(&ctx.raw.agent) {
        Some(adapter) => adapter.turn_signals(),
        None => TurnSignals::BOTH,
    };
    let native_turn = native_turn_id(ctx);
    let input = TurnInput {
        envelope_turn: ctx.raw.turn,
        native_turn,
        cache,
        signals,
    };
    let (ordinal, open, synthesized_start) = effective_turn(input, events);
    for ev in events.iter_mut() {
        ev.turn_ordinal = ordinal;
    }
    TurnStamp {
        ordinal,
        open,
        synthesized_start,
        native_turn: native_turn.map(str::to_owned),
    }
}

/// The agent's own name for the turn a hook belongs to.
///
/// Codex puts a `turn_id` string on its hook payloads. It is read for Codex
/// only, because it is Codex's documented field; another agent's payload
/// that happened to carry a key of that name has not promised what it means.
fn native_turn_id<'a>(ctx: &IngestCtx<'a>) -> Option<&'a str> {
    if ctx.raw.agent != AgentId::CODEX {
        return None;
    }
    match ctx.raw.payload.get("turn_id")?.as_str()? {
        "" => None,
        id => Some(id),
    }
}

/// The turn boundaries one hook closed: a prompt submission / turn-start
/// closes the human's H_i window; a turn-end closes the agent's A_i window.
/// Deduped per (ordinal, boundary).
pub(super) fn closed_boundaries(
    events: &[CanonicalEvent],
    stamp: &TurnStamp,
) -> Vec<(u32, Boundary)> {
    let mut boundaries = Vec::new();
    if stamp.synthesized_start {
        // The Start an End-only adapter never sends: pushed FIRST, so the
        // human window H_i closes before anything this hook records.
        boundaries.push((stamp.ordinal, Boundary::Start));
    }
    for ev in events {
        let boundary = match ev.kind {
            EventKind::TurnStart | EventKind::PromptSubmit => Boundary::Start,
            EventKind::TurnEnd => Boundary::End,
            _ => continue,
        };
        if !boundaries.contains(&(ev.turn_ordinal, boundary)) {
            boundaries.push((ev.turn_ordinal, boundary));
        }
    }
    boundaries
}

/// The effective turn ordinal for one hook, whether a turn is open after it,
/// and whether its opener was synthesized.
///
/// An envelope-carried ordinal is trusted verbatim. A wire with none (native
/// agent hooks carry no turn) derives it: a prompt/turn-start on a closed
/// turn opens ordinal N+1; further events ride the open turn; a turn-end
/// closes it. A prompt queued mid-turn stays in the open turn.
///
/// Three openers stand in for a prompt the adapter did not send, and each is
/// reported as synthesized so the caller records a `Start` boundary first:
///
/// * an End-only adapter (`end && !start`) opens a closed turn on its first
///   event that is not itself a `TurnEnd`/`SessionEnd`;
/// * any other adapter's first activity (a tool call, a tool result, an edit
///   or a turn end) at turn 0 opens turn 1, because work is being done and a
///   client that skipped its prompt hook (Codex does, when the hook's trust
///   lapses) would otherwise leave every event at turn 0, where no spend is
///   ever settled;
/// * a native turn name that differs from the one the cache holds opens the
///   next ordinal, closing a turn whose end never arrived.
pub(super) fn effective_turn(input: TurnInput<'_>, events: &[CanonicalEvent]) -> (u32, bool, bool) {
    let real_open = events
        .iter()
        .any(|e| matches!(e.kind, EventKind::PromptSubmit | EventKind::TurnStart));
    let ends = events.iter().any(|e| e.kind == EventKind::TurnEnd);
    let (current, was_open) = input
        .cache
        .map_or((0, false), |c| (c.current_turn_ordinal, c.turn_open));
    let known = input.cache.and_then(|c| c.native_turn.as_deref());
    let renamed = matches!((input.native_turn, known), (Some(now), Some(before)) if now != before);
    let end_only = input.signals.end && !input.signals.start;
    let end_only_opener = end_only
        && !was_open
        && events
            .iter()
            .any(|e| !matches!(e.kind, EventKind::TurnEnd | EventKind::SessionEnd));
    let first_activity = !end_only
        && current == 0
        && !was_open
        && events.iter().any(|e| {
            matches!(
                e.kind,
                EventKind::ToolCall
                    | EventKind::ToolResult
                    | EventKind::FileEdit
                    | EventKind::TurnEnd
            )
        });
    let synthesized = input.envelope_turn.is_none()
        && (end_only_opener || (!real_open && (first_activity || renamed)));
    let opens = real_open || synthesized;
    let turn = match input.envelope_turn {
        Some(t) => t,
        None if opens && (!was_open || renamed) => current.saturating_add(1),
        None => current,
    };
    (turn, (was_open || opens) && !ends, synthesized)
}

/// The sidecar's progress, or one seeded at the transcript's current length
/// (a late start never back-fills). The flag says the sidecar was seeded
/// here, so a settle that then finds nothing to do still writes it.
fn usage_progress(ctx: &SettleCtx<'_>, len: u64) -> Result<(UsageProgress, bool)> {
    match read_usage_progress(ctx.git_dir, ctx.session)? {
        Some(progress) => Ok((progress, false)),
        None => Ok((
            UsageProgress {
                schema: SCHEMA,
                session_id: ctx.session.to_owned(),
                byte_offset: len,
                settled_turn: 0,
                updated_at_ms: ctx.now_ms,
            },
            true,
        )),
    }
}

/// Which turn a settle consumes, if any: the hook that closed a turn never
/// settles it, an ordinary hook settles the turn before the open one, and a
/// final settle takes the current turn open or not. A turn at or below the
/// last settled one, or turn 0, settles nothing.
fn turn_to_settle(cache: &SessionCache, mode: SettleMode, settled_turn: u32) -> Option<u32> {
    let turn = match mode {
        SettleMode::AfterHook {
            turn_closed_here: true,
        } => return None,
        SettleMode::AfterHook { .. } if cache.turn_open => {
            cache.current_turn_ordinal.checked_sub(1)?
        }
        SettleMode::AfterHook { .. } | SettleMode::Final => cache.current_turn_ordinal,
    };
    if turn == 0 || turn <= settled_turn {
        return None;
    }
    Some(turn)
}

/// Land the window's spend at the turn's own anchor; nothing is written when
/// the window carried no usage and no context sample, or the turn has no
/// anchor. Says whether provider-reported usage was projected.
async fn settle_window(
    store: &impl CanonicalStore,
    ctx: &SettleCtx<'_>,
    turn: u32,
    window: &TurnWindow,
) -> Result<bool> {
    if !window.usage.is_present() && window.main_context.is_none() {
        return Ok(false);
    }
    let Some(anchor) = turn_anchor(store, ctx.session, turn).await? else {
        return Ok(false);
    };
    let facts = PayloadFacts {
        model: window.model.clone(),
        model_source: window
            .model
            .as_ref()
            .map(|_| ModelSource::RuntimeHook.as_str().to_owned()),
        tool_name: None,
        child_session: None,
        read_path: None,
        read_content: None,
    };
    let projection = ProjectionCtx {
        agent: ctx.owner,
        repo_id: ctx.repo_id,
    };
    let mut usage_recorded = false;
    if window.usage.is_present() {
        crate::project::project_usage(
            store,
            &anchor,
            &facts,
            projection,
            &window.usage,
            ctx.now_ms,
        )
        .await?;
        usage_recorded = true;
    }
    if let Some(context) = &window.main_context {
        crate::project::project_main_context(
            store,
            &anchor,
            "Stop",
            &facts,
            projection,
            &context.usage,
        )
        .await?;
    }
    Ok(usage_recorded)
}

/// `count` bytes of `path` from `offset`; empty on any I/O failure (a settle
/// that cannot read counts nothing and says so through the offset it leaves).
fn read_span(path: &Path, offset: u64, count: u64) -> Vec<u8> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.take(count).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    buf
}

/// The event a turn's settled spend anchors on: its `turn_end` row when one
/// was captured, else the turn's last event — rebuilt as a synthetic
/// [`CanonicalEvent`] so the projection derives the same ids the Stop-time
/// path would have. The synthetic session-start an End-only adapter gets
/// (ordinal `u32::MAX`) is not a spend anchor.
async fn turn_anchor(
    store: &impl CanonicalStore,
    session: &str,
    turn: u32,
) -> Result<Option<CanonicalEvent>> {
    let rows = store
        .query(
            "SELECT event_ordinal, occurred_at_ms, payload_sha256, schema_version \
             FROM captured_event \
             WHERE session_id = ? AND turn_ordinal = ? AND kind != 'session_start' \
             ORDER BY (kind = 'turn_end') DESC, event_ordinal DESC LIMIT 1",
            &[Value::text(session.to_owned()), Value::Int(i64::from(turn))],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let ordinal = u32::try_from(row.get_i64(0)?).unwrap_or(u32::MAX);
    let occurred_at_ms = row.get_i64(1)?;
    let payload_sha256 = rift_core::Sha256Id::parse_hex(&row.get_text(2)?)?;
    let schema_version = u16::try_from(row.get_i64(3)?).unwrap_or(1);
    Ok(Some(CanonicalEvent {
        schema_version,
        session: session.to_owned(),
        turn_ordinal: turn,
        event_ordinal: ordinal,
        kind: EventKind::TurnEnd,
        trust: Trust::TranscriptUntrusted,
        occurred_at_ms,
        payload_sha256,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_shadow_hooks::{CanonicalEvent, Trust};

    fn ev(kind: EventKind) -> CanonicalEvent {
        CanonicalEvent {
            schema_version: 1,
            session: String::new(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind,
            trust: Trust::GitFact,
            occurred_at_ms: 0,
            payload_sha256: rift_core::Sha256Id::from_bytes([0u8; 32]),
        }
    }

    /// One hook of one event kind, with no native turn name.
    fn turn_of(
        envelope_turn: Option<u32>,
        kind: EventKind,
        cache: Option<&SessionCache>,
        signals: TurnSignals,
    ) -> (u32, bool, bool) {
        let input = TurnInput {
            envelope_turn,
            native_turn: None,
            cache,
            signals,
        };
        effective_turn(input, &[ev(kind)])
    }

    #[test]
    fn effective_turn_trusts_envelope_and_handles_queued_prompts() {
        let mut cache = SessionCache::new("s", "claude-code", "active", 0);
        cache.current_turn_ordinal = 4;
        cache.turn_open = true;
        // Envelope ordinal wins verbatim, even against a divergent cache.
        let both = TurnSignals::BOTH;
        let (t, _, _) = turn_of(Some(9), EventKind::ToolCall, Some(&cache), both);
        assert_eq!(t, 9);
        // A prompt queued while the turn is open stays in the open turn.
        let (t, open, _) = turn_of(None, EventKind::PromptSubmit, Some(&cache), both);
        assert_eq!((t, open), (4, true));
        // No cache at all: the first prompt opens turn 1.
        let (t, open, _) = turn_of(None, EventKind::PromptSubmit, None, both);
        assert_eq!((t, open), (1, true));
        // A turn-end with nothing open at turn 0 is work the prompt hook did
        // not announce: it opens turn 1 and closes it in the same hook.
        let got = turn_of(None, EventKind::TurnEnd, None, both);
        assert_eq!(got, (1, false, true));
        // Past turn 0 a stray turn-end stays on the closed ordinal.
        cache.turn_open = false;
        let got = turn_of(None, EventKind::TurnEnd, Some(&cache), both);
        assert_eq!(got, (4, false, false));
    }

    /// **An End-only adapter gets a synthesized Start at session start and at
    /// the next activity after each End** (2026-08-29). Antigravity's `Stop`
    /// closes a turn nothing opened; before this, its ordinal never advanced,
    /// so `record_turn_boundary_snapshot` (insert-or-ignore on
    /// `sha256(session ‖ ordinal ‖ boundary)`) dropped every Stop after the
    /// first. Now the first non-End event of a closed turn opens the next
    /// ordinal and reports the synthesized Start.
    #[test]
    fn an_end_only_adapter_gets_a_synthesized_start_at_session_start_and_next_activity() {
        let end_only = TurnSignals {
            start: false,
            end: true,
        };
        // Session start, no cache: opens turn 1 with a synthesized Start.
        let got = turn_of(None, EventKind::SessionStart, None, end_only);
        assert_eq!(got, (1, true, true));
        // Activity on the open turn rides it — nothing synthesized.
        let mut cache = SessionCache::new("s", "antigravity", "active", 0);
        cache.current_turn_ordinal = 1;
        cache.turn_open = true;
        let got = turn_of(None, EventKind::ToolCall, Some(&cache), end_only);
        assert_eq!(got, (1, true, false));
        // The End closes it and is never an opener.
        let got = turn_of(None, EventKind::TurnEnd, Some(&cache), end_only);
        assert_eq!(got, (1, false, false));
        // The next activity after the End opens turn 2 with a synthesized Start…
        cache.turn_open = false;
        let got = turn_of(None, EventKind::ToolCall, Some(&cache), end_only);
        assert_eq!(got, (2, true, true));
        // …but a lone End on a closed turn, or a SessionEnd, opens nothing.
        let got = turn_of(None, EventKind::TurnEnd, Some(&cache), end_only);
        assert_eq!(got, (1, false, false));
        let got = turn_of(None, EventKind::SessionEnd, Some(&cache), end_only);
        assert_eq!(got, (1, false, false));
        // An envelope-carried ordinal is trusted verbatim and never synthesized
        // over: nothing opens (no prompt rode it), nothing is synthesized.
        let got = turn_of(Some(7), EventKind::ToolCall, Some(&cache), end_only);
        assert_eq!(got, (7, false, false));
    }

    /// **A Start-capable adapter opens turn 1 on its first activity, and only
    /// there.** Its own prompt/turn-start stays the opener of every later
    /// turn; but a session whose prompt hook never arrived (Codex, when a
    /// hook's trust lapses) would otherwise hold every event at turn 0, where
    /// no spend is ever settled. A session start is not activity and opens
    /// nothing.
    #[test]
    fn a_start_capable_adapter_opens_turn_one_on_first_activity_and_only_there() {
        for signals in [
            TurnSignals::BOTH,
            TurnSignals {
                start: true,
                end: false,
            },
            TurnSignals {
                start: false,
                end: false,
            },
        ] {
            let got = turn_of(None, EventKind::SessionStart, None, signals);
            assert_eq!(got, (0, false, false), "{signals:?}");
            let got = turn_of(None, EventKind::ToolCall, None, signals);
            assert_eq!(got, (1, true, true), "{signals:?}");
            let got = turn_of(None, EventKind::PromptSubmit, None, signals);
            assert_eq!(got, (1, true, false), "{signals:?}");
            // Past turn 0, activity on a closed turn stays on it.
            let mut cache = SessionCache::new("s", "codex", "active", 0);
            cache.current_turn_ordinal = 3;
            let got = turn_of(None, EventKind::ToolCall, Some(&cache), signals);
            assert_eq!(got, (3, false, false), "{signals:?}");
        }
    }

    /// Codex names its turns. A new name is a new turn with no prompt hook
    /// in sight, the same name rides the turn it names, and a name first seen
    /// on a cache that recorded none changes nothing.
    #[test]
    fn a_new_native_turn_name_opens_the_next_turn() {
        let mut cache = SessionCache::new("s", "codex", "active", 0);
        cache.current_turn_ordinal = 2;
        cache.native_turn = Some("turn-b".to_owned());
        let named = |name: &'static str, kind: EventKind, cache: &SessionCache| {
            let input = TurnInput {
                envelope_turn: None,
                native_turn: Some(name),
                cache: Some(cache),
                signals: TurnSignals::BOTH,
            };
            effective_turn(input, &[ev(kind)])
        };
        assert_eq!(
            named("turn-b", EventKind::ToolCall, &cache),
            (2, false, false)
        );
        assert_eq!(
            named("turn-c", EventKind::ToolCall, &cache),
            (3, true, true)
        );
        // A turn whose end never arrived is closed by the next name.
        cache.turn_open = true;
        assert_eq!(
            named("turn-c", EventKind::ToolCall, &cache),
            (3, true, true)
        );
        assert_eq!(
            named("turn-c", EventKind::TurnEnd, &cache),
            (3, false, true)
        );
        cache.native_turn = None;
        assert_eq!(
            named("turn-c", EventKind::ToolCall, &cache),
            (2, true, false)
        );
    }
}
