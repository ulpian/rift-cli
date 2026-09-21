//! What a context window holds right now, and the announcement that writes
//! back what a manifest just told it.

use rift_shadow::hooks::AdapterRegistry;

use super::super::now_ms;
use super::super::recovery::announce_holds;

/// One file a context window holds: the path, the revision it holds, and when
/// it last saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::hooks) struct HeldRevision {
    pub(super) path: String,
    pub(super) hash: String,
    pub(super) at_ms: u64,
}

/// One entry a pushed frame named: the path, and the revision the window
/// holds of it, in the spelling the relation and the journal both use.
pub(in crate::hooks) type NamedHold = (String, String);

/// The revisions this window holds, from the window journal the gate writes.
///
/// Priced deliberately: it parses the payload and reads a bounded file, so it
/// is passed to [`turn_manifest`](super::turn_manifest) as a thunk and called only on the turns that
/// have already decided they may have something to announce. A quiet turn
/// still costs two stamp comparisons and nothing else.
pub(in crate::hooks) fn held_now(
    repo: &std::path::Path,
    agent: &rift_core::AgentId,
    verb: &str,
    body: &[u8],
) -> Vec<HeldRevision> {
    let Ok(raw) = rift_shadow::hooks::build_raw_hook(agent.as_str(), verb, body, now_ms()) else {
        return Vec::new();
    };
    if raw.session.is_empty() {
        return Vec::new();
    }
    // The context window, not the session — the same key the gate wrote the
    // journal under, so a subagent is answered about its own reads and never
    // its parent's.
    let registry = AdapterRegistry::with_builtin();
    let window = registry
        .get(agent)
        .map(|adapter| adapter.facts(verb, &raw.payload))
        .and_then(|facts| facts.child_session);
    held_revisions(
        &crate::window_journal::WindowJournal::open(repo, &raw.session, window.as_deref())
            .load()
            .journal,
    )
}

/// The revision this window holds of every file it has read or written since
/// it last stopped holding anything, newest first.
///
/// Deliberately **not** `held_before_eviction`, which answers the compacted
/// context's question — what a window held at the moment it lost it. This one
/// asks what it holds *now*, so it reads the live stretch and nothing older.
///
/// An `edit` line counts, and that is the load-bearing half. The agent wrote
/// those bytes, so the revision it holds is the one it wrote; a manifest built
/// from reads alone would compare the checkout against the revision the agent
/// held *before its own edit* and announce that edit back to it, every turn,
/// as news.
pub(super) fn held_revisions(journal: &rift_agent::window_journal::Journal) -> Vec<HeldRevision> {
    use rift_agent::window_journal::Line;
    let mut out: Vec<HeldRevision> = Vec::new();
    for line in journal.live() {
        // Through the one normaliser on the way in, because every later
        // comparison is against a `/`-spelled name: the projection's own delta
        // (so a file does not get named twice), the serving policy's globs,
        // and the worktree hasher. A window whose journal predates the `/` rule
        // is answered about its files rather than about two spellings of them.
        let respell = |p: &str| rift_recall_substrate::worktree::slash_spelled(p).into_owned();
        let (path, hash, at_ms) = match line {
            Line::Read {
                path, hash, at_ms, ..
            } => (respell(path), hash.to_hex(), *at_ms),
            Line::Edit {
                path,
                hash_new,
                at_ms,
                ..
            } => (respell(path), hash_new.to_hex(), *at_ms),
            _ => continue,
        };
        // Last writer wins: the lines are in the order they happened, so the
        // newest line about a file is what the window holds of it.
        match out.iter_mut().find(|held| held.path == path) {
            Some(held) => {
                held.hash = hash;
                held.at_ms = held.at_ms.max(at_ms);
            }
            None => out.push(HeldRevision { path, hash, at_ms }),
        }
    }
    out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| a.path.cmp(&b.path)));
    out
}

