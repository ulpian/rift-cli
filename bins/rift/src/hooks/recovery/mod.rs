//! The post-compaction recovery map (injection tier c). The one moment a full
//! listing of what a session holds pays for itself: its context has just been
//! summarised or rebuilt, it no longer holds the bytes it read, and it is
//! about to read them all again. Everywhere else the same listing is spend —
//! a quiet turn is answered with silence, and that property is untouched by
//! anything here.

mod announce;
mod claim;
mod entries;

use rift_shadow::hooks::AdapterRegistry;

use super::agent::{boundary_budget, boundary_ceiling};
use super::now_ms;
use announce::{announce_recovery, fit_recovery_map};
use claim::{claim_recovery, record_recovery_injection};
use entries::{recovery_map_entries, RecoveryEntry};

pub(super) use announce::announce_holds;
pub(crate) use entries::basis_snapshots_at;

/// What the cheap gates found, when they found a boundary a map could travel
/// on: the hook itself, the budget it has, the journal it was decided from and
/// the set that window stopped holding.
///
/// A context struct rather than six returns, because every step after the
/// gates needs a different three of them and the order they were established
/// in is the order they are needed in.
struct RecoveryOpening<'cfg> {
    raw: rift_shadow::hooks::RawHook,
    engine: rift_recall_policy::PolicyEngine<'cfg>,
    budget: u32,
    journal: crate::window_journal::WindowJournal,
    window: Option<String>,
    held: Vec<rift_agent::window_journal::HeldFile>,
}

/// The map a context that has just lost its state is handed, or `None` when
/// there is nothing to hand it.
///
/// Four steps, each of which may end the turn in silence: the gates that
/// decide there is a boundary at all, the entries that survive the guard and
/// the caps, the fit into the operator's budget, and the once-per-generation
/// claim. Nothing is booked and nothing is announced until the text exists.
///
/// Unlike the gate path, this one may open a store and may use the tokenizer:
/// it runs at a turn boundary, not in front of a tool call.
pub(super) fn recovery_map(
    repo: &std::path::Path,
    cfg: &rift_config::EffectiveConfig,
    agent: &rift_core::AgentId,
    verb: &str,
    body: &[u8],
) -> Option<String> {
    let opening = recovery_opening(repo, cfg, agent, verb, body)?;
    let (entries, omitted) = recovery_map_entries(repo, &opening.engine, &opening.held)?;
    let rendered: Vec<rift_recall_serve::HeldEntry<'_>> =
        entries.iter().map(RecoveryEntry::rendered).collect();
    let (text, named) = fit_recovery_map(&rendered, omitted, opening.budget)?;
    // Claimed only once the text exists: a map that could not be fitted is
    // still owed, and the next boundary hook of this generation may fit it.
    if !claim_recovery(
        repo,
        &opening.raw.session,
        opening.window.as_deref(),
        &opening.held,
    ) {
        return None;
    }
    record_recovery_injection(
        repo,
        agent,
        &opening.raw.session,
        opening.window.as_deref(),
        &text,
    );
    announce_recovery(
        &opening.journal,
        &opening.engine,
        &opening.raw,
        &entries,
        &named,
    );
    Some(text)
}

