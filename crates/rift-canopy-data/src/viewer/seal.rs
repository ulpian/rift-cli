//! The turn-seal card: for each session a checkpoint covers, the frontier
//! its last turn-end seal recorded and how far the live transcript has run
//! past it (L-5, design Q9).
//!
//! The facts come from the session's `.turn-seal` sidecar, read through the
//! format the hook binary writes (`rift_shadow_hooks::turn_frontier`); the
//! compactor's carry in it is skipped, since the card says nothing about it.
//! The card also says whether this checkpoint is the session's newest
//! turn-end seal, so a reader on an older checkpoint knows a fresher
//! transcript is one row away rather than taking this one as the latest.

use std::path::Path;

use rift_canopy::{ViewerLine, ViewerLineKind, ViewerTimelineItem, ViewerTimelineSegment};
use rift_recall_substrate::CheckpointCatalogueEntry;
use rift_shadow_hooks::turn_frontier::{recorded, Recorded, TurnFrontier};

use super::text::{compact_utc, fact_line, short, warning_item};

/// The card for `entry`'s sessions, `None` when none of them has a sidecar
/// (a checkpoint from before turn-end seals, or a session sealed elsewhere).
pub(super) fn turn_seal_segment(
    sessions: &Path,
    entry: &CheckpointCatalogueEntry,
) -> Option<ViewerTimelineSegment> {
    let covered = &entry.metadata.sessions;
    let items: Vec<ViewerTimelineItem> = recorded(sessions)
        .into_iter()
        .filter_map(|record| match record {
            Recorded::Frontier(f) if covered.contains(&f.session_id) => {
                Some(frontier_item(sessions, &entry.checkpoint_id, &f))
            }
            Recorded::Unreadable(session) if covered.contains(&session) => Some(warning_item(
                &format!("turn-seal:{session}"),
                &format!("turn seal {session}"),
                "the frontier record does not parse",
            )),
            Recorded::Frontier(_) | Recorded::Unreadable(_) => None,
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(ViewerTimelineSegment {
        title: "Turn seal".to_owned(),
        boundary: Some(
            "the last turn-end seal per session; lag is the live transcript past that seal"
                .to_owned(),
        ),
        ordered_by_time: false,
        items,
    })
}

fn frontier_item(sessions: &Path, checkpoint_id: &str, f: &TurnFrontier) -> ViewerTimelineItem {
    ViewerTimelineItem {
        id: format!("turn-seal:{}", f.session_id),
        title: format!("turn seal {}", f.session_id),
        timestamp: compact_utc(f.sealed_at_ms),
        summary: vec![
            newest_line(checkpoint_id, &f.checkpoint),
            lag_line(f.lag_bytes(sessions)),
        ],
        body: frontier_lines(f),
        expanded_body: None,
        collapsed: false,
        full_session_only: false,
    }
}

fn newest_line(checkpoint_id: &str, frontier: &str) -> ViewerLine {
    if frontier == checkpoint_id {
        return fact_line("frontier", "this checkpoint is the newest turn-end seal");
    }
    ViewerLine::plain(
        ViewerLineKind::Warning,
        format!(
            "frontier: a newer turn-end seal, checkpoint {}, holds a fresher transcript",
            short(frontier)
        ),
    )
}

fn lag_line(lag: Option<u64>) -> ViewerLine {
    match lag {
        Some(bytes) => fact_line(
            "seal lag",
            &format!("{bytes} bytes behind the live transcript"),
        ),
        None => fact_line("seal lag", "no live transcript on record"),
    }
}

fn frontier_lines(f: &TurnFrontier) -> Vec<ViewerLine> {
    let sealed = match (f.sealed_lines, f.fallback) {
        (_, Some(fallback)) => format!("whole reseal ({})", fallback.word()),
        (Some(lines), None) => format!("{lines} lines in segments"),
        (None, None) => "nothing new".to_owned(),
    };
    vec![
        fact_line("frontier checkpoint", &f.checkpoint),
        fact_line("read", &format!("{} bytes", f.read_bytes)),
        fact_line("sealed", &sealed),
    ]
}

#[cfg(test)]
mod tests {
    use rift_shadow_hooks::turn_frontier::{write, SealFallback};

    use super::super::fixtures::{entry, item_text};
    use super::*;

    fn frontier(session: &str, checkpoint: &str, read_bytes: u64) -> TurnFrontier<u8> {
        TurnFrontier {
            session_id: session.to_owned(),
            checkpoint: checkpoint.to_owned(),
            read_bytes,
            sealed_lines: Some(3),
            fallback: None,
            sealed_at_ms: 0,
            compact: Some(7),
            carried: Vec::new(),
            scan: None,
        }
    }

    fn covering(id: &str, sessions: &[&str]) -> CheckpointCatalogueEntry {
        let mut e = entry(id, &"a".repeat(40));
        e.metadata.sessions = sessions.iter().map(|s| (*s).to_owned()).collect();
        e
    }

    fn texts(segment: &ViewerTimelineSegment) -> String {
        segment
            .items
            .iter()
            .map(item_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_card_names_the_frontier_and_the_bytes_the_live_transcript_ran_past_it() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.jsonl");
        std::fs::write(&live, "x".repeat(100)).unwrap();
        let mut cache = rift_shadow_hooks::SessionCache::new("s1", "claude-code", "active", 0);
        cache.transcript_path = Some(live.to_string_lossy().into_owned());
        rift_shadow_hooks::write_cache(dir.path(), &cache).unwrap();
        write(dir.path(), &frontier("s1", "cp-new", 60)).unwrap();

        let card = turn_seal_segment(dir.path(), &covering("cp-new", &["s1"])).unwrap();
        let text = texts(&card);
        assert!(
            text.contains("this checkpoint is the newest turn-end seal"),
            "{text}"
        );
        assert!(
            text.contains("40 bytes behind the live transcript"),
            "{text}"
        );
        assert!(text.contains("3 lines in segments"), "{text}");
    }

    #[test]
    fn an_older_checkpoint_points_at_the_newer_seal_and_a_fallback_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = frontier("s1", "cp-new", 10);
        f.fallback = Some(SealFallback::PacksChanged);
        write(dir.path(), &f).unwrap();

        let card = turn_seal_segment(dir.path(), &covering("cp-old", &["s1"])).unwrap();
        let text = texts(&card);
        assert!(
            text.contains("a newer turn-end seal, checkpoint cp-new"),
            "{text}"
        );
        assert!(text.contains("whole reseal (packs-changed)"), "{text}");
        assert!(text.contains("no live transcript on record"), "{text}");
    }

    #[test]
    fn sessions_without_a_sidecar_draw_no_card_and_a_torn_record_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &frontier("other", "cp", 1)).unwrap();
        assert!(turn_seal_segment(dir.path(), &covering("cp", &["s1"])).is_none());

        std::fs::write(dir.path().join("s1.turn-seal"), b"{not json").unwrap();
        let card = turn_seal_segment(dir.path(), &covering("cp", &["s1"])).unwrap();
        assert!(texts(&card).contains("does not parse"));
    }
}
