//! Which session made a serve: the attribution row beside the two serve
//! relations, and the per-session read a checkpoint freezes its serves from.
//!
//! A module of its own because it is the one place that names a serve's
//! session. Every live writer that knows the session, and the rebuild that
//! reads it back off a frozen line, write the row through
//! [`append_attributed_serve`], and a serve booked before its session was
//! known is attributed by its keys through [`attribute_booked_serve`]; the
//! archive reads the rows back through
//! [`session_serves`]. So a serve's shard is decided by the row, never by
//! which checkpoint happened to run next, and nothing but the store holds it.

use rift_core::Result;
use rift_store::{CanonicalStore, Row, Value};

/// The session that made one serve and, when the writer knew it, the context
/// window inside that session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeAttribution {
    /// The agent's own session id.
    pub session_id: String,
    /// The window journal's one-way key of the window, or `None` where the
    /// writer had no window in hand.
    pub window_key: Option<String>,
}

/// One schema-1 decision row a session made, with every column its frozen
/// line is rebuilt from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributedDecision {
    /// The row's key (`memd_audit_event.audit_id`).
    pub audit_id: String,
    /// The `memd.*` verb.
    pub verb: String,
    /// The handle, or a refusal's target.
    pub handle: Option<String>,
    /// The neutral reason tag.
    pub reason: Option<String>,
    /// What the decision put in front of the model.
    pub tokens_returned: i64,
    /// What it avoided.
    pub tokens_avoided: i64,
    /// When the row was written.
    pub recorded_at: i64,
    /// The window the serve was made in, when known.
    pub window_key: Option<String>,
    /// What the pull's caller claimed to hold, when it named held revisions.
    pub pull_claim: Option<AttributedPullClaim>,
}

/// The `memd_audit_pull_claim` row beside one decision, as its frozen line
/// carries it: counts and the rule's word, never a hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributedPullClaim {
    /// Distinct revisions the caller said it held.
    pub stated: i64,
    /// Of those, the ones this family recorded a read of.
    pub recorded: i64,
    /// Of those, the ones no recorded read accounts for.
    pub unrecorded: i64,
    /// The rule the pull's figure was booked under.
    pub basis: String,
}

/// One schema-4 call row a session made, with every column its frozen line is
/// rebuilt from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributedCall {
    /// The row's key (`serve_call_event.call_id`).
    pub call_id: String,
    /// The serving channel's token.
    pub channel: String,
    /// `<family>:<step>` or the skill verb.
    pub tool_name: String,
    /// The `memd.*` verb.
    pub terminal_verb: String,
    /// What the call put in front of the model.
    pub tokens_returned: i64,
    /// What it avoided.
    pub tokens_avoided: i64,
    /// When the row was written.
    pub recorded_at: i64,
    /// The window the serve was made in, when known.
    pub window_key: Option<String>,
}

/// Everything one session served, in the two relations a serve can live in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionServes {
    /// Its decision rows, ordered by key.
    pub decisions: Vec<AttributedDecision>,
    /// Its call rows, ordered by key.
    pub calls: Vec<AttributedCall>,
}

/// Which relation a serve key addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Relation {
    /// A schema-1 row of `memd_audit_event`.
    Decision,
    /// A schema-4 row of `serve_call_event`.
    Call,
}

impl Relation {
    /// The relation's name, as `memd_audit_session.relation` stores it.
    fn token(self) -> &'static str {
        match self {
            Self::Decision => "memd_audit_event",
            Self::Call => "serve_call_event",
        }
    }

    /// Whether the relation holds a row under `key`.
    fn holds_sql(self) -> &'static str {
        match self {
            Self::Decision => "SELECT COUNT(*) FROM memd_audit_event WHERE audit_id = ?",
            Self::Call => "SELECT COUNT(*) FROM serve_call_event WHERE call_id = ?",
        }
    }
}

/// Append one serve envelope through the one appender and, when a session
/// made it, the row that says which.
///
/// The attribution is written after the serve's own row and only when that
/// row exists, so a declined envelope leaves no orphan; both writes are
/// insert-or-ignore on the serve's key, so a redelivered job or a rebuild that
/// meets the live row converges on one pair rather than two.
///
/// # Errors
///
/// The store's own error when an append or the existence check fails.
pub async fn append_attributed_serve(
    store: &impl CanonicalStore,
    envelope: &[u8],
    now: i64,
    attribution: Option<&ServeAttribution>,
) -> Result<()> {
    crate::audit::append_memd_audit(store, envelope, now).await?;
    let Some(attribution) = attribution else {
        return Ok(());
    };
    record_serve_session(store, envelope, attribution).await
}