/// Everything the gates decide before a store is opened, or `None` at the
/// first one that closes.
///
/// The order they are asked in is the cost order: the verb and the source are
/// a string comparison, the budget is a policy file, and the journal is a
/// bounded file read. A fresh session stops at the first gate; a session that
/// never lost anything stops at the last.
fn recovery_opening<'cfg>(
    repo: &std::path::Path,
    cfg: &'cfg rift_config::EffectiveConfig,
    agent: &rift_core::AgentId,
    verb: &str,
    body: &[u8],
) -> Option<RecoveryOpening<'cfg>> {
    if !recovery_hook(agent, verb) {
        return None;
    }
    let raw = rift_shadow::hooks::build_raw_hook(agent.as_str(), verb, body, now_ms()).ok()?;
    if !recovery_verb(agent, verb, &raw.payload) || raw.session.is_empty() {
        return None;
    }
    let policy = rift_recall_policy::ServePolicy::load(&repo.join(".rift"));
    let budget = boundary_budget(
        rift_recall_policy::resolve(
            &policy,
            agent.as_str(),
            crate::recall::family_capability(agent),
        )
        .recovery_budget,
        boundary_ceiling(agent, verb),
    );
    if budget == 0 {
        return None;
    }
    let window = recovery_window(agent, verb, &raw);
    let journal = crate::window_journal::WindowJournal::open(repo, &raw.session, window.as_deref());
    let loaded = journal.load().journal;
    if !recovery_follows_the_boundary(agent, last_eviction(&loaded)) {
        return None;
    }
    let held = loaded.held_before_eviction();
    if held.is_empty() {
        // A session that lost nothing it had read — a fresh one, or one whose
        // reads Rift never saw — is told nothing. There is no honest listing to
        // give it, and the pointer is already its introduction.
        return None;
    }
    Some(RecoveryOpening {
        raw,
        engine: rift_recall_policy::PolicyEngine::new(cfg).with_serve_policy(policy),
        budget,
        journal,
        window,
        held,
    })
}

/// Whether this `(family, verb)` is a hook a map could ever travel on —
/// decided before the payload is even parsed.
///
/// An ordinary turn is the common case by a wide margin, and it must cost this
/// path nothing: the family's boundary verbs are a string comparison, and a
/// verb that is not one of them is answered before anything is read, parsed or
/// hashed.
fn recovery_hook(agent: &rift_core::AgentId, verb: &str) -> bool {
    if *agent == rift_core::AgentId::CLAUDE_CODE {
        return verb == "SessionStart";
    }
    if *agent == rift_core::AgentId::CODEX {
        return matches!(verb, "SessionStart" | "UserPromptSubmit");
    }
    if *agent == rift_core::AgentId::ANTIGRAVITY {
        return verb == "PreInvocation";
    }
    false
}

/// Whether this `(family, verb, source)` could be the hook that meets a context
/// which has lost its state — decided before anything is read from disk.
///
/// Claude Code names the reason a session started, and two of those names mean
/// the conversation was rebuilt from a summary or a transcript rather than
/// begun. Codex publishes its compaction on both sides of the summary but names
/// no source, so its boundary is "the next hook that can carry text", which the
/// journal's own last signal decides ([`recovery_follows_the_boundary`]).
///
/// Antigravity publishes no compaction signal at all. What it publishes is a
/// counter, and its first invocation is the only execution boundary it names —
/// so the map rides that, and what decides whether there is a map to give is
/// the same thing that decides it everywhere else: whether the window's journal
/// holds a boundary with reads before it. A family that never announces one
/// therefore rarely has a map to be handed, which is the honest position: an
/// execution boundary is not a statement that the context was rebuilt, and
/// reading it as one would also re-arm declines over coverage nobody said was
/// lost.
fn recovery_verb(agent: &rift_core::AgentId, verb: &str, payload: &serde_json::Value) -> bool {
    if !recovery_hook(agent, verb) {
        return false;
    }
    if *agent == rift_core::AgentId::CLAUDE_CODE {
        return matches!(hook_field(payload, "source"), Some("compact" | "resume"));
    }
    if *agent == rift_core::AgentId::ANTIGRAVITY {
        return hook_number(payload, "invocationNum") == Some(0);
    }
    true
}

/// Whether the window's own last signal says this hook follows a boundary.
///
/// For the family whose verb already said so, the journal only has to have a
/// boundary at all. For the family whose boundary is announced on a different
/// hook, the last signal must be the one that closes a compaction — otherwise
/// this is an ordinary turn, and an ordinary turn is answered by the manifest
/// or by silence.
fn recovery_follows_the_boundary(
    agent: &rift_core::AgentId,
    last: Option<rift_agent::window_journal::EvictReason>,
) -> bool {
    if *agent == rift_core::AgentId::CODEX {
        return last == Some(rift_agent::window_journal::EvictReason::PostCompact);
    }
    last.is_some()
}

