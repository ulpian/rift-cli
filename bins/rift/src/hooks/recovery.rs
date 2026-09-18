//! The post-compaction recovery map (injection tier c). The one moment a full
//! listing of what a session holds pays for itself: its context has just been
//! summarised or rebuilt, it no longer holds the bytes it read, and it is
//! about to read them all again. Everywhere else the same listing is spend —
//! a quiet turn is answered with silence, and that property is untouched by
//! anything here.

use rift_core::Result;
use rift_shadow::hooks::AdapterRegistry;

use super::agent::{boundary_budget, boundary_ceiling};
use super::audit::{append_memd_audit, serve_audit_envelopes, serve_audit_event};
use super::gate::AUDIT_IDS;
use super::now_ms;

/// The most entries one map will consider naming, before the budget has its
/// say. A session that read six hundred files is a session whose map is a wall
/// of paths; the warm end of the list is where the next read is, and the rest
/// is a count.
const MAX_RECOVERY_ENTRIES: usize = 64;

/// The most files one map asks a verdict about — the cap the diff module
/// declares, applied here because this is the caller it was declared for. Past
/// it an entry is its path, its revision and the lines held — facts that cost
/// nothing to state — and no claim about what the file is now.
const MAX_RECOVERY_VERDICTS: usize = rift_recall_serve::held_diff::MAX_DIFFED_FILES;

/// The characters one token of the boundary budget is worth.
///
/// The renderer fits to characters because the tokenizer is not its crate's;
/// the conversion is made here, once, and then **checked** against the real
/// count below — so this is a starting point for the fit, never the bound
/// itself.
const RECOVERY_CHARS_PER_TOKEN: usize = 4;

/// How many times the fit may narrow its character budget before it gives up
/// and says nothing. Each pass is one render and one tokenizer count over a
/// bounded list, and a fit that has not converged in this many passes is one
/// whose entries do not fit at all.
const MAX_RECOVERY_FITS: usize = 8;

/// What the map can say about one file the session held.
enum RecoveryVerdict {
    /// The bytes still hash to the revision the session read.
    Unchanged,
    /// The file changed, in these places in the numbering a reader sees now.
    ChangedAt(Vec<(u32, u32)>),
    /// Nothing was reachable to compare against, so nothing is claimed.
    Unknown,
}

