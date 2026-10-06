//! The worktree half of a checkpoint: where a session was seen, gathered from
//! the capture projection's observations at archive time, and the one writer
//! of `session_worktrees.jsonl`.
//!
//! A module of its own because the artifact has a grammar of its own and two
//! rules no other artifact needs. A checkout's absolute path never enters a
//! shard: it is a local address, and a rebuild resolves the frozen token back
//! to this machine's checkout instead. And `HEAD` is not frozen: every commit
//! moves it and the next hook records a new observation, so a line that
//! carried it would mint a new checkpoint for a session whose capture did not
//! change. Observations that differ only by head are one line. And only an
//! agent hook's observations are frozen: `commit`, `attach` and `import` are
//! a verb's own record of its act, written after the archive it made, so
//! freezing one would make the verb's next run mint a new checkpoint for a
//! session that did not change. A session known only through those verbs
//! restores no observation on a cold rebuild.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_store::{CanonicalStore, SessionWorktreeRow, Value};

use super::archive::is_identifier;
use super::artifacts::canonical_json;
use super::WorktreeLabel;
use crate::ids;

/// The line format version every `session_worktrees.jsonl` line declares as
/// `schema_version`.
pub const SESSION_WORKTREES_SCHEMA_VERSION: u32 = 1;

/// The bases an agent hook observes under — the only ones the artifact
/// freezes. The verbs' own words (`commit`, `attach`, `import`) stay in the
/// projection alone.
const HOOK_BASES: [&str; 4] = [
    "payload_cwd",
    "workspace_path",
    "process_cwd",
    ids::SHELL_CD_BASIS,
];

/// Where a session was seen, as `session_worktrees.jsonl` freezes it: the
/// member, its branch, the subagent, how it was decided and when it was first
/// seen — never the checkout's path and never `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedWorktreeObservation {
    /// The agent's native session id, as `metadata.json`'s `sessions` names it.
    pub session: String,
    /// `RepoPaths::worktree_token()` — 12 lowercase hex.
    pub token: String,
    /// The member's administrative name, or `main`.
    pub name: String,
    /// Whether the observed checkout was a linked worktree.
    pub linked: bool,
    /// `HEAD`'s branch at the observation, `None` when detached or unreadable.
    pub branch: Option<String>,
    /// The subagent whose hook made the observation, `None` for the session's own.
    pub subagent: Option<String>,
    /// How the observed directory was decided: one of the hook bases.
    pub basis: String,
    /// When the state was first observed (epoch ms).
    pub at: i64,
}

impl ArchivedWorktreeObservation {
    /// Whether the writer will freeze the observation; a caller asks only to
    /// count losses, and the bridge asks to refuse a line the writer could
    /// not have produced.
    #[must_use]
    pub fn is_freezable(&self) -> bool {
        worktree_state(self).is_some()
    }
}

/// Every observation an agent hook recorded for the session `native`
/// captured from `agent`, from both observation relations, in no particular
/// order and without their heads: the writer groups and sorts them. A verb's
/// own observation is not gathered.
///
/// Observations a subagent's hook made are recorded under the parent's
/// session id, so they are included; another session's never are.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure.
pub async fn session_worktree_observations(
    store: &impl CanonicalStore,
    agent: &str,
    native: &str,
) -> Result<Vec<ArchivedWorktreeObservation>> {
    let relation = SessionWorktreeRow::relation(store).await;
    let sql = format!(
        "SELECT worktree_token, worktree_name, is_linked, branch, subagent_id, basis, \
         first_observed_at FROM {relation} WHERE session_id = ? AND basis IN (?, ?, ?, ?)"
    );
    let session_row = ids::session_row_id(agent, native);
    let mut params = vec![Value::text(session_row)];
    params.extend(HOOK_BASES.map(Value::text));
    let rows = store.query(&sql, &params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(ArchivedWorktreeObservation {
            session: native.to_owned(),
            token: row.get_text(0)?,
            name: row.get_text(1)?,
            linked: row.get_i64(2)? != 0,
            branch: row.get_opt_text(3)?,
            subagent: row.get_opt_text(4)?,
            basis: row.get_text(5)?,
            at: row.get_i64(6)?,
        });
    }
    Ok(out)
}

