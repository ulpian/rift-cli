//! Winning the right to hand a window its map, once, and booking what the map
//! cost.
//!
//! Both halves are state writes rather than text, and both are the reason the
//! map is handed over exactly once per generation: the marker decides, and the
//! audit row is what the honesty gate nets afterwards.

use rift_core::Result;

use rift_recall_ledger::attribution::append_attributed_serve;

use crate::hooks::audit::{serve_attribution, serve_audit_envelopes, serve_audit_event};
use crate::hooks::gate::AUDIT_IDS;
use crate::hooks::now_ms;

/// Win the once-per-generation right to hand this window its map, atomically.
///
/// The same `create_new` discipline the session pointer's claim uses, and for
/// the same reason: the decision counts only once the state write persisted, so
/// a retried hook re-reads the marker and stays silent.
///
/// What the marker is named for is the load-bearing part. It cannot be the
/// eviction, because **the boundary hook writes an eviction of its own** — a
/// second delivery of the same hook is a second eviction line and would be a
/// second claim over the very same held set. So the name is the window and the
/// **content of the map's input**: the files, the revisions, the lines and when
/// each was last read. Two hooks that would hand over the same listing hand it
/// over once; a context that has read something since, and lost that too, is a
/// different listing and gets it.
pub(super) fn claim_recovery(
    repo: &std::path::Path,
    session: &str,
    window: Option<&str>,
    held: &[rift_agent::window_journal::HeldFile],
) -> bool {
    use std::fmt::Write as _;
    let dir = repo.join(".rift").join("state").join("recovery");
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    // Hashed, so nothing an agent supplied — a session id, a path — becomes a
    // path component here, exactly as on the journal's own file names.
    let mut fingerprint = format!("{session}\u{0}{}", window.unwrap_or_default());
    for file in held {
        let _ = write!(
            fingerprint,
            "\u{0}{}@{}:{}",
            file.path,
            file.hash.to_hex(),
            file.last_read_at_ms
        );
        for run in file.held.runs() {
            let _ = write!(fingerprint, ",{}-{}", run.lo, run.hi);
        }
    }
    let name = rift_hashchain::leaf_hash(fingerprint.as_bytes()).to_hex();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(format!("{name}.json")))
        .is_ok()
}

/// Book what the map cost, in both relations, through the same pair of
/// envelopes every other hook-side decision is written with, attributed to
/// the session and window it was handed to.
///
/// A recovery map is pure spend — it avoids nothing by itself, and what it
/// saves is a read that never happens later — so it books a returned figure and
/// no avoided one, which is exactly what the honesty gate has to net. The
/// attribution is what the session's next checkpoint freezes the map's rows
/// by, keyed exactly as the session pointer's: the native session id and the
/// window journal's key of the window the map was decided from.
/// Best-effort: an unavailable projection costs an audit row, never a turn.
pub(super) fn record_recovery_injection(
    repo: &std::path::Path,
    agent: &rift_core::AgentId,
    session: &str,
    window: Option<&str>,
    text: &str,
) {
    use rift_core::UlidSource as _;
    let audit = crate::agent_queue::ServeAudit {
        call_id: AUDIT_IDS.next_ulid().to_hex(),
        decision: crate::agent_queue::ServeAuditKind::RecoveryInjected,
        agent: agent.as_str().to_owned(),
        basis: None,
        frame: text.to_owned(),
        tag: None,
        total: None,
        declared_avoided: None,
        // Not a name search: nothing here stood in for a listing.
        shape: None,
        session: Some(session.to_owned()),
        window: Some(crate::window_journal::key(session, window)),
    };
    let attribution = serve_attribution(audit.session.as_deref(), audit.window.as_deref());
    let _ = (|| -> Result<()> {
        let db = crate::family::recall_db(repo)?;
        let store = rift_store::open_recall_projection(&db)?;
        let at_ms = now_ms();
        let event = serve_audit_event(repo, &audit);
        let (audited, called) = serve_audit_envelopes(&event, &audit, at_ms);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(rift_core::RiftError::from)?;
        rt.block_on(async {
            for envelope in [audited, called] {
                append_attributed_serve(&store, envelope.as_bytes(), at_ms, attribution.as_ref())
                    .await?;
            }
            Ok(())
        })
    })();
}