/// The context window this hook belongs to, where the family names one.
///
/// The window, not the session: it is the key the gate wrote the journal
/// under, so a subagent is handed its own window's map and never its parent's.
fn recovery_window(
    agent: &rift_core::AgentId,
    verb: &str,
    raw: &rift_shadow::hooks::RawHook,
) -> Option<String> {
    AdapterRegistry::with_builtin()
        .get(agent)
        .map(|adapter| adapter.facts(verb, &raw.payload))
        .and_then(|facts| facts.child_session)
}

/// The window's own last eviction signal, which is what says a boundary
/// happened at all.
fn last_eviction(
    journal: &rift_agent::window_journal::Journal,
) -> Option<rift_agent::window_journal::EvictReason> {
    journal.lines().iter().rev().find_map(|line| match line {
        rift_agent::window_journal::Line::Evict { reason, .. } => Some(*reason),
        _ => None,
    })
}

/// The same field, where the family writes a number rather than a string.
pub(super) fn hook_number(payload: &serde_json::Value, key: &str) -> Option<u64> {
    payload
        .get("payload")
        .and_then(|inner| inner.get(key))
        .or_else(|| payload.get(key))
        .and_then(serde_json::Value::as_u64)
}

/// A field of a hook payload, whichever of the two accepted shapes it arrived
/// in: the native object as the family sends it, or the envelope that carries
/// that object under `payload`. The same two shapes the wire parser accepts.
fn hook_field<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload
        .get("payload")
        .and_then(|inner| inner.get(key))
        .or_else(|| payload.get(key))
        .and_then(serde_json::Value::as_str)
}