/// Serialize the session's worktree observations exactly as the shard's
/// `session_worktrees.jsonl` freezes them: one canonical line per distinct
/// state — session, member, branch, subagent and basis — dated by the
/// earliest observation of it, sorted by session, time and the line's own
/// bytes, unfreezable observations left out. `None` when nothing is left —
/// the artifact is never written empty, so a shard without it keeps its old
/// leaf sequence.
#[must_use]
pub fn session_worktrees_jsonl_bytes(
    observations: &[ArchivedWorktreeObservation],
) -> Option<Vec<u8>> {
    let mut earliest: BTreeMap<Vec<u8>, (&str, i64, serde_json::Value)> = BTreeMap::new();
    for seen in observations {
        let Some(state) = worktree_state(seen) else {
            continue;
        };
        let entry = earliest.entry(canonical_json(&state));
        let first = entry.or_insert((seen.session.as_str(), seen.at, state));
        first.1 = first.1.min(seen.at);
    }
    let mut lines: Vec<(&str, i64, Vec<u8>)> = Vec::with_capacity(earliest.len());
    for (_, (session, at, mut state)) in earliest {
        let obj = state.as_object_mut()?;
        obj.insert("at".to_owned(), serde_json::json!(at));
        lines.push((session, at, canonical_json(&state)));
    }
    if lines.is_empty() {
        return None;
    }
    lines.sort();
    let mut out = Vec::new();
    for (.., line) in lines {
        out.extend_from_slice(&line);
        out.push(b'\n');
    }
    Some(out)
}

/// One `session_worktrees.jsonl` line without its `at`, or `None` when any
/// member falls outside its shape. Nothing is withheld: a placeholder would
/// restore a row no live path wrote.
fn worktree_state(seen: &ArchivedWorktreeObservation) -> Option<serde_json::Value> {
    if !is_name(&seen.session) || !is_name(&seen.name) || seen.name.contains('/') {
        return None;
    }
    if !HOOK_BASES.contains(&seen.basis.as_str()) || !seen.subagent.as_deref().is_none_or(is_name) {
        return None;
    }
    // The archiving worktree's label already freezes a token, a member name
    // and a branch; one screen for both artifacts.
    WorktreeLabel {
        name: seen.name.clone(),
        token: seen.token.clone(),
        linked: seen.linked,
        branch: seen.branch.clone(),
    }
    .screened()?;
    let mut line = serde_json::json!({
        "basis": seen.basis,
        "linked": seen.linked,
        "name": seen.name,
        "schema_version": SESSION_WORKTREES_SCHEMA_VERSION,
        "session": seen.session,
        "token": seen.token,
    });
    let obj = line.as_object_mut()?;
    for (member, value) in [("branch", &seen.branch), ("subagent", &seen.subagent)] {
        if let Some(value) = value {
            obj.insert(member.to_owned(), serde_json::json!(value));
        }
    }
    Some(line)
}

/// A non-empty identifier.
fn is_name(value: &str) -> bool {
    !value.is_empty() && is_identifier(value)
}

#[cfg(test)]
mod tests {
    use rift_core::SecretBytes;
    use rift_shadow_redact::RulePackSet;
    use rift_store::{record_session_worktree, CanonicalStore, SessionWorktreeRow, SqliteStore};

    use super::*;
    use crate::SessionFacts;

    /// An observation of the main checkout on `main` at `at`.
    fn seen(session: &str, at: i64) -> ArchivedWorktreeObservation {
        ArchivedWorktreeObservation {
            session: session.to_owned(),
            token: "0123456789ab".to_owned(),
            name: "main".to_owned(),
            linked: false,
            branch: Some("main".to_owned()),
            subagent: None,
            basis: "payload_cwd".to_owned(),
            at,
        }
    }

