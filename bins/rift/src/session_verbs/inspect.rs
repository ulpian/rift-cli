//! `rift session list` and `rift session info`: the read side, where the
//! journal's live facts and the projection's totals are joined into one row.

use std::path::Path;

use rift_core::Result;
use rift_store::CanonicalStore as _;

use super::journals::{family_journal, MemberJournal};
use super::{exit, safe, Outcome};
use crate::provenance::Label;

/// The state `session list` and `session info` print for a session whose
/// cache file exists and does not parse — never `archived`, which would say
/// the session is over when it is Rift's own file that is broken.
const CACHE_UNREADABLE: &str = "cache unreadable";

/// `rift session list`: the session-STATE surface. The
/// journal (live and ended sessions across **every member of the repository
/// family**, with ages) is the primary; the projection — the family's one
/// store — adds agents/models/token totals per session and
/// contributes sessions the journal no longer holds (historical, imported).
///
/// The enumeration is [`family_journal`], so this reads the same set from any
/// member: run from a linked worktree it used to list that worktree and its
/// siblings and silently omit the main one, which is where most sessions are
/// where most sessions are.
pub(crate) async fn list(store: &rift_store::SqliteStore, root: &Path) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    let now = crate::hooks::now_ms();
    let journal = family_journal(&repo);
    let unreadable: std::collections::BTreeSet<String> = journal
        .iter()
        .flat_map(|member| member.unreadable.iter().cloned())
        .collect();
    let (mut rows, journalled) = journalled_rows(store, journal, now).await;
    rows.extend(archived_rows(store, &journalled, &unreadable).await?);
    rows.sort_by_key(|r| {
        std::cmp::Reverse(
            r.get("last_at_ms")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
        )
    });
    Ok(Outcome::ok(
        session_lines(&rows),
        serde_json::json!({ "sessions": rows }),
    ))
}

/// The rows the journal itself holds, family-wide, and the native ids they
/// cover — the set the projection pass must not list a second time.
async fn journalled_rows(
    store: &rift_store::SqliteStore,
    journal: Vec<MemberJournal>,
    now: i64,
) -> (Vec<serde_json::Value>, std::collections::BTreeSet<String>) {
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for member in journal {
        for s in member.sessions {
            seen.insert(s.session_id.clone());
            let row_id = rift_core::capture_session_row_id(&s.agent, &s.session_id);
            rows.push(
                session_row_json(
                    store,
                    &s.session_id,
                    &row_id,
                    Some(&s.agent),
                    &s.state,
                    Some(now.saturating_sub(s.updated_at_ms)),
                    s.updated_at_ms,
                )
                .await,
            );
        }
    }
    (rows, seen)
}

/// Projection-only sessions (their journal entry is gone, or they were
/// imported): the identity map recovers the native id the operator knows them
/// by; a session it never bound falls back to its row id.
///
/// A session whose cache file exists but does not parse is not archived, and
/// is not printed as one: its state reads `cache unreadable` until the
/// session's next hook moves the file aside and writes the cache whole.
async fn archived_rows(
    store: &rift_store::SqliteStore,
    journalled: &std::collections::BTreeSet<String>,
    unreadable: &std::collections::BTreeSet<String>,
) -> Result<Vec<serde_json::Value>> {
    let projected = store
        .query(
            "SELECT s.session_id, s.agent, COALESCE(ci.native_session_id, s.session_id), \
             s.started_at \
             FROM session s \
             LEFT JOIN capture_session_identity ci ON ci.session_id = s.session_id \
             ORDER BY s.started_at DESC LIMIT 50",
            &[],
        )
        .await?;
    let mut rows = Vec::new();
    for r in &projected {
        let row_id = r.get_text(0)?;
        let agent = r.get_text(1)?;
        let native = r.get_text(2).unwrap_or_else(|_| row_id.clone());
        if journalled.contains(&native) {
            continue;
        }
        let started = r.get_i64(3).unwrap_or(0);
        let state = if unreadable.contains(&native) {
            CACHE_UNREADABLE
        } else {
            "archived"
        };
        rows.push(
            session_row_json(store, &native, &row_id, Some(&agent), state, None, started).await,
        );
    }
    Ok(rows)
}