// Declared at the foot rather than at the head: the CI gates cut a file's
// production half at its first column-zero `#[cfg(test)]`, and everything
// above this line is production.
#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::fixtures::{
        compacted, drop_family_store, evict, read_and_hold, recall_repo, CC, START,
    };
    use super::*;
    use crate::hooks::agent::{
        boundary_is_open, hook_degradation, hook_health_path, record_hook_health_in,
        session_pointer_in,
    };
    use crate::hooks::worker::session_start_hook;

    /// **What the map said, written down.** A recovery map that names a file
    /// tells the window it holds it, and the window's own journal records the
    /// announcement — which is what a later act on that file is booked
    /// against. Only the files the map named: a path it did not carry is a
    /// count, not a fact the model was given.
    #[test]
    fn a_recovery_map_records_what_it_told_the_window_it_holds() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        let session = "s-announced";
        read_and_hold(&repo, session, "a.txt", "alpha\nbravo\n", 10);
        read_and_hold(&repo, session, "b.txt", "charlie\n", 20);
        evict(&repo, session, 40);

        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let map = recovery_map(&repo, &cfg, &CC, START, &compacted(session)).expect("a map");
        let journal = crate::window_journal::WindowJournal::open(&repo, session, None)
            .load()
            .journal;
        let mut announced: Vec<(String, u32, u32)> = journal
            .lines()
            .iter()
            .filter_map(|line| match line {
                rift_agent::window_journal::Line::Hold { path, range, .. } => {
                    Some((path.clone(), range.lo, range.hi))
                }
                _ => None,
            })
            .collect();
        announced.sort();
        assert_eq!(
            announced,
            vec![("a.txt".to_owned(), 1, 2), ("b.txt".to_owned(), 1, 1),],
            "the map named both files: {map}"
        );
        for (path, ..) in &announced {
            assert!(
                journal.standing_hold(path).is_some(),
                "{path} is what a later act is weighed against"
            );
        }
    }

    /// **The ticket's claim, end to end through the one entry point.** A
    /// session that read three files and was then compacted is handed the map
    /// of what it stopped holding — once — inside the operator's budget, and
    /// the map's cost is booked.
    ///
    /// Once is the load-bearing half. The same boundary hook can arrive twice
    /// (a retry, a crash between the write and the answer, two hooks for one
    /// compaction), and a map handed twice is a second wall of paths in a
    /// context that has just been squeezed.
    #[test]
    fn a_compacted_session_receives_its_held_set_once_within_the_recovery_budget() {
        use rift_store::CanonicalStore as _;

        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        let session = "s-compacted";
        read_and_hold(&repo, session, "a.txt", "alpha\nbravo\n", 10);
        read_and_hold(&repo, session, "b.txt", "charlie\n", 20);
        read_and_hold(&repo, session, "c.txt", "delta\n", 30);
        evict(&repo, session, 40);

        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let map = recovery_map(&repo, &cfg, &CC, START, &compacted(session))
            .expect("a compacted window is handed its map");
        for path in ["a.txt", "b.txt", "c.txt"] {
            assert!(map.contains(path), "{path} is missing from {map}");
        }
        assert!(
            map.contains("unchanged since your read"),
            "nothing moved, so every entry says so: {map}"
        );
        assert!(
            map.contains("rift recall changed-hunks"),
            "the way out of a re-read is a runnable command: {map}"
        );
        let budget = rift_recall_policy::resolve(
            &rift_recall_policy::ServePolicy::load(&repo.join(".rift")),
            CC.as_str(),
            crate::recall::family_capability(&CC),
        )
        .recovery_budget;
        assert!(
            rift_tokens::count(&map) <= budget,
            "the map cost {} of a {budget}-token budget",
            rift_tokens::count(&map)
        );

        assert_eq!(
            recovery_map(&repo, &cfg, &CC, START, &compacted(session)),
            None,
            "one generation, one map"
        );
        // The boundary hook writes an eviction of its own, so a redelivery
        // arrives with a *new* last signal over the very same held set. That
        // must still be one map: the claim is named for what the map says, not
        // for the signal that asked for it.
        evict(&repo, session, 50);
        assert_eq!(
            recovery_map(&repo, &cfg, &CC, START, &compacted(session)),
            None,
            "a second eviction over the same held set is not a second map"
        );

        // What it cost is booked, in the relation the honesty gate nets.
        let db = crate::family::recall_db(&repo).unwrap();
        let store = rift_store::open_recall_projection(&db).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let rows = rt
            .block_on(store.query(
                "SELECT tokens_returned, tokens_avoided FROM memd_audit_event \
                 WHERE verb = 'memd.recovery_injected'",
                &[],
            ))
            .unwrap();
        assert_eq!(rows.len(), 1, "one map, one row");
        assert_eq!(
            u32::try_from(rows[0].get_i64(0).unwrap()).unwrap(),
            rift_tokens::count(&map),
            "the row carries what the map cost"
        );
        assert_eq!(
            rows[0].get_i64(1).unwrap(),
            0,
            "a map avoids nothing by itself"
        );
        drop(store);
        drop_family_store(&repo);
    }

    /// A session that never lost anything is handed no map — it is handed the
    /// pointer, which is what a session that has just begun is owed.
    #[test]
    fn a_fresh_session_receives_no_map() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();

        let startup = serde_json::json!({ "session_id": "s-fresh", "source": "startup" })
            .to_string()
            .into_bytes();
        assert_eq!(
            recovery_map(&repo, &cfg, &CC, START, &startup),
            None,
            "a session that started is not a session that was compacted"
        );
        // And even on the boundary source, a window with no reads behind it has
        // nothing to be told.
        assert_eq!(
            recovery_map(&repo, &cfg, &CC, START, &compacted("s-fresh")),
            None
        );
        // Through the one entry point, that turn is the pointer's.
        let pointer = session_pointer_in(&repo, &CC, START, &startup)
            .expect("a fresh session still gets its pointer");
        assert!(pointer.contains("rift recall"), "{pointer}");
        drop_family_store(&repo);
    }

    /// **A map is a serve its session made.** Both of its rows are written
    /// with the attribution that names the session and the window it was
    /// handed to, which is what the session's next checkpoint freezes; a map
    /// written unattributed would be the one serve no shard carries.
    #[test]
    fn a_recovery_maps_rows_name_the_session_and_window_it_was_handed_to() {
        use rift_store::CanonicalStore as _;

        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        let session = "s-attributed";
        read_and_hold(&repo, session, "a.txt", "alpha\n", 10);
        evict(&repo, session, 40);

        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        recovery_map(&repo, &cfg, &CC, START, &compacted(session)).expect("a map");

        let db = crate::family::recall_db(&repo).unwrap();
        let store = rift_store::open_recall_projection(&db).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let rows = rt
            .block_on(store.query(
                "SELECT relation, session_id, window_key FROM memd_audit_session \
                 ORDER BY relation",
                &[],
            ))
            .unwrap();
        let attributed: Vec<(String, String, String)> = rows
            .iter()
            .map(|row| {
                (
                    row.get_text(0).unwrap(),
                    row.get_text(1).unwrap(),
                    row.get_text(2).unwrap(),
                )
            })
            .collect();
        let window = crate::window_journal::key(session, None);
        assert_eq!(
            attributed,
            vec![
                (
                    "memd_audit_event".to_owned(),
                    session.to_owned(),
                    window.clone()
                ),
                ("serve_call_event".to_owned(), session.to_owned(), window),
            ],
            "the decision row and the call row both name the session and window"
        );
        drop(store);
        drop_family_store(&repo);
    }

    /// **A hook that fires before every model invocation speaks once per
    /// execution.** The counter the family puts in its own payload is the whole
    /// of the rule, and everything that is not plainly its first value is an
    /// ordinary delivery: a later invocation, an unnumbered payload, bytes that
    /// are not JSON at all. A pointer repeated through a tool loop is the one
    /// way an injection stops being a saving.
    ///
    /// Swept over the other families too, because this gate must be invisible
    /// to them: their boundary verbs fire once a turn already, and a payload
    /// question asked of them would be a second rule nobody wrote down.
    #[test]
    fn a_per_invocation_boundary_speaks_only_at_its_first_invocation() {
        let agy = rift_core::AgentId::ANTIGRAVITY;
        let body = |json: serde_json::Value| json.to_string().into_bytes();

        assert!(boundary_is_open(
            &agy,
            "PreInvocation",
            &body(serde_json::json!({"conversationId": "c", "invocationNum": 0}))
        ));
        // The same fact wrapped in the envelope the wire parser also accepts.
        assert!(boundary_is_open(
            &agy,
            "PreInvocation",
            &body(serde_json::json!({"payload": {"invocationNum": 0}}))
        ));
        for later in [1_u64, 2, 40] {
            assert!(
                !boundary_is_open(
                    &agy,
                    "PreInvocation",
                    &body(serde_json::json!({"invocationNum": later}))
                ),
                "invocation {later} is inside the loop"
            );
        }
        for unreadable in [
            serde_json::json!({}),
            serde_json::json!({"invocationNum": "0"}),
            serde_json::json!({"invocationNum": -1}),
            serde_json::json!([0]),
        ] {
            assert!(
                !boundary_is_open(&agy, "PreInvocation", &body(unreadable.clone())),
                "{unreadable} names no first invocation"
            );
        }
        assert!(
            !boundary_is_open(&agy, "PreInvocation", b"not json at all"),
            "an unreadable delivery costs a pointer, never a repetition"
        );

        // Every other family and verb passes through untouched, including the
        // verbs this family is answered on but never injected through.
        for verb in ["PreToolUse", "PostInvocation", "Stop"] {
            assert!(boundary_is_open(&agy, verb, b"not json at all"), "{verb}");
        }
        for verb in [START, "UserPromptSubmit", "PreToolUse"] {
            assert!(boundary_is_open(&CC, verb, b""), "{verb}");
            assert!(
                boundary_is_open(&rift_core::AgentId::CODEX, verb, b""),
                "{verb}"
            );
        }
    }

    /// A proactive document pass is asked for once per session, on each
    /// family's own way of saying a session has begun — and never on an
    /// ordinary turn, which would ask for the same walk on every invocation
    /// of a loop.
    #[test]
    fn only_the_beginning_of_a_session_asks_for_a_document_pass() {
        let agy = rift_core::AgentId::ANTIGRAVITY;
        let codex = rift_core::AgentId::CODEX;
        let none = serde_json::json!({});

        for family in [CC, codex] {
            assert!(session_start_hook(&family, START, &none));
            for verb in ["UserPromptSubmit", "PreToolUse", "Stop", "SessionEnd"] {
                assert!(!session_start_hook(&family, verb, &none), "{verb}");
            }
        }

        // The family that names no session start: its zeroth invocation is the
        // one execution boundary it publishes, read exactly as the recovery
        // map reads it.
        assert!(session_start_hook(
            &agy,
            "PreInvocation",
            &serde_json::json!({"invocationNum": 0})
        ));
        for later in [serde_json::json!({"invocationNum": 3}), none.clone()] {
            assert!(
                !session_start_hook(&agy, "PreInvocation", &later),
                "{later} is not the beginning of anything"
            );
        }
        assert!(!session_start_hook(&agy, START, &none), "not its verb");
    }

    /// The boundary is the family's to announce, and each of the three
    /// announces a different thing. Stated on the pure predicates, swept over
    /// every verb, source and counter the alpha families send, because this is
    /// the gate that keeps a full listing off an ordinary turn.
    #[test]
    fn only_a_families_own_boundary_signal_asks_for_a_map() {
        use rift_agent::window_journal::EvictReason;
        let agy = rift_core::AgentId::ANTIGRAVITY;
        let codex = rift_core::AgentId::CODEX;

        let source = |name: &str| serde_json::json!({ "source": name });
        let invocation = |num: u64| serde_json::json!({ "invocationNum": num });

        // An ordinary turn does not reach the payload at all.
        assert!(recovery_hook(&CC, START));
        assert!(!recovery_hook(&CC, "UserPromptSubmit"));
        assert!(recovery_hook(&agy, "PreInvocation"));
        assert!(recovery_hook(&codex, "UserPromptSubmit"));

        assert!(recovery_verb(&CC, START, &source("compact")));
        assert!(recovery_verb(&CC, START, &source("resume")));
        for name in ["startup", "clear", "fork"] {
            assert!(
                !recovery_verb(&CC, START, &source(name)),
                "{name} is not a rebuilt context"
            );
        }
        assert!(
            !recovery_verb(&CC, START, &serde_json::json!({})),
            "an unnamed source is not a rebuilt context"
        );
        assert!(
            !recovery_verb(&CC, "UserPromptSubmit", &source("compact")),
            "an ordinary turn is never a boundary — the silence property"
        );
        // The family with no compaction signal: its execution boundary is the
        // first invocation, and every later one in the same loop is an
        // ordinary turn however the payload is spelled.
        assert!(recovery_verb(&agy, "PreInvocation", &invocation(0)));
        for num in [1, 2, 17] {
            assert!(
                !recovery_verb(&agy, "PreInvocation", &invocation(num)),
                "invocation {num} is inside the loop, not at its boundary"
            );
        }
        assert!(
            !recovery_verb(&agy, "PreInvocation", &source("compact")),
            "an unnumbered delivery is not a first invocation"
        );
        for verb in [START, "UserPromptSubmit", "PreToolUse", "PostInvocation"] {
            assert!(!recovery_verb(&agy, verb, &invocation(0)), "{verb}");
        }
        // The family that announces on a different hook: the verb is a
        // candidate, and its own last signal decides.
        assert!(recovery_verb(
            &codex,
            "UserPromptSubmit",
            &serde_json::json!({})
        ));
        assert!(recovery_follows_the_boundary(
            &codex,
            Some(EvictReason::PostCompact)
        ));
        for reason in [
            None,
            Some(EvictReason::PreCompact),
            Some(EvictReason::Clear),
        ] {
            assert!(
                !recovery_follows_the_boundary(&codex, reason),
                "{reason:?} does not close a compaction"
            );
        }
        assert!(!recovery_follows_the_boundary(&CC, None));
        assert!(recovery_follows_the_boundary(
            &CC,
            Some(EvictReason::Compact)
        ));
    }

    /// A path this repository does not serve is not served by a new renderer
    /// either — it is left out and counted, exactly as every other serve leaves
    /// it out.
    #[test]
    fn a_denied_path_is_left_out_and_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        std::fs::create_dir_all(repo.join(".rift").join("policies")).unwrap();
        std::fs::write(
            repo.join(".rift").join("policies").join("serve.toml"),
            "denied_paths = [\"**/vault/**\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("vault")).unwrap();
        let session = "s-denied";
        read_and_hold(&repo, session, "open.txt", "one\n", 10);
        read_and_hold(&repo, session, "vault/key.txt", "secret\n", 20);
        evict(&repo, session, 30);

        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let map = recovery_map(&repo, &cfg, &CC, START, &compacted(session)).expect("a map");
        assert!(!map.contains("vault"), "{map}");
        assert!(map.contains("open.txt"), "{map}");
        assert!(map.contains("not shown"), "the omission is counted: {map}");

        // And when the policy leaves nothing to say, the answer is silence
        // rather than a frame carrying only a count.
        let session = "s-all-denied";
        read_and_hold(&repo, session, "vault/other.txt", "secret\n", 10);
        evict(&repo, session, 20);
        assert_eq!(
            recovery_map(&repo, &cfg, &CC, START, &compacted(session)),
            None
        );
        drop_family_store(&repo);
    }

    /// ★ **A guarded file ahead of a named one does not move the
    /// announcement.** The map drops the paths this repository does not serve
    /// and then names what is left by position; the held set still has the
    /// dropped file in it. Resolving one list's positions in the other names
    /// the wrong file — and the file it names is the guarded one, whose bytes
    /// a later act would then book a saving against.
    ///
    /// So the fixture puts the denied path *first*, which is the only order
    /// that can skew, and asserts the two halves that matter: the frame names
    /// the served file, and the journal's announcement is about that same file
    /// and nothing else.
    #[test]
    fn a_recovery_map_announces_only_what_it_named_when_a_denied_path_precedes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        std::fs::create_dir_all(repo.join(".rift").join("policies")).unwrap();
        std::fs::write(
            repo.join(".rift").join("policies").join("serve.toml"),
            "denied_paths = [\"private/**\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("private")).unwrap();
        let session = "s-skew";
        // Most recently read leads the held set, so the guarded file is read
        // LAST: it sits ahead of the served one and shifts every position
        // after it.
        read_and_hold(&repo, session, "src.rs", "one\ntwo\nthree\n", 10);
        read_and_hold(&repo, session, "private/secret.txt", "hunter2\n", 20);
        evict(&repo, session, 30);

        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let map = recovery_map(&repo, &cfg, &CC, START, &compacted(session)).expect("a map");
        assert!(map.contains("src.rs"), "the served file is named: {map}");
        assert!(!map.contains("secret"), "the guarded file is not: {map}");

        let announced: Vec<(String, u32, u32)> =
            crate::window_journal::WindowJournal::open(&repo, session, None)
                .load()
                .journal
                .lines()
                .iter()
                .filter_map(|line| match line {
                    rift_agent::window_journal::Line::Hold { path, range, .. } => {
                        Some((path.clone(), range.lo, range.hi))
                    }
                    _ => None,
                })
                .collect();
        assert_eq!(
            announced,
            vec![("src.rs".to_owned(), 1, 3)],
            "the announcement is the file the map named, and only it: {map}"
        );
        drop_family_store(&repo);
    }

    #[test]
    fn hook_health_records_timeout_markers_and_clears_after_success() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        record_hook_health_in(repo, "codex", Some("hook execution did not complete"));
        assert_eq!(
            hook_degradation(repo, "codex").as_deref(),
            Some("hook execution did not complete")
        );

        record_hook_health_in(repo, "codex", None);
        assert_eq!(hook_degradation(repo, "codex"), None);
    }

    #[test]
    fn malformed_hook_health_is_degraded_without_becoming_a_reason_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let path = hook_health_path(tmp.path(), "antigravity");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();

        assert_eq!(
            hook_degradation(tmp.path(), "antigravity").as_deref(),
            Some("hook health state is malformed")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_hook_health_is_rejected() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let path = hook_health_path(tmp.path(), "claude-code");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = tmp.path().join("outside.json");
        std::fs::write(&target, r#"{"schema_version":1,"reason":"forged"}"#).unwrap();
        symlink(target, &path).unwrap();

        assert_eq!(
            hook_degradation(tmp.path(), "claude-code").as_deref(),
            Some("hook health state is invalid")
        );
    }
}