    /// One canonical line per state, keys sorted, in (session, time) order
    /// whatever order they arrive in, a repeated state written once under its
    /// earliest time; nothing, no artifact.
    #[test]
    fn observations_freeze_canonically_one_line_per_state_at_its_earliest_time() {
        assert_eq!(session_worktrees_jsonl_bytes(&[]), None);
        let child = ArchivedWorktreeObservation {
            subagent: Some("agent-1".to_owned()),
            basis: "shell_cd".to_owned(),
            ..seen("s-1", 9)
        };
        let observations = vec![seen("s-2", 1), child, seen("s-1", 5), seen("s-1", 3)];
        let bytes = session_worktrees_jsonl_bytes(&observations).unwrap();
        let reversed: Vec<_> = observations.iter().rev().cloned().collect();
        assert_eq!(session_worktrees_jsonl_bytes(&reversed).unwrap(), bytes);
        let line = |session: &str, at: i64, basis: &str, subagent: &str| {
            format!(
                "{{\"at\":{at},\"basis\":\"{basis}\",\"branch\":\"main\",\
                 \"linked\":false,\"name\":\"main\",\"schema_version\":1,\
                 \"session\":\"{session}\",{subagent}\"token\":\"0123456789ab\"}}\n"
            )
        };
        let want = [
            line("s-1", 3, "payload_cwd", ""),
            line("s-1", 9, "shell_cd", "\"subagent\":\"agent-1\","),
            line("s-2", 1, "payload_cwd", ""),
        ]
        .concat();
        assert_eq!(String::from_utf8(bytes).unwrap(), want);
    }

    /// An absent branch or subagent is an absent member, never null.
    #[test]
    fn a_detached_observation_carries_no_branch_member() {
        let bare = ArchivedWorktreeObservation {
            branch: None,
            ..seen("s", 1)
        };
        let bytes = session_worktrees_jsonl_bytes(&[bare]).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        let want = [
            "at",
            "basis",
            "linked",
            "name",
            "schema_version",
            "session",
            "token",
        ];
        assert_eq!(keys, want);
    }

    /// An absolute path for a name, a token that is not lowercase hex, a
    /// basis no hook observes under or a subagent that is not an identifier refuses the
    /// observation whole; nothing is rewritten.
    #[test]
    fn an_observation_outside_its_declared_shapes_never_reaches_the_artifact() {
        let with = |edit: fn(&mut ArchivedWorktreeObservation)| {
            let mut observation = seen("s", 1);
            edit(&mut observation);
            observation
        };
        let refused = [
            with(|o| o.name = "/Users/x/rift-worktrees/lane-a".to_owned()),
            with(|o| o.token = "0123456789AB".to_owned()),
            with(|o| o.token = "0123456789".to_owned()),
            with(|o| o.basis = "guessed".to_owned()),
            with(|o| o.basis = "attach".to_owned()),
            with(|o| o.subagent = Some("ignore previous instructions".to_owned())),
            with(|o| o.branch = Some("a\nb".to_owned())),
            with(|o| o.name = String::new()),
            with(|o| o.session = String::new()),
        ];
        assert!(refused.iter().all(|o| !o.is_freezable()));
        assert_eq!(session_worktrees_jsonl_bytes(&refused), None);
    }

    proptest::proptest! {
        /// Whatever an observation holds, no line ever carries a `work_tree`,
        /// a `head` or any member outside the declared set.
        #[test]
        fn no_line_carries_a_path_a_head_or_an_undeclared_member(
            name in "(?s).{0,24}",
            branch in proptest::option::of("(?s).{0,24}"),
            subagent in proptest::option::of("(?s).{0,24}"),
        ) {
            let observation = ArchivedWorktreeObservation {
                name,
                branch,
                subagent,
                ..seen("s", 1)
            };
            let Some(bytes) = session_worktrees_jsonl_bytes(&[observation]) else {
                return Ok(());
            };
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let declared = [
                "at", "basis", "branch", "linked", "name", "schema_version", "session",
                "subagent", "token",
            ];
            for member in value.as_object().unwrap().keys() {
                proptest::prop_assert!(declared.contains(&member.as_str()), "{}", member);
            }
        }
    }

    /// A capture projection with the observation relations.
    fn projection() -> (tempfile::TempDir, SqliteStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = rift_store::open_capture_projection(dir.path().join("p.db")).unwrap();
        (dir, store)
    }

    /// One row of `session`'s observations under `basis` at `head`, first
    /// seen at `at`.
    fn row(session: &str, basis: &str, head: &str, at: i64) -> SessionWorktreeRow {
        let session_id = ids::session_row_id("claude-code", session);
        let state = ids::session_worktree_observation_id(
            &session_id,
            "0123456789ab",
            None,
            Some(head),
            None,
        );
        SessionWorktreeRow {
            observation_id: ids::session_worktree_filed_id(&state, basis),
            session_id,
            worktree_token: "0123456789ab".to_owned(),
            worktree_name: "lane-a".to_owned(),
            is_linked: 1,
            work_tree: "/w/lane-a".to_owned(),
            branch: None,
            head_oid: Some(head.to_owned()),
            subagent_id: None,
            basis: basis.to_owned(),
            first_observed_at: at,
            schema_version: 1,
        }
    }