/// One line per session, in the order the caller sorted them.
fn session_lines(rows: &[serde_json::Value]) -> String {
    let mut human = String::from("sessions:\n");
    if rows.is_empty() {
        human.push_str("  (none)");
    }
    for r in rows {
        let text = |k: &str| {
            r.get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_owned()
        };
        let age = r
            .get("age_ms")
            .and_then(serde_json::Value::as_i64)
            .map_or(String::new(), |ms| format!("  age={}", human_age(ms)));
        let tokens = r
            .pointer("/tokens/output")
            .and_then(serde_json::Value::as_i64)
            .map_or(String::new(), |out| format!("  out-tokens={out}"));
        human.push_str(&format!(
            "  {}  {}  {}  {}  worktree={}{age}{tokens}\n",
            Label::of(&text("session_id")),
            Label::of(&text("agent")),
            Label::of(&text("model")),
            text("state"),
            // Absence renders through the shared constant, so this column and
            // the dashboard's badge say `unknown` with the same word.
            r.get("worktree")
                .and_then(serde_json::Value::as_str)
                .map_or_else(
                    || crate::provenance::UNKNOWN_WORKTREE.to_owned(),
                    str::to_owned
                ),
        ));
    }
    human.trim_end().to_owned()
}

/// One list/info row: journal facts + the projection's agent/model/token
/// totals, resolved through the derived session row id.
async fn session_row_json(
    store: &rift_store::SqliteStore,
    native_id: &str,
    row_id: &str,
    agent: Option<&str>,
    state: &str,
    age_ms: Option<i64>,
    last_at_ms: i64,
) -> serde_json::Value {
    let meta = store
        .query(
            "SELECT agent, COALESCE(model_primary, 'unknown') FROM session WHERE session_id = ?",
            &[rift_store::Value::text(row_id.to_owned())],
        )
        .await
        .ok();
    let (proj_agent, model) = meta
        .as_ref()
        .and_then(|rows| rows.first())
        .map_or((None, None), |r| (r.get_text(0).ok(), r.get_text(1).ok()));
    let tokens = crate::provenance::session_cost(store, row_id)
        .await
        .ok()
        .flatten();
    // The session's CURRENT worktree — its latest observation.
    // `null` is UNKNOWN, which is every session captured before worktree
    // provenance existed, and the human column below spells it rather than
    // defaulting to `main`.
    let worktree = crate::provenance::session_worktree(store, row_id).await;
    serde_json::json!({
        "session_id": native_id,
        "row_id": row_id,
        "worktree": worktree,
        "agent": agent.map(str::to_owned).or(proj_agent),
        "model": model,
        "state": state,
        "age_ms": age_ms,
        "last_at_ms": last_at_ms,
        "tokens": tokens.as_ref().map(|c| serde_json::json!({
            "uncached_input": c.uncached_input,
            "cached_input": c.cached_input,
            "cache_creation": c.cache_creation,
            "output": c.output,
        })),
        "amount_micros": tokens.as_ref().and_then(|c| c.amount_micros),
    })
}

/// A coarse human age: seconds under a minute, then minutes/hours/days.
fn human_age(ms: i64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// `rift session info ID` — one session in full: journal state + age,
/// projection identity, four-class token totals, and its checkpoints.
pub(crate) async fn info(
    store: &rift_store::SqliteStore,
    root: &Path,
    reference: &str,
) -> Result<Outcome> {
    use rift_shadow::hooks::session_cache::read_cache;
    let repo = rift_git::discover(root)?;
    let now = crate::hooks::now_ms();
    let sessions = crate::family::run_state::sessions_dir(&repo.git_dir);
    let read = read_cache(&sessions, reference);
    let fallback = if read.is_err() {
        CACHE_UNREADABLE
    } else {
        "archived"
    };
    let journal = read.ok().flatten();
    let Some(row_id) = projection_row_id(store, journal.as_ref(), reference).await? else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!(
                "no session {} in the journal or the projection",
                safe(reference)
            ),
            serde_json::json!({ "error": "unknown_session", "session_id": reference }),
        ));
    };
    let state = journal.as_ref().map_or(fallback, |c| c.state.as_str());
    let age_ms = journal
        .as_ref()
        .map(|c| now.saturating_sub(c.updated_at_ms));
    let last = journal.as_ref().map_or(0, |c| c.updated_at_ms);
    let mut row = session_row_json(store, reference, &row_id, None, state, age_ms, last).await;
    let checkpoint_rows = checkpoints_of(store, &row_id).await?;
    // `info` is the surface that carries the HISTORY — the ordered
    // set of every worktree the session was observed in. One session id can be
    // observed in several checkouts over its life (a worktree-isolated subagent
    // runs under its parent's id), and the ordered set is what an auditor
    // reads; the scalar `worktree` above is its last element.
    let history = crate::provenance::session_worktree_history(store, &row_id).await;
    if let Some(obj) = row.as_object_mut() {
        obj.insert("worktree_history".into(), serde_json::json!(history));
        obj.insert("checkpoints".into(), serde_json::json!(checkpoint_rows));
        obj.insert(
            "transcript_path".into(),
            journal
                .as_ref()
                .and_then(|c| c.transcript_path.clone())
                .map_or(serde_json::Value::Null, serde_json::Value::from),
        );
    }
    let mut human = format!(
        "session {}\n  state: {}{}\n  worktree: {}\n  checkpoints: {}",
        safe(reference),
        state,
        age_ms.map_or(String::new(), |a| format!("  age: {}", human_age(a))),
        crate::provenance::worktree_label(history.last().map(|o| &o.name)),
        checkpoint_rows.len(),
    );
    for o in &history {
        human.push_str(&format!(
            "\n  observed in {} ({}) on {} — basis {} @{}",
            o.name,
            if o.linked { "linked" } else { "main" },
            o.branch
                .as_ref()
                .map_or_else(|| "(detached)".to_owned(), ToString::to_string),
            o.basis,
            o.first_observed_at,
        ));
    }
    human.push_str("\n  (full detail in --json)");
    Ok(Outcome::ok(human, row))
}