/// One entry of the map, owned, so the borrowed render type can be built from
/// it in one pass at the end.
///
/// It carries the announcement's own facts — the revision the window read and
/// the runs it holds of that revision — rather than a way back into the held
/// set it was built from. The renderer names entries by their position in
/// *this* list and the guard below drops held files from it, so the two lists
/// have different positions in them; an entry that answers for itself cannot
/// be answered for by the wrong file.
struct RecoveryEntry {
    path: String,
    hash: String,
    held: Vec<(u32, u32)>,
    verdict: RecoveryVerdict,
    /// The revision the window read, spelled as the ledger writes it.
    revision: rift_core::Sha256Id,
    /// The runs the window holds of that revision, as the ledger writes them.
    runs: Vec<rift_agent::read_ledger::Interval>,
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

/// The same field, where the family writes a number rather than a string.
pub(super) fn hook_number(payload: &serde_json::Value, key: &str) -> Option<u64> {
    payload
        .get("payload")
        .and_then(|inner| inner.get(key))
        .or_else(|| payload.get(key))
        .and_then(serde_json::Value::as_u64)
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
/// ledger's own last signal decides ([`recovery_follows_the_boundary`]).
///
/// Antigravity publishes no compaction signal at all. What it publishes is a
/// counter, and its first invocation is the only execution boundary it names —
/// so the map rides that, and what decides whether there is a map to give is
/// the same thing that decides it everywhere else: whether the window's ledger
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
/// For the family whose verb already said so, the ledger only has to have a
/// boundary at all. For the family whose boundary is announced on a different
/// hook, the last signal must be the one that closes a compaction — otherwise
/// this is an ordinary turn, and an ordinary turn is answered by the manifest
/// or by silence.
fn recovery_follows_the_boundary(
    agent: &rift_core::AgentId,
    last: Option<rift_agent::read_ledger::EvictReason>,
) -> bool {
    if *agent == rift_core::AgentId::CODEX {
        return last == Some(rift_agent::read_ledger::EvictReason::PostCompact);
    }
    last.is_some()
}

/// The map a context that has just lost its state is handed, or `None` when
/// there is nothing to hand it.
///
/// Every gate fails to `None`, and the order they are asked in is the cost
/// order: the verb and the source are a string comparison, the ledger is a
/// bounded file read, the verdicts are a hash of at most sixteen files, and the
/// store is opened last and only to book what the map cost. A fresh session
/// stops at the first gate; a session that never lost anything stops at the
/// third.
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
    // The context window, not the session — the same key the gate wrote the
    // ledger under, so a subagent is handed its own window's map and never its
    // parent's.
    let registry = AdapterRegistry::with_builtin();
    let window = registry
        .get(agent)
        .map(|adapter| adapter.facts(verb, &raw.payload))
        .and_then(|facts| facts.child_session);
    let window_ledger = crate::read_ledger::ReadLedger::open(repo, &raw.session, window.as_deref());
    let ledger = window_ledger.load().ledger;
    let last = ledger.lines().iter().rev().find_map(|line| match line {
        rift_agent::read_ledger::Line::Evict { reason, .. } => Some(*reason),
        _ => None,
    });
    if !recovery_follows_the_boundary(agent, last) {
        return None;
    }
    let held = ledger.held_before_eviction();
    if held.is_empty() {
        // A session that lost nothing it had read — a fresh one, or one whose
        // reads Rift never saw — is told nothing. There is no honest listing to
        // give it, and the pointer is already its introduction.
        return None;
    }
    let engine = rift_recall_policy::PolicyEngine::new(cfg).with_serve_policy(policy);
    // The revision a window holds is reachable only through the family's store
    // and the read-only substrate, so both are opened once for the whole
    // boundary — for the verdicts below and for the row that books what the map
    // cost. Neither is available on the gate path and neither is asked for
    // there; this is a turn boundary, where a store is already the norm. An
    // unreachable one costs verdicts, never the map.
    let rt = tokio::runtime::Builder::new_current_thread().build().ok()?;
    let basis = crate::family::recall_db(repo)
        .ok()
        .and_then(|db| rift_store::open_migrated(&db, &rift_store::recall_migrations()).ok())
        .zip(rift_recall_substrate::GixSubstrate::open(repo).ok());
    // The ladder's second rung: the revisions a read recorded, kept by the
    // detached worker. A window that read a file and then edited it holds a
    // revision no blob has, which is the case the blob route answers
    // `Unknown` for and this one answers exactly.
    let snapshots = basis_snapshots_at(repo);
    let (entries, omitted) = recovery_entries(
        &held,
        &|path| engine.is_denied_path(path),
        &mut |path, hash| {
            recovery_verdict(repo, path, hash, basis.as_ref(), snapshots.as_ref(), &rt)
        },
    );
    if entries.is_empty() {
        // Every file this window held is one this repository does not serve.
        // A frame with a count and no facts in it is spend, so the honest
        // answer is the same silence a session with no held set gets.
        return None;
    }
    let rendered: Vec<rift_recall_serve::HeldEntry<'_>> = entries
        .iter()
        .map(|entry| rift_recall_serve::HeldEntry {
            path: &entry.path,
            hash: &entry.hash,
            held: &entry.held,
            verdict: match &entry.verdict {
                RecoveryVerdict::Unchanged => rift_recall_serve::HeldVerdict::Unchanged,
                RecoveryVerdict::ChangedAt(sites) => {
                    rift_recall_serve::HeldVerdict::ChangedAt(sites)
                }
                RecoveryVerdict::Unknown => rift_recall_serve::HeldVerdict::Unknown,
            },
        })
        .collect();
    let (text, named) = fit_recovery_map(&rendered, omitted, budget)?;
    // Claimed only once the text exists: a map that could not be fitted is
    // still owed, and the next boundary hook of this generation may fit it.
    if !claim_recovery(repo, &raw.session, window.as_deref(), &held) {
        return None;
    }
    record_recovery_injection(repo, agent, &text);
    announce_recovery(&window_ledger, &engine, &raw, &entries, &named);
    Some(text)
}

/// The map's entries, in the order the renderer will name them, and the count
/// of held files that did not become one.
///
/// The guard and the entry cap are the only two ways a held file fails to
/// become an entry, and both are applied here — which is the whole point of
/// the split. Everything downstream, the renderer and the announcement alike,
/// speaks about positions in the list this returns, so there is no second
/// index space for a dropped file to shift.
fn recovery_entries(
    held: &[rift_agent::read_ledger::HeldFile],
    denied: &dyn Fn(&str) -> bool,
    verdict: &mut dyn FnMut(&str, rift_core::Sha256Id) -> RecoveryVerdict,
) -> (Vec<RecoveryEntry>, usize) {
    let mut entries: Vec<RecoveryEntry> = Vec::new();
    let mut omitted = 0usize;
    for file in held {
        if denied(&file.path) {
            // The same filter every serve applies. A path this repository does
            // not serve is not served by a new renderer either, and the count
            // says something was left out.
            omitted = omitted.saturating_add(1);
            continue;
        }
        if entries.len() >= MAX_RECOVERY_ENTRIES {
            omitted = omitted.saturating_add(1);
            continue;
        }
        let answer = if entries.len() < MAX_RECOVERY_VERDICTS {
            verdict(&file.path, file.hash)
        } else {
            RecoveryVerdict::Unknown
        };
        let runs = file.held.runs().to_vec();
        entries.push(RecoveryEntry {
            path: file.path.clone(),
            hash: file.hash.to_hex(),
            held: runs.iter().map(|run| (run.lo, run.hi)).collect(),
            verdict: answer,
            revision: file.hash,
            runs,
        });
    }
    (entries, omitted)
}

/// What the map announced, read out of the very list whose positions the
/// renderer handed back.
///
/// A position the renderer did not produce resolves to nothing rather than to
/// a neighbour: the failure direction is an announcement nobody makes.
fn recovery_announcements<'a>(
    entries: &'a [RecoveryEntry],
    named: &'a [usize],
) -> impl Iterator<
    Item = (
        &'a str,
        rift_core::Sha256Id,
        &'a [rift_agent::read_ledger::Interval],
    ),