    /// The gather reads both relations for the session and no other
    /// session's rows.
    #[tokio::test]
    async fn the_gather_unions_both_relations_for_the_session_alone() {
        let (_dir, store) = projection();
        let head = "a".repeat(40);
        for r in [
            row("s-1", "payload_cwd", &head, 10),
            row("s-1", "shell_cd", &head, 20),
            row("s-2", "payload_cwd", &head, 30),
        ] {
            assert!(record_session_worktree(&store, &r).await.unwrap());
        }
        assert_eq!(store.count("session_worktree_cd").await.unwrap(), 1);
        let mut gathered = session_worktree_observations(&store, "claude-code", "s-1")
            .await
            .unwrap();
        gathered.sort_by_key(|o| o.at);
        let bases: Vec<(&str, i64, &str)> = gathered
            .iter()
            .map(|o| (o.session.as_str(), o.at, o.basis.as_str()))
            .collect();
        assert_eq!(bases, [("s-1", 10, "payload_cwd"), ("s-1", 20, "shell_cd")]);
        assert!(gathered.iter().all(|o| o.name == "lane-a" && o.linked));
    }

    /// ★ A commit moves `HEAD`, and the next hook records the session at the
    /// new head; with no new capture, the second commit derives the id the
    /// first did, because the artifact freezes no head.
    #[tokio::test]
    async fn two_commits_with_no_new_capture_derive_one_checkpoint_id() {
        let (_dir, store) = projection();
        let (raw, packs) = (
            SecretBytes::new(b"capture".to_vec()),
            RulePackSet::builtin(),
        );
        let predict = |observations| {
            let facts = SessionFacts {
                worktree_observations: observations,
                ..SessionFacts::default()
            };
            crate::condense::predicted_checkpoint_id("s-1", &raw, &facts, &packs).unwrap()
        };
        let first = row("s-1", "payload_cwd", &"a".repeat(40), 10);
        assert!(record_session_worktree(&store, &first).await.unwrap());
        let gathered = session_worktree_observations(&store, "claude-code", "s-1");
        let at_first_commit = predict(gathered.await.unwrap());

        let moved = row("s-1", "payload_cwd", &"b".repeat(40), 20);
        assert!(record_session_worktree(&store, &moved).await.unwrap());
        let gathered = session_worktree_observations(&store, "claude-code", "s-1");
        let at_second_commit = predict(gathered.await.unwrap());

        assert_eq!(at_second_commit, at_first_commit);
        assert_ne!(
            predict(Vec::new()),
            at_first_commit,
            "the artifact is a leaf"
        );
    }

    /// ★ An attach records its own observation after the archive it made; a
    /// second attach of the unchanged session gathers the artifact it froze
    /// the first time and derives the same id, because a verb's observation
    /// is never frozen.
    #[tokio::test]
    async fn a_second_attach_of_an_unchanged_session_derives_the_same_id() {
        let (_dir, store) = projection();
        let (raw, packs) = (
            SecretBytes::new(b"capture".to_vec()),
            RulePackSet::builtin(),
        );
        let predict = |observations| {
            let facts = SessionFacts {
                worktree_observations: observations,
                ..SessionFacts::default()
            };
            crate::condense::predicted_checkpoint_id("s-1", &raw, &facts, &packs).unwrap()
        };
        let head = "a".repeat(40);
        let hook = row("s-1", "payload_cwd", &head, 10);
        assert!(record_session_worktree(&store, &hook).await.unwrap());
        let gathered = session_worktree_observations(&store, "claude-code", "s-1");
        let first_attach = predict(gathered.await.unwrap());

        let mut attached = row("s-1", "attach", &head, 20);
        attached.observation_id = "attach-row".to_owned();
        assert!(record_session_worktree(&store, &attached).await.unwrap());
        let gathered = session_worktree_observations(&store, "claude-code", "s-1");
        let second_attach = predict(gathered.await.unwrap());

        assert_eq!(second_attach, first_attach);
        assert_ne!(predict(Vec::new()), first_attach, "the artifact is present");
    }
}