/// The projection row this reference names: through the journal's agent when
/// the journal has the session, else by the recorded native id.
async fn projection_row_id(
    store: &rift_store::SqliteStore,
    journal: Option<&rift_shadow::hooks::session_cache::SessionCache>,
    reference: &str,
) -> Result<Option<String>> {
    let Some(cache) = journal else {
        let bound = store
            .query(
                "SELECT session_id FROM capture_session_identity \
                 WHERE native_session_id = ? LIMIT 1",
                &[rift_store::Value::text(reference.to_owned())],
            )
            .await?;
        return Ok(bound.first().and_then(|r| r.get_text(0).ok()));
    };
    Ok(Some(rift_core::capture_session_row_id(
        &cache.agent,
        reference,
    )))
}

/// The session's checkpoints, newest first.
async fn checkpoints_of(
    store: &rift_store::SqliteStore,
    row_id: &str,
) -> Result<Vec<serde_json::Value>> {
    let checkpoints = store
        .query(
            "SELECT sic.checkpoint_id, COALESCE(c.created_at, 0) \
             FROM session_in_checkpoint sic \
             LEFT JOIN checkpoint c ON c.checkpoint_id = sic.checkpoint_id \
             WHERE sic.session_id = ? ORDER BY COALESCE(c.created_at, 0) DESC",
            &[rift_store::Value::text(row_id.to_owned())],
        )
        .await?;
    Ok(checkpoints
        .iter()
        .filter_map(|r| {
            let id = r.get_text(0).ok()?;
            let at = r.get_i64(1).unwrap_or(0);
            Some(serde_json::json!({ "checkpoint_id": id, "created_at_ms": at }))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rung of the age ladder, at both sides of each boundary. The
    /// coarsening is the point: a session list must not print `3600s`.
    #[test]
    fn the_age_ladder_coarsens_at_each_boundary() {
        for (ms, expected) in [
            (0_i64, "0s"),
            (999, "0s"),
            (1_000, "1s"),
            (59_000, "59s"),
            (60_000, "1m"),
            (3_599_000, "59m"),
            (3_600_000, "1h"),
            (86_399_000, "23h"),
            (86_400_000, "1d"),
            (864_000_000, "10d"),
        ] {
            assert_eq!(human_age(ms), expected, "human_age({ms})");
        }
    }

    #[tokio::test]
    async fn pending_context_bad_legacy_cache_is_unreadable_not_archived() {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let git_dir = tmp.path().join(".git");
        let sessions = crate::family::run_state::sessions_dir(&git_dir);
        let member = sessions.parent().unwrap();
        rift_shadow::hooks::run_state::record_git_dir(member, &git_dir).unwrap();
        rift_shadow::hooks::run_state::record_work_tree(member, tmp.path()).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        let native = "legacy-info-unreadable";
        let old = rift_shadow::hooks::run_state::RunRoot::Sessions.legacy_under(&git_dir);
        std::fs::create_dir_all(&old).unwrap();
        let path = rift_shadow::hooks::session_cache::cache_path(&old, native).unwrap();
        std::fs::write(&path, b"{bad json").unwrap();
        assert!(!rift_shadow::hooks::run_state::is_moved(member));
        assert!(rift_shadow::hooks::session_cache::read_cache(&sessions, native).is_err());
        let store = rift_store::SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        let row_id = rift_core::capture_session_row_id("claude-code", native);
        rift_store::record_session(
            &store,
            &rift_store::SessionRow {
                session_id: row_id.clone(),
                agent: "claude-code".into(),
                model_primary: String::new(),
                repo_id: "repo".into(),
                started_at: 1000,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        rift_store::record_capture_session_identity(
            &store,
            &rift_store::CaptureSessionIdentityRow {
                session_id: row_id,
                agent: "claude-code".into(),
                native_session_id: native.into(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        let outcome = info(&store, tmp.path(), native).await.unwrap();
        assert_eq!(outcome.json["state"], CACHE_UNREADABLE);
    }
}