/// Write down what a manifest just told this window it holds.
///
/// The mirror of [`held_now`], through the same payload and the same window
/// key: the manifest is built from what the journal says the window holds, and
/// this is the journal being told what the window was then told. It is called
/// only on the turns that actually injected, with the entries the frame
/// actually named.
///
/// The lines it announces are the ones the journal already records for that
/// revision — which is what the manifest's own entry was derived from — so a
/// file whose coverage is in two pieces, or whose revision the journal has
/// nothing for, announces nothing and books nothing later.
pub(in crate::hooks) fn announce_now(
    repo: &std::path::Path,
    agent: &rift_core::AgentId,
    verb: &str,
    body: &[u8],
    named: &[NamedHold],
) {
    if named.is_empty() {
        return;
    }
    let Ok(raw) = rift_shadow::hooks::build_raw_hook(agent.as_str(), verb, body, now_ms()) else {
        return;
    };
    if raw.session.is_empty() {
        return;
    }
    // A policy that cannot be read announces nothing, which is the same
    // direction the manifest's own walk takes: it costs a saving, never a line
    // naming a path this repository does not serve.
    let Ok(cfg) = rift_config::EffectiveConfig::load(&repo.join(".rift")) else {
        return;
    };
    let engine = rift_recall_policy::PolicyEngine::new(&cfg)
        .with_serve_policy(rift_recall_policy::ServePolicy::load(&repo.join(".rift")));
    let registry = AdapterRegistry::with_builtin();
    let window = registry
        .get(agent)
        .map(|adapter| adapter.facts(verb, &raw.payload))
        .and_then(|facts| facts.child_session);
    let journal = crate::window_journal::WindowJournal::open(repo, &raw.session, window.as_deref());
    let lines = journal.load().journal;
    let at_ms = u64::try_from(raw.occurred_at_ms).unwrap_or_default();
    let turn = raw.turn.unwrap_or_default();
    for (path, hash) in named {
        let Ok(hash) = rift_core::Sha256Id::parse_hex(hash) else {
            continue;
        };
        let coverage = lines.coverage(path, hash);
        announce_holds(
            &journal,
            &engine,
            turn,
            at_ms,
            std::iter::once((path.as_str(), hash, coverage.runs())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::manifest::rows::ManifestRow;

    /// What the window holds of a file is the newest thing it did to it, and
    /// an edit counts: the agent wrote those bytes. A manifest built from
    /// reads alone would announce the agent's own edit back to it as news.
    #[test]
    fn the_revision_a_window_holds_is_the_last_one_it_read_or_wrote() {
        let hash = |n: u8| rift_core::Sha256Id::parse_hex(&format!("{n:064x}")).unwrap();
        let read = |path: &str, h: u8, at_ms: u64| rift_agent::window_journal::Line::Read {
            turn: 1,
            path: path.to_owned(),
            hash: hash(h),
            range: rift_agent::window_journal::Interval::new(1, 9).unwrap(),
            total_lines: 9,
            at_ms,
        };
        let journal = rift_agent::window_journal::Journal::from_lines(vec![
            read("a.rs", 1, 10),
            read("b.rs", 2, 20),
            rift_agent::window_journal::Line::Edit {
                turn: 2,
                path: "a.rs".to_owned(),
                hash_old: Some(hash(1)),
                hash_new: hash(3),
                sites: vec![],
                shift: 0,
                full: true,
                at_ms: 30,
            },
        ]);
        let held = held_revisions(&journal);
        assert_eq!(
            held.iter()
                .map(|h| (h.path.as_str(), h.hash.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("a.rs", &*format!("{:064x}", 3)),
                ("b.rs", &*format!("{:064x}", 2))
            ],
            "newest first, and the edit is what the window holds of a.rs"
        );
        // An eviction empties it: what a window holds after losing its context
        // is what it has read since, and the recovery map owns the rest.
        let mut evicted = journal;
        evicted.push(rift_agent::window_journal::Line::Evict {
            turn: 3,
            reason: rift_agent::window_journal::EvictReason::PreCompact,
            at_ms: 40,
        });
        assert!(held_revisions(&evicted).is_empty());
    }

    /// A window whose journal predates the `/` rule holds the same files as one
    /// written after it. The two spellings fold into one entry — so a manifest
    /// cannot name one file twice — and the entry carries the spelling every
    /// row, every policy glob and the push guard's alphabet use.
    #[test]
    fn a_held_path_is_respelled_before_anything_compares_it() {
        let hash = |n: u8| rift_core::Sha256Id::parse_hex(&format!("{n:064x}")).unwrap();
        let read = |path: &str, h: u8, at_ms: u64| rift_agent::window_journal::Line::Read {
            turn: 1,
            path: path.to_owned(),
            hash: hash(h),
            range: rift_agent::window_journal::Interval::new(1, 9).unwrap(),
            total_lines: 9,
            at_ms,
        };
        let journal = rift_agent::window_journal::Journal::from_lines(vec![
            read(r"bins\rift\src\hooks.rs", 1, 10),
            read("bins/rift/src/hooks.rs", 2, 20),
        ]);
        let held = held_revisions(&journal);
        assert_eq!(
            held.iter()
                .map(|h| (h.path.as_str(), h.hash.as_str()))
                .collect::<Vec<_>>(),
            vec![("bins/rift/src/hooks.rs", &*format!("{:064x}", 2))],
            "one file, one entry, and the newest line is what the window holds"
        );
        // The baseline side of the same comparison: a projection row that
        // predates the rule enters the manifest under the one spelling too.
        assert_eq!(
            ManifestRow::from_baseline(r"crates\rift-store\src\schema.rs", &format!("{:064x}", 1))
                .path,
            "crates/rift-store/src/schema.rs"
        );
    }
}
