//! Worktree attribution for dashboard queries. Unknown attribution is never main.

use rift_store::{CanonicalStore, SqliteStore, Value};
use std::path::Path;
use tokio::runtime::Runtime;

pub(crate) struct WorktreeScope {
    relation: String,
    pub(crate) member: String,
}

impl WorktreeScope {
    pub(crate) fn read(store: &SqliteStore, rt: &Runtime, path: &Path) -> Option<Self> {
        let member = rift_git::discover(path).ok()?.member_name();
        let relation = rt.block_on(rift_store::SessionWorktreeRow::relation(store));
        rt.block_on(store.query(&format!("SELECT 1 FROM {relation} LIMIT 1"), &[]))
            .ok()?;
        Some(Self {
            relation: relation.to_string(),
            member,
        })
    }

    // Column identifiers are module-owned SQL, never user input. Only the member
    // name is data, and every caller binds it through `parameter`.
    pub(crate) fn sessions(&self, column: &str) -> String {
        let relation = &self.relation;
        format!("{column} IN (SELECT w.session_id FROM {relation} w JOIN \
            (SELECT session_id, MAX(first_observed_at) AS latest FROM {relation} GROUP BY session_id) l \
            ON l.session_id=w.session_id AND l.latest=w.first_observed_at \
            GROUP BY w.session_id HAVING MIN(w.worktree_name)=MAX(w.worktree_name) AND MIN(w.worktree_name)=?)")
    }

    pub(crate) fn checkpoints(column: &str) -> String {
        format!("EXISTS (SELECT 1 FROM checkpoint_worktree rift_w WHERE rift_w.checkpoint_id={column} AND rift_w.worktree_name=?)")
    }

    /// Captured events carry native IDs; attribution carries canonical IDs.
    /// A native ID shared by different canonical sessions cannot be attributed.
    fn events(&self, column: &str) -> String {
        let canonical = self.sessions("MIN(c.session_id)");
        format!("{column} IN (SELECT c.native_session_id FROM capture_session_identity c \
            GROUP BY c.native_session_id HAVING MIN(c.session_id)=MAX(c.session_id) AND {canonical})")
    }

    pub(crate) fn capture_rows(&self, table: &str) -> String {
        match table {
            "checkpoint" => Self::checkpoints("checkpoint.checkpoint_id"),
            "captured_event" => self.events("session_id"),
            _ => self.sessions("session_id"),
        }
    }

    pub(crate) fn parameter(&self) -> Value {
        Value::Text(self.member.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (SqliteStore, Runtime, String) {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        let rt = crate::store::runtime().unwrap();
        for id in ["a", "b", "c", "d", "e"] {
            rt.block_on(store.insert(
                "session",
                &[
                    "session_id",
                    "agent",
                    "model_primary",
                    "repo_id",
                    "started_at",
                    "schema_version",
                ],
                &[
                    Value::text(id),
                    Value::text("codex"),
                    Value::text("model"),
                    Value::text("repo"),
                    Value::Int(1),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
        for (id, session, member, at) in [
            ("1", "a", "main", 1),
            ("2", "b", "feature", 1),
            ("3", "c", "main", 2),
            ("4", "c", "feature", 2),
            ("5", "e", "main", 1),
            ("6", "e", "feature", 2),
        ] {
            rt.block_on(store.insert(
                "session_worktree",
                &[
                    "observation_id",
                    "session_id",
                    "worktree_token",
                    "worktree_name",
                    "is_linked",
                    "work_tree",
                    "basis",
                    "first_observed_at",
                    "schema_version",
                ],
                &[
                    Value::text(id),
                    Value::text(session),
                    Value::text("0123456789ab"),
                    Value::text(member),
                    Value::Int(i64::from(member != "main")),
                    Value::text("/fixture"),
                    Value::text("payload_cwd"),
                    Value::Int(at),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
        let relation = rt
            .block_on(rift_store::SessionWorktreeRow::relation(&store))
            .to_owned();
        (store, rt, relation)
    }

    #[test]
    fn scope_excludes_unknown_and_conflicting_observations_and_follows_moves() {
        let (store, rt, relation) = fixture();
        for (member, expected) in [("main", 1), ("feature", 2), ("x' OR 1=1 --", 0)] {
            let scope = WorktreeScope {
                relation: relation.clone(),
                member: member.into(),
            };
            let sql = format!(
                "SELECT COUNT(*) FROM session WHERE {}",
                scope.sessions("session_id")
            );
            assert_eq!(
                rt.block_on(store.query_opt_i64(&sql, &[scope.parameter()]))
                    .unwrap(),
                Some(expected)
            );
        }
    }

    #[test]
    fn native_events_require_unambiguous_canonical_identity_and_attribution() {
        let (store, rt, relation) = fixture();
        for (session, agent, native) in [
            ("a", "codex", "native-a"),
            ("b", "codex", "shared"),
            ("c", "claude-code", "shared"),
            ("d", "codex", "native-d"),
            ("e", "codex", "native-e"),
        ] {
            rt.block_on(store.insert(
                "capture_session_identity",
                &["session_id", "agent", "native_session_id", "schema_version"],
                &[
                    Value::text(session),
                    Value::text(agent),
                    Value::text(native),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
        for (member, expected) in [("main", 1), ("feature", 1), ("x' OR 1=1 --", 0)] {
            let scope = WorktreeScope {
                relation: relation.clone(),
                member: member.into(),
            };
            let sql = format!(
                "SELECT COUNT(*) FROM capture_session_identity WHERE {}",
                scope.events("native_session_id")
            );
            assert_eq!(
                rt.block_on(store.query_opt_i64(&sql, &[scope.parameter()]))
                    .unwrap(),
                Some(expected),
                "native identities must resolve unambiguously before attribution"
            );
        }
    }
}