> {
    named.iter().filter_map(|at| {
        let entry = entries.get(*at)?;
        Some((entry.path.as_str(), entry.revision, entry.runs.as_slice()))
    })
}

/// Write down what the map told this window it holds.
///
/// Only the entries the frame carried: the ones the guard withheld and the
/// budget dropped reached the model as a count, and a count is not a fact
/// about a file.
fn announce_recovery(
    ledger: &crate::read_ledger::ReadLedger,
    engine: &rift_recall_policy::PolicyEngine<'_>,
    raw: &rift_shadow::hooks::RawHook,
    entries: &[RecoveryEntry],
    named: &[usize],
) {
    announce_holds(
        ledger,
        engine,
        raw.turn.unwrap_or_default(),
        u64::try_from(raw.occurred_at_ms).unwrap_or_default(),
        recovery_announcements(entries, named),
    );
}

/// Write down what an injected frame told this window it holds.
///
/// One line per file, and only where the announcement is about **one run of
/// lines**: a held set in two pieces is priced by no single range, and the
/// standing rule for a fact Rift cannot state exactly is that it does not
/// state it at all. The failure direction is a saving nobody books.
///
/// The guard is asked here as well, of every path, and not because a caller is
/// expected to have forgotten it. A hold line is what a later booking claims a
/// saving against, so a line naming a path this repository does not serve
/// would put a denied file into the accounting whatever went wrong upstream —
/// a caller's own filter shifting a position, a frame built from a list nobody
/// filtered. One chokepoint, asked last, so there is no route around it.
///
/// Best-effort throughout — a line that cannot be written is an announcement
/// nothing will be booked against, which costs a figure and never a claim.
pub(super) fn announce_holds<'a>(
    ledger: &crate::read_ledger::ReadLedger,
    engine: &rift_recall_policy::PolicyEngine<'_>,
    turn: u32,
    at_ms: u64,
    entries: impl Iterator<
        Item = (
            &'a str,
            rift_core::Sha256Id,
            &'a [rift_agent::read_ledger::Interval],
        ),
    >,
) {
    for (path, hash, runs) in entries {
        if engine.is_denied_path(path) {
            continue;
        }
        let [range] = runs else { continue };
        let _ = ledger.append(&rift_agent::read_ledger::Line::Hold {
            turn,
            path: path.to_owned(),
            hash,
            range: *range,
            at_ms,
        });
    }
}

/// This member's view of the family's read-time basis snapshots, when the
/// family has an identity to look it up by.
///
/// `None` is not a failure: a checkout whose family cannot be resolved from
/// here has no shared store, and the basis ladder simply stops at the rung it
/// has always had. Opened where a store is already open — never on the gate
/// path, which is why it is a function the boundary and manifest turns call
/// and nothing a decision reaches.
pub(crate) fn basis_snapshots_at(
    work_tree: &std::path::Path,
) -> Option<rift_recall_serve::basis_store::BasisStore> {
    let family = crate::family::try_at(work_tree)?;
    Some(rift_recall_serve::basis_store::BasisStore::for_family(
        &family.common_dir,
    ))
}