/// Attribute a serve some other process already booked, by its two keys.
///
/// The skill CLI books its serve with no session in hand, and names both keys
/// in its answer; the agent hook that ingests that answer is where the session
/// is known. A key that names no row writes nothing, so a spoofed or stale
/// line attributes nothing, and the row is insert-or-ignore on the key, so the
/// first attribution a serve receives is the one it keeps.
///
/// # Errors
///
/// The store's own error when the existence check or the insert fails.
pub async fn attribute_booked_serve(
    store: &impl CanonicalStore,
    audit_id: &str,
    call_id: &str,
    attribution: &ServeAttribution,
) -> Result<()> {
    attribute_key(store, audit_id, Relation::Decision, attribution).await?;
    attribute_key(store, call_id, Relation::Call, attribution).await
}

/// Every serve `session_id` made, from both relations, each ordered by key.
///
/// # Errors
///
/// The store's own error when a query fails.
pub async fn session_serves(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<SessionServes> {
    let session = [Value::text(session_id)];
    let decisions = store
        .query(
            "SELECT e.audit_id, e.verb, e.handle, e.reason, e.tokens_returned, \
             e.tokens_avoided, e.recorded_at, s.window_key, c.stated_count, \
             c.recorded_count, c.unrecorded_count, c.basis \
             FROM memd_audit_event e JOIN memd_audit_session s \
             ON s.serve_key = e.audit_id AND s.relation = 'memd_audit_event' \
             LEFT JOIN memd_audit_pull_claim c ON c.audit_id = e.audit_id \
             WHERE s.session_id = ? ORDER BY e.audit_id",
            &session,
        )
        .await?
        .iter()
        .map(|row| {
            Ok(AttributedDecision {
                audit_id: row.get_text(0)?,
                verb: row.get_text(1)?,
                handle: row.get_opt_text(2)?,
                reason: row.get_opt_text(3)?,
                tokens_returned: row.get_i64(4)?,
                tokens_avoided: row.get_i64(5)?,
                recorded_at: row.get_i64(6)?,
                window_key: row.get_opt_text(7)?,
                pull_claim: pull_claim_at(row, 8)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let calls = session_calls(store, &session).await?;
    Ok(SessionServes { decisions, calls })
}

/// The call half of [`session_serves`].
async fn session_calls(
    store: &impl CanonicalStore,
    session: &[Value],
) -> Result<Vec<AttributedCall>> {
    store
        .query(
            "SELECT c.call_id, c.channel, c.tool_name, c.terminal_verb, c.tokens_returned, \
             c.tokens_avoided, c.recorded_at, s.window_key \
             FROM serve_call_event c JOIN memd_audit_session s \
             ON s.serve_key = c.call_id AND s.relation = 'serve_call_event' \
             WHERE s.session_id = ? ORDER BY c.call_id",
            session,
        )
        .await?
        .iter()
        .map(|row| {
            Ok(AttributedCall {
                call_id: row.get_text(0)?,
                channel: row.get_text(1)?,
                tool_name: row.get_text(2)?,
                terminal_verb: row.get_text(3)?,
                tokens_returned: row.get_i64(4)?,
                tokens_avoided: row.get_i64(5)?,
                recorded_at: row.get_i64(6)?,
                window_key: row.get_opt_text(7)?,
            })
        })
        .collect()
}

/// The claim a LEFT JOIN put in the four columns from `at`, or `None` where
/// the decision has no claim row and all four are NULL.
fn pull_claim_at(row: &Row, at: usize) -> Result<Option<AttributedPullClaim>> {
    let Some(basis) = row.get_opt_text(at + 3)? else {
        return Ok(None);
    };
    Ok(Some(AttributedPullClaim {
        stated: row.get_i64(at)?,
        recorded: row.get_i64(at + 1)?,
        unrecorded: row.get_i64(at + 2)?,
        basis,
    }))
}

/// Write the attribution row of the serve `envelope` booked, if it booked one.
async fn record_serve_session(
    store: &impl CanonicalStore,
    envelope: &[u8],
    attribution: &ServeAttribution,
) -> Result<()> {
    let Some((key, relation)) = serve_key_of(envelope) else {
        return Ok(());
    };
    attribute_key(store, &key, relation, attribution).await
}

/// Write the attribution row of the serve `key` addresses in `relation`, when
/// that relation holds it.
async fn attribute_key(
    store: &impl CanonicalStore,
    key: &str,
    relation: Relation,
    attribution: &ServeAttribution,
) -> Result<()> {
    let held = store
        .query_opt_i64(relation.holds_sql(), &[Value::text(key)])
        .await?;
    if held.unwrap_or(0) == 0 {
        return Ok(());
    }
    store
        .insert_or_ignore(
            "memd_audit_session",
            &[
                "serve_key",
                "relation",
                "session_id",
                "window_key",
                "schema_version",
            ],
            &[
                Value::text(key),
                Value::text(relation.token()),
                Value::text(&attribution.session_id),
                attribution
                    .window_key
                    .as_deref()
                    .map_or(Value::Null, Value::text),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(())
}

/// The key an envelope's row is addressed by, and the relation it lands in;
/// `None` for an envelope that books neither serve relation.
fn serve_key_of(envelope: &[u8]) -> Option<(String, Relation)> {
    let env = serde_json::from_slice::<serde_json::Value>(envelope).ok()?;
    let key = env.get("idempotency_key")?.as_str()?.to_owned();
    let relation = match env.get("schema").and_then(serde_json::Value::as_u64) {
        Some(4) => Relation::Call,
        Some(1) | None => Relation::Decision,
        Some(_) => return None,
    };
    Some((key, relation))
}

#[cfg(test)]
mod tests {
    use rift_recall_events::{MemdEvent, ServeChannel};
    use rift_store::{apply_migrations, recall_migrations, SqliteStore};

    use super::*;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&store, &recall_migrations()).unwrap();
        store
    }

    fn capped(n: u32) -> MemdEvent {
        MemdEvent::GrepCapped {
            handle: format!("grep:matches={n}"),
        }
    }

    fn attributed(session: &str, window: Option<&str>) -> ServeAttribution {
        ServeAttribution {
            session_id: session.to_owned(),
            window_key: window.map(str::to_owned),
        }
    }

    /// Book one decision's pair under `session`, as the worker does.
    fn book(store: &SqliteStore, event: &MemdEvent, call: &str, session: &ServeAttribution) {
        let audited = event.to_hook_json_keyed(call);
        let called = event.to_serve_call_json(
            ServeChannel::HookInject,
            call,
            "claude-code:search-gate",
            rift_core::Timestamp::from_millis(1_700_000_000_000),
        );
        for envelope in [audited, called] {
            block(append_attributed_serve(
                store,
                envelope.as_bytes(),
                1_700_000_000_000,
                Some(session),
            ))
            .unwrap();
        }
    }

    /// Two sessions interleaving their serves: each reads back exactly its
    /// own, in both relations, and the order they arrived in moves nothing.
    #[test]
    fn interleaved_sessions_read_back_only_their_own_serves() {
        let store = recall_store();
        let a = attributed("s-a", Some(&"a".repeat(64)));
        let b = attributed("s-b", None);
        book(&store, &capped(1), "call-1", &a);
        book(&store, &capped(2), "call-2", &b);
        book(&store, &capped(3), "call-3", &a);

        let mine = block(session_serves(&store, "s-a")).unwrap();
        let calls: Vec<&str> = mine.calls.iter().map(|c| c.call_id.as_str()).collect();
        assert_eq!(calls, ["call-1", "call-3"]);
        assert_eq!(mine.decisions.len(), 2);
        assert!(mine
            .decisions
            .iter()
            .all(|d| d.window_key.as_deref() == Some(&*"a".repeat(64))));
        let theirs = block(session_serves(&store, "s-b")).unwrap();
        assert_eq!(theirs.calls.len(), 1);
        assert_eq!(theirs.calls[0].window_key, None);
        assert_eq!(
            theirs.decisions[0].handle.as_deref(),
            Some("grep:matches=2")
        );
    }

    /// A pull's claim row rides with its decision, and a decision with no
    /// claim row reads back none.
    #[test]
    fn a_decision_reads_back_its_pull_claim_and_one_without_reads_none() {
        use rift_recall_events::{PullClaim, PullClaimBasis};
        let store = recall_store();
        let session = attributed("s-a", None);
        let pull = MemdEvent::ManifestServed {
            estimated_tokens_returned: 3,
            estimated_tokens_avoided: 4,
            pull_claim: None,
        }
        .with_pull_claim(PullClaim::new(4, 1, PullClaimBasis::Unverified));
        book(&store, &pull, "call-pull", &session);
        book(&store, &capped(1), "call-grep", &session);

        let claims: Vec<(String, Option<AttributedPullClaim>)> =
            block(session_serves(&store, "s-a"))
                .unwrap()
                .decisions
                .into_iter()
                .map(|d| (d.verb, d.pull_claim))
                .collect();
        let claimed = AttributedPullClaim {
            stated: 4,
            recorded: 1,
            unrecorded: 3,
            basis: "unverified".to_owned(),
        };
        assert!(claims.contains(&("memd.manifest_served".to_owned(), Some(claimed))));
        assert!(claims.contains(&("memd.grep_capped".to_owned(), None)));
        assert_eq!(claims.len(), 2);
    }

    /// A serve no session made is an ordinary row with no attribution, and no
    /// session's read ever returns it.
    #[test]
    fn an_unattributed_serve_is_a_row_no_session_reads_back() {
        let store = recall_store();
        let envelope = capped(9).to_hook_json_keyed("call-cli");
        block(append_attributed_serve(
            &store,
            envelope.as_bytes(),
            1,
            None,
        ))
        .unwrap();
        assert_eq!(block(store.count("memd_audit_event")).unwrap(), 1);
        assert_eq!(block(store.count("memd_audit_session")).unwrap(), 0);
        assert_eq!(
            block(session_serves(&store, "s-a")).unwrap(),
            SessionServes::default()
        );
    }

    /// An envelope the appender declines leaves no attribution behind, and a
    /// redelivered one leaves one.
    #[test]
    fn a_declined_envelope_leaves_no_orphan_and_a_redelivered_one_leaves_one_row() {
        let store = recall_store();
        let session = attributed("s-a", None);
        for envelope in [
            "not json",
            r#"{"schema":4,"verb":"memd.x","idempotency_key":"k"}"#,
        ] {
            block(append_attributed_serve(
                &store,
                envelope.as_bytes(),
                1,
                Some(&session),
            ))
            .unwrap();
        }
        assert_eq!(block(store.count("memd_audit_session")).unwrap(), 0);
        book(&store, &capped(4), "call-4", &session);
        book(&store, &capped(4), "call-4", &session);
        assert_eq!(block(store.count("memd_audit_session")).unwrap(), 2);
    }

    /// A serve booked with no session is attributed by its two keys to the
    /// session that names them first; a later claim moves nothing, and a key
    /// that names no row writes nothing.
    #[test]
    fn a_booked_serve_keeps_its_first_attribution_and_a_missing_key_attributes_nothing() {
        let store = recall_store();
        let event = capped(5);
        let audited = event.to_hook_json_keyed("call-5");
        let called = event.to_serve_call_json(
            ServeChannel::SkillCli,
            "call-5",
            "known-state",
            rift_core::Timestamp::from_millis(1_700_000_000_000),
        );
        for envelope in [&audited, &called] {
            block(append_attributed_serve(
                &store,
                envelope.as_bytes(),
                1,
                None,
            ))
            .unwrap();
        }
        let audit_id = serve_key_of(audited.as_bytes()).unwrap().0;
        let first = attributed("s-a", Some(&"a".repeat(64)));
        block(attribute_booked_serve(
            &store,
            &"f".repeat(64),
            "call-none",
            &first,
        ))
        .unwrap();
        assert_eq!(block(store.count("memd_audit_session")).unwrap(), 0);

        block(attribute_booked_serve(&store, &audit_id, "call-5", &first)).unwrap();
        block(attribute_booked_serve(
            &store,
            &audit_id,
            "call-5",
            &attributed("s-b", None),
        ))
        .unwrap();
        let mine = block(session_serves(&store, "s-a")).unwrap();
        assert_eq!(mine.decisions.len(), 1);
        assert_eq!(mine.calls.len(), 1);
        assert_eq!(mine.calls[0].window_key.as_deref(), Some(&*"a".repeat(64)));
        assert_eq!(
            block(session_serves(&store, "s-b")).unwrap(),
            SessionServes::default()
        );
    }
}
