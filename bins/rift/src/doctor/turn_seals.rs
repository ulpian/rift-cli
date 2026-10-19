//! The sessions whose last turn-end seal fell back to a whole reseal, or
//! whose live transcript has run past that seal by more than the bound.
//!
//! Each turn-end checkpoint records the frontier it sealed in the session's
//! `turn-seal` sidecar; this leg reads those files and the transcript's
//! length, one closed word per named session (`segment-chain-broken`,
//! `packs-changed`, `lagging`, `unreadable`). Report only: it opens no store
//! and writes nothing, so `--fix` changes nothing here.

use std::path::Path;

use crate::hooks::turn_end::frontier::{recorded, Recorded};

/// The turn-seal leg. Silent when every recorded seal is well.
pub(super) fn doctor_turn_seals(repo: &Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    turn_seal_leg(&paths.git_dir)
}

fn turn_seal_leg(git_dir: &Path) -> (String, serde_json::Value) {
    let sessions = crate::family::run_state::sessions_dir(git_dir);
    let mut human = String::new();
    let mut named = Vec::new();
    for entry in recorded(&sessions) {
        let frontier = match entry {
            Recorded::Frontier(f) => f,
            Recorded::Unreadable(session) => {
                human.push_str(&format!(
                    "\n  turn seal: session {session} unreadable (its frontier record does not parse)"
                ));
                named.push(serde_json::json!({ "session": session, "finding": "unreadable" }));
                continue;
            }
        };
        let lag = frontier.lag_bytes(git_dir);
        let Some(word) = frontier.finding(lag) else {
            continue;
        };
        human.push_str(&format!(
            "\n  turn seal: session {} {word} at checkpoint {} (sealed {} bytes, lag {} bytes)",
            frontier.session_id,
            frontier.checkpoint,
            frontier.read_bytes,
            lag.map_or_else(|| "unknown".to_owned(), |l| l.to_string()),
        ));
        named.push(serde_json::json!({
            "session": frontier.session_id,
            "finding": word,
            "checkpoint": frontier.checkpoint,
            "frontier_bytes": frontier.read_bytes,
            "lag_bytes": lag,
        }));
    }
    (human, serde_json::json!({ "sessions": named }))
}

#[cfg(test)]
mod tests {
    use crate::hooks::turn_end::frontier::{write, SealFallback, TurnFrontier};

    use super::*;

    fn frontier(session: &str, fallback: Option<SealFallback>) -> TurnFrontier {
        TurnFrontier {
            session_id: session.to_owned(),
            checkpoint: format!("cp-{session}"),
            read_bytes: 40,
            sealed_lines: None,
            fallback,
            sealed_at_ms: 1,
            compact: None,
        }
    }

    #[test]
    fn a_fallen_back_seal_is_named_by_its_closed_word_and_a_clean_one_is_silent() {
        let checkout = tempfile::tempdir().unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(checkout.path())
            .status()
            .unwrap();
        assert!(ok.success());
        let git_dir = rift_git::discover(checkout.path()).unwrap().git_dir;
        let sessions = crate::family::run_state::sessions_dir(&git_dir);
        std::fs::create_dir_all(&sessions).unwrap();
        assert_eq!(turn_seal_leg(&git_dir).0, "");
        write(&sessions, &frontier("s-clean", None)).unwrap();
        write(
            &sessions,
            &frontier("s-packs", Some(SealFallback::PacksChanged)),
        )
        .unwrap();
        std::fs::write(sessions.join("s-torn.turn-seal"), b"{").unwrap();
        let (human, json) = turn_seal_leg(&git_dir);
        assert!(
            human.contains("session s-packs packs-changed at checkpoint cp-s-packs"),
            "{human}"
        );
        assert!(human.contains("session s-torn unreadable"), "{human}");
        assert!(!human.contains("s-clean"), "{human}");
        assert_eq!(json["sessions"][0]["finding"], "packs-changed");
        assert_eq!(json["sessions"][1]["finding"], "unreadable");
    }
}