/// What is true of one held file now: unchanged, changed in these places, or
/// nothing anyone can say.
///
/// The two answers are reached by different routes on purpose. **Unchanged** is
/// equality of the revision, which is exact and needs nothing but the bytes in
/// the checkout: the file hashes to what the session read, so what the session
/// held is what a re-read would return, and the re-read can be skipped
/// outright. **Changed at** is the diff module's answer against the revision
/// the session holds, fetched through the blob map — the same computation the
/// manifest and the pull verb give, so a reader is never handed two sets of
/// line numbers for one question.
///
/// Everything else is [`RecoveryVerdict::Unknown`], and that includes a file
/// that changed in more places than an entry may name: a partial list of
/// changed places would tell a reader that everything unnamed is unchanged,
/// which is the one thing this must never say.
fn recovery_verdict(
    repo: &std::path::Path,
    rel_path: &str,
    held: rift_core::Sha256Id,
    basis: Option<&(rift_store::SqliteStore, rift_recall_substrate::GixSubstrate)>,
    snapshots: Option<&rift_recall_serve::basis_store::BasisStore>,
    rt: &tokio::runtime::Runtime,
) -> RecoveryVerdict {
    let Some((blob, bytes)) = rift_recall_substrate::worktree::hash_blob_bytes(repo, rel_path)
    else {
        return RecoveryVerdict::Unknown;
    };
    let held = held.to_hex();
    if blob.content_sha256 == held {
        return RecoveryVerdict::Unchanged;
    }
    let Some((store, substrate)) = basis else {
        return RecoveryVerdict::Unknown;
    };
    let diff = rt.block_on(rift_recall_serve::held_diff::held_diff_against_basis(
        store, substrate, snapshots, &held, &bytes,
    ));
    match diff {
        rift_recall_serve::held_diff::HeldDiff::Changed { ranges, .. } => {
            RecoveryVerdict::ChangedAt(ranges)
        }
        _ => RecoveryVerdict::Unknown,
    }
}

/// The map, fitted to the boundary budget in **tokens**.
///
/// The renderer drops whole entries from the cold end to fit a character
/// budget; the budget that matters is the operator's, in tokens, so the
/// conversion is a starting point and the real tokenizer decides. Each pass
/// narrows the character budget in proportion to how far over it landed, which
/// converges in one or two passes on any real list; a list that has not fitted
/// after a few is one that cannot, and silence is the honest answer.
fn fit_recovery_map(
    entries: &[rift_recall_serve::HeldEntry<'_>],
    omitted: usize,
    budget: u32,
) -> Option<(String, Vec<usize>)> {
    let budget_usize = usize::try_from(budget).unwrap_or(usize::MAX);
    let mut chars = budget_usize.saturating_mul(RECOVERY_CHARS_PER_TOKEN);
    for _ in 0..MAX_RECOVERY_FITS {
        let (text, named) = rift_recall_serve::render_recovery_map_naming(entries, omitted, chars)?;
        let counted = rift_tokens::count(&text);
        if counted <= budget {
            return Some((text, named));
        }
        let counted = usize::try_from(counted).unwrap_or(usize::MAX).max(1);
        let narrowed = chars.saturating_mul(budget_usize) / counted;
        if chars == 0 {
            return None;
        }
        chars = narrowed.min(chars.saturating_sub(1));
    }
    None
}

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
fn claim_recovery(
    repo: &std::path::Path,
    session: &str,
    window: Option<&str>,
    held: &[rift_agent::read_ledger::HeldFile],
) -> bool {
    use std::fmt::Write as _;
    let dir = repo.join(".rift").join("state").join("recovery");
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    // Hashed, so nothing an agent supplied — a session id, a path — becomes a
    // path component here, exactly as on the ledger's own file names.
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
/// envelopes every other hook-side decision is written with.
///
/// A recovery map is pure spend — it avoids nothing by itself, and what it
/// saves is a read that never happens later — so it books a returned figure and
/// no avoided one, which is exactly what the honesty gate has to net.
/// Best-effort: an unavailable projection costs an audit row, never a turn.
fn record_recovery_injection(repo: &std::path::Path, agent: &rift_core::AgentId, text: &str) {
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
    };
    let _ = (|| -> Result<()> {
        let db = crate::family::recall_db(repo)?;
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
        let at_ms = now_ms();
        let event = serve_audit_event(repo, &audit);
        let (audited, called) = serve_audit_envelopes(&event, &audit, at_ms);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(rift_core::RiftError::from)?;
        rt.block_on(async {
            append_memd_audit(&store, audited.as_bytes(), at_ms).await?;
            append_memd_audit(&store, called.as_bytes(), at_ms).await
        })
    })();
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use crate::hooks::agent::{
        boundary_is_open, hook_degradation, hook_health_path, record_hook_health_in,
        session_pointer_in,
    };
    use crate::hooks::agent::{drop_projection_store, set_up_here};
    use crate::hooks::worker::session_start_hook;

    /// The families and verbs this chain is driven on, spelled once.
    const CC: rift_core::AgentId = rift_core::AgentId::CLAUDE_CODE;
    const START: &str = "SessionStart";

    /// A Recall-enabled repository with a real git dir, so every store this
    /// path opens is a family's.
    fn recall_repo(tmp: &std::path::Path) -> std::path::PathBuf {
        rift_git::test_support::fake_repo(tmp).unwrap();
        std::fs::create_dir_all(tmp.join(".rift")).unwrap();
        std::fs::write(
            tmp.join(".rift").join("config.toml"),
            "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
        )
        .unwrap();
        set_up_here(tmp);
        tmp.to_path_buf()
    }

    /// Write `text` to `rel` and record that the window read all of it, at the
    /// revision it has right now.
    fn read_and_hold(repo: &std::path::Path, session: &str, rel: &str, text: &str, at_ms: u64) {
        std::fs::write(repo.join(rel), text).unwrap();
        let ledger = crate::read_ledger::ReadLedger::open(repo, session, None);
        let lines = u32::try_from(text.lines().count().max(1)).unwrap();
        ledger
            .append(&rift_agent::read_ledger::Line::Read {
                turn: 1,
                path: rel.to_owned(),
                hash: rift_core::Sha256Id::parse_hex(&rift_shadow::sha256_hex(&[text.as_bytes()]))
                    .unwrap(),
                range: rift_agent::read_ledger::Interval::new(1, lines).unwrap(),
                total_lines: lines,
                at_ms,
            })
            .unwrap();
    }

    fn evict(repo: &std::path::Path, session: &str, at_ms: u64) {
        crate::read_ledger::ReadLedger::open(repo, session, None)
            .append(&rift_agent::read_ledger::Line::Evict {
                turn: 1,
                reason: rift_agent::read_ledger::EvictReason::PreCompact,
                at_ms,
            })
            .unwrap();
    }

    fn compacted(session: &str) -> Vec<u8> {
        serde_json::json!({ "session_id": session, "source": "compact" })
            .to_string()
            .into_bytes()
    }

    fn drop_family_store(repo: &std::path::Path) {
        if let Ok(db) = crate::family::recall_db(repo) {
            if let Some(dir) = db.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        drop_projection_store(repo);
    }

    /// **What the map said, written down.** A recovery map that names a file
    /// tells the window it holds it, and the window's own ledger records the
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
        let ledger = crate::read_ledger::ReadLedger::open(&repo, session, None)
            .load()
            .ledger;
        let mut announced: Vec<(String, u32, u32)> = ledger
            .lines()
            .iter()
            .filter_map(|line| match line {
                rift_agent::read_ledger::Line::Hold { path, range, .. } => {
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
                ledger.standing_hold(path).is_some(),
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
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
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

    /// An entry claims "unchanged" only when the bytes still hash to what the
    /// session read; a file that moved, or that is not there any more, is
    /// listed with its revision and its lines and **no claim at all**.
    ///
    /// The failure this forbids is the expensive one: a model told a changed
    /// file is unchanged skips the read that would have corrected it.
    #[test]
    fn an_entry_claims_unchanged_only_against_the_held_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        let session = "s-verdicts";
        read_and_hold(&repo, session, "still.txt", "one\n", 10);
        read_and_hold(&repo, session, "moved.txt", "one\n", 20);
        read_and_hold(&repo, session, "gone.txt", "one\n", 30);
        std::fs::write(repo.join("moved.txt"), "one\ntwo\n").unwrap();
        std::fs::remove_file(repo.join("gone.txt")).unwrap();
        evict(&repo, session, 40);

        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let map = recovery_map(&repo, &cfg, &CC, START, &compacted(session)).expect("a map");
        let line = |path: &str| -> String {
            map.lines()
                .find(|line| line.contains(path))
                .unwrap_or_default()
                .to_owned()
        };
        assert!(
            line("still.txt").contains("unchanged since your read"),
            "{map}"
        );
        assert!(
            !line("moved.txt").contains("unchanged"),
            "the bytes moved: {map}"
        );
        assert!(
            !line("gone.txt").contains("unchanged"),
            "there is nothing to compare against: {map}"
        );
        assert!(
            line("moved.txt").contains("L1-1"),
            "what it held is still stated: {map}"
        );
        drop_family_store(&repo);
    }

    /// **Each entry is labelled against the revision the session holds**: the
    /// file that still hashes to it says so, and the file that moved says
    /// *where* it moved, in the numbering a reader is about to see.
    ///
    /// The two labels are reached by different routes and this walks both. The
    /// unchanged one is an equality of hashes over the bytes in the checkout.
    /// The changed one goes through the family store's blob map to the
    /// revision the session read and diffs it — the same computation the
    /// manifest and the pull verb use — so a real repository, a real commit
    /// and a real mapped blob are what this is built on rather than a stub.
    #[test]
    fn entries_are_labelled_unchanged_or_changed_against_the_held_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| -> String {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", repo.join("absent-gitconfig"))
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .output()
                .expect("git is on PATH");
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.invalid"]);
        git(&["config", "user.name", "t"]);
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        std::fs::write(
            repo.join(".rift").join("config.toml"),
            "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
        )
        .unwrap();
        let basis = "alpha\nbravo\ncharlie\n";
        std::fs::write(repo.join("moved.txt"), basis).unwrap();
        std::fs::write(repo.join("still.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "seed"]);

        let session = "s-labelled";
        read_and_hold(&repo, session, "moved.txt", basis, 20);
        read_and_hold(&repo, session, "still.txt", "one\n", 10);

        // The route to the revision a window holds is the blob map the
        // projection writes at ingest; this fixture writes the one row that
        // walk would have written, so the diff has a basis to reach.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let db = crate::family::recall_db(&repo).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        rt.block_on(rift_recall_ledger::blob_map::persist_blob_oid_map(
            &store,
            &[rift_recall_substrate::worktree::WorktreeBlob {
                path: "moved.txt".to_owned(),
                content_sha256: rift_shadow::sha256_hex(&[basis.as_bytes()]),
                blob_oid: git(&["rev-parse", "HEAD:moved.txt"]),
            }],
            0,
        ))
        .unwrap();
        drop(store);

        // The file moves after the session read it, and then the context is
        // compacted.
        std::fs::write(repo.join("moved.txt"), "alpha\nBRAVO\ncharlie\n").unwrap();
        evict(&repo, session, 30);

        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let map = recovery_map(&repo, &cfg, &CC, START, &compacted(session)).expect("a map");
        let line = |path: &str| -> String {
            map.lines()
                .find(|line| line.contains(path))
                .unwrap_or_default()
                .to_owned()
        };
        assert!(
            line("moved.txt").contains("changed at lines 2-2"),
            "the place it moved, in current numbering: {map}"
        );
        assert!(
            line("still.txt").contains("unchanged since your read"),
            "{map}"
        );
        assert!(
            !line("moved.txt").contains("unchanged"),
            "one file, one claim: {map}"
        );
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
        use rift_agent::read_ledger::EvictReason;
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
    /// The budget is a ceiling measured with the real tokenizer, and the fit
    /// pays for it by dropping the cold end of the list into a count — never by
    /// cutting a line in half, and never by exceeding the budget.
    #[test]
    fn the_budget_drops_the_cold_end_into_a_count() {
        let paths: Vec<String> = (0..40)
            .map(|i| format!("src/module-{i:02}/file.rs"))
            .collect();
        let hash = "3f".repeat(32);
        let held = [(1u32, 500u32)];
        let entries: Vec<rift_recall_serve::HeldEntry<'_>> = paths
            .iter()
            .map(|path| rift_recall_serve::HeldEntry {
                path,
                hash: &hash,
                held: &held,
                verdict: rift_recall_serve::HeldVerdict::Unchanged,
            })
            .collect();
        let mut fitted = 0usize;
        for budget in [40u32, 120, 400, 800, 4000] {
            // Silence is a permitted answer — a budget that cannot carry even
            // the frame and one entry buys nothing by carrying half of one —
            // but an overrun never is.
            let Some((text, _)) = fit_recovery_map(&entries, 0, budget) else {
                continue;
            };
            fitted += 1;
            assert!(
                rift_tokens::count(&text) <= budget,
                "{} tokens over a {budget}-token budget",
                rift_tokens::count(&text)
            );
            let shown = text.lines().filter(|l| l.starts_with("- ")).count();
            assert!(shown >= 1, "at least one entry survives: {text}");
            if shown < paths.len() {
                assert!(
                    text.contains("not shown"),
                    "what was dropped is counted: {text}"
                );
            }
        }
        assert!(fitted >= 3, "the ordinary budgets fit a map");
        assert_eq!(
            fit_recovery_map(&entries, 0, 0),
            None,
            "a zero budget is the operator turning the map off"
        );
    }

    /// **A family that declares a bound on its context channel gets text that
    /// is inside it, and it is bought by dropping whole entries.**
    ///
    /// The bound is not the operator's budget and does not replace it: it is a
    /// second ceiling, and the rendering budget is the lower of the two in
    /// either direction. What must never happen is the text going out over the
    /// family's number for the client to deal with — the client keeps a head
    /// and a tail and spills the middle to a file, so the model reads a frame
    /// cut in two places instead of a shorter frame that is whole.
    #[test]
    fn text_is_fitted_to_the_declared_limit_by_dropping_entries() {
        let declared = rift_agent::respond::DECLARED_CONTEXT_LIMIT_TOKENS;
        assert_eq!(
            boundary_ceiling(&rift_core::AgentId::CODEX, "UserPromptSubmit"),
            Some(declared),
            "the number the installer wrote into this family's hook file"
        );
        assert_eq!(
            boundary_ceiling(&CC, "UserPromptSubmit"),
            None,
            "a family that bounds the channel by nothing bounds it by nothing"
        );
        // Lowered, never raised — in both directions, and unbounded where the
        // family declares no ceiling at all.
        assert_eq!(boundary_budget(400, Some(declared)), 400);
        assert_eq!(boundary_budget(declared + 2_800, Some(declared)), declared);
        assert_eq!(boundary_budget(declared + 2_800, None), declared + 2_800);
        assert_eq!(boundary_budget(0, Some(declared)), 0, "off stays off");

        // Twenty entries: a list the declared bound carries whole, so the
        // comparison below is about the bound and not about the list's size.
        let paths: Vec<String> = (0..20)
            .map(|i| format!("src/module-{i:02}/file.rs"))
            .collect();
        let hash = "3f".repeat(32);
        let held = [(1u32, 500u32)];
        let entries: Vec<rift_recall_serve::HeldEntry<'_>> = paths
            .iter()
            .map(|path| rift_recall_serve::HeldEntry {
                path,
                hash: &hash,
                held: &held,
                verdict: rift_recall_serve::HeldVerdict::Unchanged,
            })
            .collect();
        let whole_entries = |text: &str| -> usize {
            for line in text.lines().filter(|line| line.starts_with("- ")) {
                assert!(
                    paths.iter().any(|path| line.contains(path.as_str()))
                        && line.contains(&hash[..12]),
                    "an entry is named whole or not at all: {line}"
                );
            }
            text.lines().filter(|line| line.starts_with("- ")).count()
        };

        // A generous operator budget, bounded only by what the family declared.
        let (roomy, _) = fit_recovery_map(&entries, 0, boundary_budget(4_000, Some(declared)))
            .expect("the declared bound carries a map");
        assert!(rift_tokens::count(&roomy) <= declared, "{roomy}");
        let roomy_shown = whole_entries(&roomy);

        // The same list under a family that declared a much smaller one: fewer
        // entries, each of them whole, and the rest are a count.
        let (tight, _) = fit_recovery_map(&entries, 0, boundary_budget(4_000, Some(400)))
            .expect("a small bound still carries a map");
        assert!(rift_tokens::count(&tight) <= 400, "{tight}");
        let tight_shown = whole_entries(&tight);
        assert!(
            tight_shown < roomy_shown,
            "the smaller bound is paid for in entries: {tight_shown} vs {roomy_shown}"
        );
        assert!(
            tight_shown >= 1,
            "and it is paid, not defaulted away: {tight}"
        );
        assert!(
            tight.contains("not shown"),
            "what was dropped is counted: {tight}"
        );
    }

    /// The bound Rift declares for itself is above everything it means to send
    /// through the channel it bounds.
    ///
    /// It is one number written in one place, so the thing that can go wrong is
    /// not disagreement between two copies but a number chosen too small: a
    /// ceiling under a shipped budget would silently shrink that budget on one
    /// family only, and a ceiling under the session pointer would drop the one
    /// injection that is not fitted at all because it has no entries to drop.
    #[test]
    fn the_declared_bound_is_above_every_budget_and_text_it_bounds() {
        let declared = rift_agent::respond::DECLARED_CONTEXT_LIMIT_TOKENS;
        assert!(
            declared >= rift_recall_policy::DEFAULT_RECOVERY_BUDGET,
            "a post-compaction map fits the bound"
        );
        assert!(
            declared >= rift_recall_policy::DEFAULT_INJECTION_TURN_BUDGET,
            "a turn's manifest fits the bound"
        );
        assert!(
            rift_tokens::count(&crate::agent_help::session_pointer()) <= declared,
            "the pointer is a fixed text with nothing to drop, so it must fit"
        );
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
    /// the served file, and the ledger's announcement is about that same file
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
            crate::read_ledger::ReadLedger::open(&repo, session, None)
                .load()
                .ledger
                .lines()
                .iter()
                .filter_map(|line| match line {
                    rift_agent::read_ledger::Line::Hold { path, range, .. } => {
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

    /// ★ **No hold line for a guarded path, whatever reached the writer.**
    /// The rule is asked of the one function that writes them, so a caller
    /// that built its list wrongly still cannot put a denied file into the
    /// accounting.
    #[test]
    fn a_hold_line_is_never_written_for_a_denied_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        std::fs::create_dir_all(repo.join(".rift").join("policies")).unwrap();
        std::fs::write(
            repo.join(".rift").join("policies").join("serve.toml"),
            "denied_paths = [\"private/**\"]\n",
        )
        .unwrap();
        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let engine = rift_recall_policy::PolicyEngine::new(&cfg)
            .with_serve_policy(rift_recall_policy::ServePolicy::load(&repo.join(".rift")));
        let ledger = crate::read_ledger::ReadLedger::open(&repo, "s-guard", None);
        let hash =
            rift_core::Sha256Id::parse_hex(&rift_shadow::sha256_hex(&[b"x".as_slice()])).unwrap();
        let runs = [rift_agent::read_ledger::Interval::new(1, 30).unwrap()];
        announce_holds(
            &ledger,
            &engine,
            1,
            10,
            [
                ("private/secret.txt", hash, runs.as_slice()),
                ("src.rs", hash, runs.as_slice()),
            ]
            .into_iter(),
        );
        let lines = ledger.load().ledger;
        assert_eq!(lines.standing_hold("private/secret.txt"), None);
        assert!(lines.standing_hold("src.rs").is_some());
    }

    /// ★ **Every position the renderer hands back names its own entry.** Over
    /// random held sets and random guard masks: whatever the guard drops, an
    /// announcement is the path, the revision and the runs of the entry at
    /// that position in the rendered list — never of the held file that
    /// happened to sit there.
    #[test]
    fn an_announcement_names_the_entry_the_renderer_named() {
        use proptest::prelude::*;

        proptest!(|(mask in proptest::collection::vec(any::<bool>(), 1..12))| {
            let held: Vec<rift_agent::read_ledger::HeldFile> = mask
                .iter()
                .enumerate()
                .map(|(at, denied)| {
                    let path = if *denied {
                        format!("private/{at}.txt")
                    } else {
                        format!("src/{at}.rs")
                    };
                    let mut set = rift_agent::read_ledger::IntervalSet::new();
                    set.insert(
                        rift_agent::read_ledger::Interval::new(1, u32::try_from(at + 1).unwrap())
                            .unwrap(),
                    );
                    rift_agent::read_ledger::HeldFile {
                        path,
                        hash: rift_core::Sha256Id::parse_hex(&rift_shadow::sha256_hex(&[
                            format!("{at}").as_bytes(),
                        ]))
                        .unwrap(),
                        held: set,
                        last_read_at_ms: 10,
                    }
                })
                .collect();
            let (entries, omitted) = recovery_entries(
                &held,
                &|path| path.starts_with("private/"),
                &mut |_, _| RecoveryVerdict::Unknown,
            );
            prop_assert_eq!(entries.len() + omitted, held.len());
            for entry in &entries {
                prop_assert!(!entry.path.starts_with("private/"), "{}", entry.path);
            }
            // Every position, in every order, including ones no renderer would
            // produce: each resolves to its own entry or to nothing.
            let named: Vec<usize> = (0..held.len() + 2).rev().collect();
            let announced: Vec<(&str, rift_core::Sha256Id, &[rift_agent::read_ledger::Interval])> =
                recovery_announcements(&entries, &named).collect();
            prop_assert_eq!(announced.len(), entries.len());
            for (path, hash, runs) in announced {
                let entry = entries
                    .iter()
                    .find(|entry| entry.path == path)
                    .expect("an announcement names an entry of the rendered list");
                prop_assert_eq!(hash, entry.revision);
                prop_assert_eq!(runs, entry.runs.as_slice());
                let file = held
                    .iter()
                    .find(|file| file.path == path)
                    .expect("and that entry is a file the window held");
                prop_assert_eq!(hash, file.hash);
                prop_assert_eq!(runs, file.held.runs());
            }
        });
    }
}
