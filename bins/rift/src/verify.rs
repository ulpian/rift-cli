//! `rift verify` — the M0 hash-chain verification skeleton.
//!
//! Re-derives a trivial single-checkpoint hash chain from a committed fixture,
//! materialises (or re-opens) the per-repo projection under
//! `~/.cache/rift/<repo_id>/`, and proves the projection's stored chain matches
//! the re-derived one. A divergent source (the tamper case) surfaces as
//! [`RiftError::Integrity`](rift_core::RiftError::Integrity) and a non-zero exit.
//!
//! The authoritative source is **git** ([`GitCheckpointSource`]): `rift verify`
//! re-derives the checkpoint chain by walking `rift/checkpoints/v1` through the
//! read-only gix [`SubstrateReader`] and cross-checks it against the projection. On
//! a fresh repo with no checkpoint branch yet, it falls back to the committed
//! fixture skeleton so the wiring (projection create/migrate, record, cross-check,
//! report) still runs end-to-end (the M0 path).

use std::path::Path;

use rift_core::{RepoId, Result, Sha256Id};
use rift_hashchain::{leaf_hash, merkle_root, ChainLink};
use rift_recall_substrate::{Dialect, GixSubstrate, SubstrateReader};
use rift_store::{
    paths::projection_db_under, record_checkpoint, verify_against_source, CanonicalStore,
    ChainSource, CheckpointRecord, VerifyReport,
};

/// The committed single-checkpoint fixture (genesis event leaves, one per line).
const FIXTURE: &str = include_str!("../fixtures/genesis_checkpoint.txt");

/// The authoritative chain source for the skeleton: one genesis checkpoint whose
/// Merkle root is recomputed over its event leaves.
struct FixtureSource {
    leaves: Vec<Vec<u8>>,
}

impl FixtureSource {
    /// Parse a fixture into event-leaf rows (skips blank lines and `#` comments).
    fn parse(raw: &str) -> Self {
        let leaves = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.as_bytes().to_vec())
            .collect();
        Self { leaves }
    }

    /// The single genesis [`ChainLink`]: content-addressed (`checkpoint_id` is the
    /// Merkle root) with no predecessor.
    fn genesis_link(&self) -> ChainLink {
        let root = merkle_root(&self.leaves.iter().map(|l| leaf_hash(l)).collect::<Vec<_>>());
        ChainLink {
            checkpoint_id: root,
            merkle_root: root,
            prev: None,
        }
    }
}

impl ChainSource for FixtureSource {
    fn links(&self) -> Result<Vec<ChainLink>> {
        Ok(vec![self.genesis_link()])
    }
}

/// The per-shard artifact holding a checkpoint's Merkle root (lowercase hex). When
/// absent the checkpoint id is treated as its own root (a content-addressed
/// checkpoint). The exact upstream artifact name is `(verify)` at release-cut.
const MERKLE_ROOT_ARTIFACT: &str = "merkle_root.txt";

/// The **git-authoritative** chain source: re-derives the checkpoint chain
/// by walking `rift/checkpoints/v1` through the read-only gix [`SubstrateReader`]
/// — the production replacement for the committed fixture. Each shard yields a
/// [`ChainLink`] whose `prev` comes from the substrate's deterministic walk order,
/// so a re-walk of the same branch is byte-stable.
struct GitCheckpointSource {
    links: Vec<ChainLink>,
}

impl GitCheckpointSource {
    /// Build the chain by walking `reader`'s checkpoint shards and reading each
    /// shard's Merkle-root artifact (falling back to the checkpoint id as its own
    /// root). Deterministic: the substrate walks shards in sorted id order.
    ///
    /// **TWO ERAS OF CHECKPOINT ID, and the filter is PARSEABILITY — never the
    /// dialect.** A chain position is a [`Sha256Id`] by type, so an id that is
    /// not one cannot occupy one. This used to `?` on the first such id, which
    /// turned every repository holding a pre-Rift shard into an internal error
    /// (exit 70) — the standing read-compatibility promise is that those
    /// repositories stay *readable*, and aborting is the loudest possible way
    /// to break that promise. Measured before it was written: the two committed legacy
    /// fixtures carry a 12-hex id (`ab34cd56ef78`) and a 26-char ULID
    /// (`01JQZ8T9WQKX3M7VN2P4RB6CDE`), so both short forms are real.
    ///
    /// Filtering on the DIALECT instead would have regressed the other half: a
    /// legacy shard whose id *is* 64 hex characters takes a chain position today
    /// and still must, or an honest repository moves from clean to a false
    /// TAMPER on the missing link. So the rule is about the id's shape and
    /// nothing else.
    ///
    /// **`prev` is re-derived, not copied.** `CheckpointShard::prev` is "the
    /// prior id in walk order", so a shard following a dropped one names a
    /// predecessor that has no chain position — and a second `?` there aborted
    /// on exactly the repositories the first one did. Each retained link's
    /// `prev` is therefore the last *retained* id: the nearest preceding shard
    /// whose id parses. When every id parses that is byte-identical to the old
    /// behaviour (the walk sets `prev` to the previous element), so this is a
    /// generalization rather than a change.
    ///
    /// **A dropped shard is not a silent one.** `shard_passes` walks the same
    /// list independently: a dropped LEGACY shard is counted in
    /// `VerifySummary::pre_rift_shards` and reported, and a NATIVE shard with an
    /// unaddressable id still fails `checkpoint_id_pass` — no era's derivation
    /// grants an id no derivation could have produced — so dropping it from the
    /// chain removes a crash, not a check.
    ///
    /// # Errors
    /// Substrate read errors propagate. A malformed id is a skip, not an error.
    fn from_reader(reader: &impl SubstrateReader) -> Result<Self> {
        let mut links = Vec::new();
        let mut last_retained: Option<Sha256Id> = None;
        for shard in reader.checkpoints()? {
            let Ok(checkpoint_id) = Sha256Id::parse_hex(&shard.checkpoint_id) else {
                continue;
            };
            let merkle_root = recorded_root(reader, &shard.checkpoint_id)?.unwrap_or(checkpoint_id);
            links.push(ChainLink {
                checkpoint_id,
                merkle_root,
                prev: last_retained,
            });
            last_retained = Some(checkpoint_id);
        }
        Ok(Self { links })
    }
}

impl ChainSource for GitCheckpointSource {
    fn links(&self) -> Result<Vec<ChainLink>> {
        Ok(self.links.clone())
    }
}

/// Parse a trimmed 64-char SHA-256 hex artifact into a [`Sha256Id`]; `None` if the
/// bytes are not exactly one valid hex digest (tolerates a trailing newline).
fn parse_root_hex(bytes: &[u8]) -> Option<Sha256Id> {
    let text = std::str::from_utf8(bytes).ok()?;
    Sha256Id::parse_hex(text.trim()).ok()
}

/// **The one derivation of a shard's recorded Merkle root**, in the order the
/// writers actually use it.
///
/// `metadata.json`'s `merkle_root` first, because that is the field the native
/// archive writes and the ONLY field the artifact bridge will record a chain row
/// from. Then `merkle_root.txt`, the legacy dialect's separate artifact. `None`
/// means neither exists and the caller falls back to the id (a content-addressed
/// checkpoint is its own root).
///
/// **Measured on this repository before it was written** — the same lesson, one
/// field over: all 102 native shards carry `merkle_root` in `metadata.json` and
/// none carries a `merkle_root.txt`. Reading only the artifact therefore fell
/// through to the id on every single row, while the bridge recorded the metadata
/// value — two writers, one fact, disagreeing everywhere. That is invisible
/// while `verify` seeds the projection it compares against, and it is a red
/// verify on every repository the moment it stops.
fn recorded_root(reader: &impl SubstrateReader, id: &str) -> Result<Option<Sha256Id>> {
    use rift_recall_substrate::parse;

    if let Some(bytes) = reader.read_artifact(id, "metadata.json")? {
        if let Some(hex) = parse::parse_metadata(&bytes, id).value.merkle_root {
            if let Ok(root) = Sha256Id::parse_hex(hex.trim()) {
                return Ok(Some(root));
            }
        }
    }
    Ok(reader
        .read_artifact(id, MERKLE_ROOT_ARTIFACT)?
        .and_then(|bytes| parse_root_hex(&bytes)))
}

/// The git-authoritative source for `repo_root`, or `None` when there is no
/// checkpoint branch yet (a fresh repo / pre-capture), so the caller falls back to
/// the M0 fixture skeleton. Opening a non-repository path is also `None`.
fn git_source(repo_root: &Path) -> Result<Option<GitCheckpointSource>> {
    let Ok(reader) = GixSubstrate::open(repo_root) else {
        return Ok(None); // not a readable git repo → fixture skeleton
    };
    if reader.checkpoints()?.is_empty() {
        return Ok(None); // no checkpoint branch yet
    }
    Ok(Some(GitCheckpointSource::from_reader(&reader)?))
}

/// The strategy recorded for a chain row seeded from the authoritative source.
///
/// The source knows the linkage and the roots; it does not know which capture
/// strategy produced each checkpoint, so this says exactly that instead of
/// guessing `manual-commit` or borrowing a fixture's name.
const SEEDED_STRATEGY: &str = "seeded-from-source";

/// Whether a verification run may populate an empty projection from the source
/// it is about to compare against.
///
/// **This is the self-fulfilment switch, and it exists so the answer is written
/// down at each call site rather than assumed.** Seeding an empty projection
/// from `source` and then comparing it to `source` proves nothing at all: it
/// made every fresh repository "verify clean" by construction, which is how a
/// projection could disagree with git for months without a single red run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Seeding {
    /// The M0 fixture skeleton only. There is no repository content to derive a
    /// projection from — the source IS a committed fixture — so the run proves
    /// the wiring (create, migrate, record, cross-check, report) and says so.
    Permitted,
    /// The git-authoritative path. The projection must have been built by an
    /// INDEPENDENT derivation (`rift rebuild`, or the levelling `run_at_with`
    /// performs) and this comparison only compares — it never writes a row.
    ///
    /// **Scope, because this enum exists so an auditor can answer "does verify
    /// prove anything?":** the guarantee is that the COMPARISON does not seed.
    /// The `rift verify` COMMAND levels first, so an empty
    /// projection is normally filled before this runs. What remains forbidden —
    /// and is what the guarantee was always about — is filling the projection
    /// from the comparand: levelling adds missing rows through
    /// `record_checkpoint_if_new`, which is `insert_or_ignore` and so can never
    /// overwrite a row that disagrees. `levelling_cannot_launder_a_planted_
    /// checkpoint_into_agreement` (hooks_chain_e2e) plants a genuine id with a wrong
    /// merkle root and pins the tamper verdict.
    Forbidden,
}

/// Open (creating + migrating if absent) the projection at `db_path` and verify
/// the stored chain against `source`, seeding it first only when `seeding`
/// permits.
async fn verify_projection(
    db_path: &Path,
    source: &impl ChainSource,
    seeding: Seeding,
) -> Result<VerifyReport> {
    let store = rift_store::open_capture_projection(db_path)?;
    if seeding == Seeding::Permitted && store.count("checkpoint").await? == 0 {
        for link in source.links()? {
            record_checkpoint(
                &store,
                &CheckpointRecord {
                    checkpoint_id: link.checkpoint_id,
                    // These rows are seeded from the AUTHORITATIVE source — git
                    // in production — so they are ordinary checkpoints, not
                    // fixtures. The old `"fixture-genesis"` literal put that
                    // word on every row of a real repository's chain, including
                    // the ones that were not genesis and had never seen a
                    // fixture; this repository's own projection still reads that way.
                    strategy: SEEDED_STRATEGY.to_string(),
                    // Not 0. Zero is a real instant, so a seeded row was
                    // indistinguishable from one genuinely dated 1970 — and this
                    // path seeds the WHOLE chain, so a projection read as
                    // fully epoch-dated. The sentinel says "unknown" in a way
                    // no capture time can collide with.
                    created_at: rift_shadow::CREATED_AT_UNKNOWN,
                    merkle_root: link.merkle_root,
                    prev: link.prev,
                    schema_version: 1,
                },
            )
            .await?;
        }
    }
    verify_against_source(&store, source).await
}

/// Verify under an explicit cache `root` and `repo` (the seam the e2e test drives
/// with a tempdir + an injected source).
async fn verify_under(
    root: &Path,
    repo: &RepoId,
    source: &impl ChainSource,
    seeding: Seeding,
) -> Result<VerifyReport> {
    verify_projection(&projection_db_under(root, repo), source, seeding).await
}

/// How many checkpoints the persisted projection holds.
async fn projection_checkpoints(db_path: &Path) -> Result<i64> {
    let store = rift_store::open_capture_projection(db_path)?;
    store.count("checkpoint").await
}

/// The granular verify verdict. Ordered by severity: a tamper beats a
/// redaction problem beats a structure problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Everything re-derived and matched.
    Ok,
    /// Git ↔ recomputed / projection mismatch (exit 1 — a security incident).
    Tamper(String),
    /// A shard's redaction report is missing/invalid (exit 3).
    Redaction(String),
    /// A shard artifact is malformed, or a derived artifact is INCOMPLETE — an
    /// un-recorded chain position, an unbuilt projection (exit 5 — not tamper).
    Structure(String),
}

/// Severity rank, so a run reports its worst finding and never downgrades.
fn rank(v: &Verdict) -> u8 {
    match v {
        Verdict::Tamper(_) => 3,
        Verdict::Redaction(_) => 2,
        Verdict::Structure(_) => 1,
        Verdict::Ok => 0,
    }
}

/// The worse of two verdicts.
fn worse(a: Verdict, b: Verdict) -> Verdict {
    if rank(&b) > rank(&a) {
        b
    } else {
        a
    }
}

/// Name at most three ids out of `ids`, with the count.
fn name_some(ids: &[String]) -> String {
    let shown: Vec<&str> = ids.iter().take(3).map(String::as_str).collect();
    if ids.len() > shown.len() {
        format!("{} (+{} more)", shown.join(", "), ids.len() - shown.len())
    } else {
        shown.join(", ")
    }
}

/// **The 1:1 check.** Every checkpoint on the branch has exactly one position in
/// `refs/rift/chain/v1`, and every position names a checkpoint on the branch.
///
/// Three findings, each with its own diagnostic, because they have three
/// different causes and three different answers:
///
/// 1. **the orders disagree** — both sources have the checkpoint and put it in
///    different places. One of two append-only histories was rewritten, so the
///    diagnostic says which position each source claims. Tamper.
/// 2. **an orphan entry** — a position names a checkpoint the branch does not
///    carry. The branch was rewritten, or this chain is another repository's.
///    Nothing can heal it. Tamper.
/// 3. **an un-chained checkpoint** — a shard with no position. The order has
///    not caught up: nothing was rewritten, and the next archive heals it (or
///    `rift chain init`, on a repository that never had a chain). Structure —
///    incomplete, not falsified.
///
/// Checked in that order, because a disagreement makes the other two counts
/// meaningless.
fn chain_completeness(work_tree: &Path) -> Verdict {
    use rift_git::chain;
    use rift_shadow::backend::NATIVE_CHECKPOINT_REF as CHECKPOINT_BRANCH;

    let Ok(repo) = rift_git::discover(work_tree) else {
        return Verdict::Ok; // not a repository: there is no order to check
    };
    let git_dir = &repo.git_dir;
    let branch = match chain::branch_order(git_dir, CHECKPOINT_BRANCH) {
        Ok(b) => b,
        // A non-linear checkpoint branch has no single write order to check
        // against. Reported, never linearized around.
        Err(e) => return Verdict::Structure(format!("the checkpoint branch: {e}")),
    };
    let order = match chain::read_order(git_dir) {
        Ok(o) => o,
        Err(e) => return Verdict::Structure(format!("the chain ref: {e}")),
    };

    // Multiplicity before membership: `orphan_entries`/`missing_entries` are
    // set filters and the zip below compares positions, so a checkpoint
    // recorded at TWO chain positions is invisible to all three when every id
    // is also present once on the branch — an adversarial review planted
    // exactly that (branch of 1, ref of 2, verify said OK). The product cannot write the state
    // (`record_archived` no-ops on a recorded id; `init_from_branch` refuses
    // an existing ref), but verify is the tamper gate and a duplicate entry
    // is the mirror of the fork this ref exists to prevent.
    {
        let mut seen = std::collections::HashSet::new();
        for id in &order {
            if !seen.insert(id) {
                return Verdict::Tamper(format!(
                    "{id}: the chain records this checkpoint at more than one position"
                ));
            }
        }
    }

    if let Some(d) = crate::chain::first_disagreement(&order, &branch) {
        return Verdict::Tamper(format!(
            "{}: the chain and the checkpoint branch disagree — {d}",
            d.branch
        ));
    }
    match chain::orphan_entries(git_dir, &branch) {
        Ok(orphans) if !orphans.is_empty() => {
            return Verdict::Tamper(format!(
                "{}: {} chain entr{} name a checkpoint that is not on the branch ({})",
                orphans.first().map_or("", String::as_str),
                orphans.len(),
                if orphans.len() == 1 { "y" } else { "ies" },
                name_some(&orphans)
            ))
        }
        Ok(_) => {}
        Err(e) => return Verdict::Structure(format!("the chain ref: {e}")),
    }
    match chain::missing_entries(git_dir, &branch) {
        Ok(missing) if !missing.is_empty() => Verdict::Structure(format!(
            "{}: {} checkpoint(s) on the branch have no chain entry ({}) — run `rift chain init` \
             if this repository has never recorded its order",
            missing.first().map_or("", String::as_str),
            missing.len(),
            name_some(&missing)
        )),
        Ok(_) => Verdict::Ok,
        Err(e) => Verdict::Structure(format!("the chain ref: {e}")),
    }
}

/// The verify result the CLI renders + maps to the granular exit contract.
#[derive(Debug, Clone)]
pub(crate) struct VerifySummary {
    /// Checkpoints cross-checked against the projection chain.
    pub(crate) checkpoints: usize,
    /// Shards that got the per-shard redaction/Merkle passes.
    pub(crate) shards_checked: usize,
    /// Shards accepted under the **vacuous Merkle era** — native shards written
    /// before the root was derived from the shard's own leaves, so their
    /// recorded root is the empty-set constant and commits to nothing. That
    /// constant is now asserted rather than assumed, and their capture is bound
    /// by the checkpoint-id pass instead.
    ///
    /// Reported rather than swallowed. A verify that quietly skipped part of its
    /// own job and still said OK is precisely the shape of self-certification
    /// this program keeps finding; the operator gets to see how much of their
    /// history predates the guarantee.
    pub(crate) vacuous_era_shards: usize,
    /// Shards of the pre-Rift dialect (read from the legacy ref — never a
    /// native shard with a stripped label), exempt from every native-only pass.
    pub(crate) pre_rift_shards: usize,
    /// Which authoritative source was used.
    pub(crate) source: &'static str,
    /// The worst finding.
    pub(crate) verdict: Verdict,
}

impl VerifySummary {
    /// Whether this run is clean — which is *necessary* for it to contribute to
    /// the soak streak, and on its own not sufficient.
    ///
    /// Exactly [`Verdict::Ok`]. Not "no tamper", not "nothing serious" — a
    /// redaction or structure finding is a broken run, and a streak that counted
    /// them would certify the thing it was built to catch.
    ///
    /// The second requirement is a [`Witness`]: the soak counts condensations, so a clean run that verified nothing in particular
    /// — every `rift verify` an operator types — contributes nothing at all.
    /// This predicate cannot see that half, and saying so here is the point.
    pub(crate) fn is_clean(&self) -> bool {
        matches!(self.verdict, Verdict::Ok)
    }

    /// The verdict's stable token, for a marker line. Category only: the
    /// verdict's detail can carry a checkpoint id and this string is written to
    /// a file the next hook prints.
    pub(crate) fn verdict_token(&self) -> &'static str {
        match self.verdict {
            Verdict::Ok => "ok",
            Verdict::Tamper(_) => "tamper",
            Verdict::Redaction(_) => "redaction",
            Verdict::Structure(_) => "structure",
        }
    }
}

/// The header both era-dependent passes read, parsed from `metadata.json` once.
struct ShardHeader {
    /// The canonicalization epoch the shard declares, or **0** when the field
    /// (or the file) is absent.
    ///
    /// The era a shard is verified under is read off the shard itself and
    /// nowhere else. Never a date, never the branch, never "shards after commit
    /// X": a repository can be cloned, rebased, re-imported or restored from a
    /// bundle, and none of those change what a shard says about itself. What
    /// the era can no longer buy is the pre-Rift *exemption* — that is the
    /// DIALECT's (the ref the shard was read from), because the one thing
    /// editing a shard cannot rewrite is where the shard is.
    canon_version: u32,
    /// The native session ids the checkpoint covers.
    sessions: Vec<String>,
    /// The `tasks/<tool-use-id>/` subtrees the shard claims to carry —
    /// the reader's enumeration; empty for a taskless
    /// shard and for every pre-task shard.
    tasks: Vec<String>,
    /// The recorded `task_tokens` rollup `(input, output)`, when present —
    /// re-summed against the task subtrees' own metadata below.
    task_tokens: Option<(u64, u64)>,
}

/// Read a shard's header. One `metadata.json` read for both fields — the
/// era pass and the id pass used to be two reads of one file.
fn shard_header(reader: &impl SubstrateReader, id: &str) -> Result<ShardHeader> {
    use rift_recall_substrate::parse;

    let Some(bytes) = reader.read_artifact(id, "metadata.json")? else {
        return Ok(ShardHeader {
            canon_version: 0,
            sessions: Vec::new(),
            tasks: Vec::new(),
            task_tokens: None,
        });
    };
    let meta = parse::parse_metadata(&bytes, id).value;
    // The task list rides the same document; parsed leniently and locally
    // (the shared lenient metadata type is another crate's surface, and a
    // malformed list simply enumerates nothing — whose missing leaves the
    // root/id passes then refuse).
    let doc = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    let tasks = doc
        .as_ref()
        .and_then(|v| v.get("tasks").cloned())
        .and_then(|t| serde_json::from_value::<Vec<String>>(t).ok())
        .unwrap_or_default();
    let task_tokens = doc
        .as_ref()
        .and_then(|v| v.get("task_tokens"))
        .and_then(|t| {
            Some((
                t.get("input_tokens").and_then(serde_json::Value::as_u64)?,
                t.get("output_tokens").and_then(serde_json::Value::as_u64)?,
            ))
        });
    Ok(ShardHeader {
        canon_version: meta.canon_version.unwrap_or(0),
        sessions: meta.sessions,
        tasks,
        task_tokens,
    })
}

/// The one `canon_version` whose `merkle_root` is a constant rather than a
/// claim — see `STRICT_MERKLE_MIN_CANON_VERSION`.
const VACUOUS_MERKLE_CANON_VERSION: u32 = 1;

/// What the per-shard passes found.
struct ShardPasses {
    /// Shards that got the redaction/Merkle passes.
    checked: usize,
    /// Shards in the **vacuous era** — native-dialect shards below the strict
    /// root era: epoch 1 (whose recorded root is the empty-set constant every
    /// native shard published before 2026-08-12, asserted by
    /// [`vacuous_root_pass`]) and a shard declaring no era at all (a deleted
    /// `canon_version` parses as epoch 0 — its root proves nothing, and its id
    /// still must address its bytes). Accepted, and counted so the acceptance
    /// is visible instead of assumed.
    vacuous_era: usize,
    /// Shards of the **pre-Rift dialect** — read from the legacy checkpoint
    /// ref, exempt from every native-only pass by invariant §1.5.
    ///
    /// Counted by DIALECT (the ref the shard was read from), never by anything
    /// the shard says about itself. A native shard whose `canon_version` was
    /// deleted used to land in this exemption; it now stays in
    /// the native passes at epoch 0, where its id must still address its own
    /// bytes. A non-zero count here therefore means real legacy history —
    /// never a stripped label.
    pre_rift: usize,
    /// The worst finding.
    worst: Verdict,
}

/// The strict Merkle pass: for a shard whose era says its recorded root is a
/// claim about these bytes, re-derive the root from the artifacts and demand a
/// match.
///
/// The leaves are re-derived by `shard_merkle_root` — literally the function the
/// archive writer calls, over the artifacts read back out of the shard — so the
/// writer and the verifier cannot drift apart into two answers about one fact.
fn strict_merkle_pass(
    reader: &impl SubstrateReader,
    id: &str,
    native: &NativeArtifacts,
    report: Option<&[u8]>,
) -> Result<Option<Verdict>> {
    use rift_shadow::backend::shard_merkle_root;

    let (Some(content), Some(report)) = (native.content.as_deref(), report) else {
        // Both artifacts are written unconditionally by every shard of this
        // era, so one being gone is a removal of content the shard's own
        // root commits to — not an incompleteness.
        return Ok(Some(Verdict::Tamper(format!(
            "{id}: a leaf artifact its recorded Merkle root commits to is missing"
        ))));
    };
    let task_leaves = native.task_leaves();
    let session_meta_leaves = native.session_meta_leaves();
    let derived =
        shard_merkle_root(&native.leaves(content, report, &task_leaves, &session_meta_leaves));
    Ok(match recorded_root(reader, id)? {
        Some(recorded) if recorded == derived => None,
        Some(_) => Some(Verdict::Tamper(format!(
            "{id}: the Merkle root re-derived from this shard's artifacts does not match the root \
             it records"
        ))),
        None => Some(Verdict::Structure(format!(
            "{id}: records no Merkle root, but its canon_version says it must"
        ))),
    })
}

/// The vacuous era's one checkable claim: an epoch-1 shard's recorded root is
/// the empty-leaf-set constant, on every shard, because `condense` handed the
/// archive an empty event set and took the root over it.
///
/// This exists because the summary told operators that vacuous-era shards "are
/// accepted with their empty-set constant" while nothing anywhere asserted it
/// — a verifier caught the gap. A sentence about behaviour that no test would
/// redden is an argument, not a claim.
///
/// **It does not close the relabelling hole and is not meant to.** A tamperer
/// who rewrites `canon_version` to 1 writes the constant too; what catches them
/// is the checkpoint-id pass below, which runs on this era as well. What this
/// catches is a shard claiming an era whose one invariant it does not satisfy —
/// no Rift build ever wrote one, measured across all 103 shards here.
fn vacuous_root_pass(reader: &impl SubstrateReader, id: &str) -> Result<Option<Verdict>> {
    Ok(match recorded_root(reader, id)? {
        Some(root) if root == rift_hashchain::empty_root() => None,
        _ => Some(Verdict::Tamper(format!(
            "{id}: declares the vacuous canonicalization era but does not record that era's \
             empty-leaf-set root"
        ))),
    })
}

/// **The checkpoint-id pass** — the binding a tamperer cannot recompute.
///
/// The recorded Merkle root lives in a file the tamperer also controls, so
/// editing `content.redacted` and recomputing `metadata.json`'s `merkle_root`
/// passed every strict check. The shard's *path*
/// is not in any file, so re-deriving it is the one binding writing a file cannot
/// fix, and edited content stops addressing the shard it sits in.
///
/// **Which derivation applies is the shard's declared era.** From epoch 3 the id
/// is `sha256(session ‖ \0 ‖ sha256_hex(root))` over the root RE-DERIVED here
/// from the artifacts — so every content artifact, `full.jsonl` and the
/// transcript included, is bound by the path. Below that the id bound only
/// `content.redacted` and `tool_calls.jsonl`, and both of that era's derivations
/// are accepted: 102 of this repository's 103 shards match one and one matches
/// the other, so a checker that insisted on a single formula would call an honest
/// shard tampered.
///
/// Runs on **every shard of the native dialect, whatever era it declares** —
/// including one that declares none: a deleted `canon_version` parses as epoch
/// 0, whose accepted set is the pre-root rules, and no era's rule grants an id
/// to edited bytes. That closes the label-stripping escape
/// the same way the epoch-1 relabel was closed: the shard's path is evidence
/// of the rule that minted it.
fn checkpoint_id_pass(
    id: &str,
    sessions: &[String],
    canon_version: u32,
    native: &NativeArtifacts,
    report: Option<&[u8]>,
) -> Option<Verdict> {
    let (Some(content), Some(report)) = (native.content.as_deref(), report) else {
        // Every native shard writes both; the absence of either is a removal of
        // bytes the id addresses, not an incompleteness.
        return Some(Verdict::Tamper(format!(
            "{id}: an artifact its checkpoint id addresses is missing"
        )));
    };
    let task_leaves = native.task_leaves();
    let session_meta_leaves = native.session_meta_leaves();
    let candidates = rift_shadow::condense::checkpoint_ids_for_shard(
        sessions,
        canon_version,
        &native.leaves(content, report, &task_leaves, &session_meta_leaves),
    );
    // An empty candidate set is `checkpoint_ids_for_shard`'s "no era rule
    // grants this shard an id at all" — a session set the rules refuse
    // (none, or NUL-carrying), or a multi-session shard declaring a
    // single-session era. No writer minted one, so falling through to the
    // mismatch verdict below is the honest outcome, not an accident.
    if candidates.iter().any(|c| c == id) {
        None
    } else {
        Some(Verdict::Tamper(format!(
            "{id}: the checkpoint id does not address this shard's own bytes"
        )))
    }
}

/// The content artifacts a native shard writes and both native passes read.
///
/// Read ONCE per shard and shared, so the Merkle pass and the id pass cannot
/// end up disagreeing about what the shard holds — and so binding the id to the
/// root cost zero extra object reads: `full.jsonl` moved here from inside the
/// Merkle pass, where it was already being read, and both passes now take it.
struct NativeArtifacts {
    content: Option<Vec<u8>>,
    tool_calls: Option<Vec<u8>>,
    full: Option<Vec<u8>>,
    transcript_compact: Option<Vec<u8>>,
    transcript_full: Option<Vec<u8>>,
    /// The `tasks/<tool-use-id>/` subtrees' bytes,
    /// enumerated from the parent metadata's `tasks` list — `SubstrateReader`
    /// has no tree listing and does not need one: a task hidden from the list
    /// is a leaf missing from the re-derivation, so the root and the id both
    /// refuse.
    tasks: Vec<TaskArtifactBytes>,
    /// The `sessions/<index>/metadata.json` bytes of a multi-session shard
    /// of a multi-session shard, `(index_name, bytes)` — empty below two sessions.
    session_metas: Vec<(String, Vec<u8>)>,
    /// The four `digest.*` artifacts, each absent on every
    /// pre-digest shard — absence re-derives the pre-digest leaf sequence.
    digest_failures: Option<Vec<u8>>,
    digest_intent: Option<Vec<u8>>,
    digest_log: Option<Vec<u8>>,
    digest_shape: Option<Vec<u8>>,
}

/// One task subtree's artifact bytes as read back out of the shard.
struct TaskArtifactBytes {
    /// The id the parent metadata listed the task under (also its tree name).
    tool_use_id: String,
    /// `tasks/<id>/metadata.json` — empty when listed but missing, which can
    /// never reproduce a writer's leaf, so the mismatch surfaces as tamper.
    metadata: Vec<u8>,
    transcript_compact: Option<Vec<u8>>,
    transcript_full: Option<Vec<u8>>,
}

impl NativeArtifacts {
    /// Read every artifact the native passes derive from, once per shard.
    /// `session_count` drives the multi-session dirs: at N ≥ 2 the shard
    /// carries `sessions/<index>/metadata.json` for every index; below that
    /// there are none, by the writer's own contract.
    fn read(
        reader: &impl SubstrateReader,
        id: &str,
        task_ids: &[String],
        session_count: usize,
    ) -> Result<Self> {
        use rift_shadow::backend::{TRANSCRIPT_COMPACT_NAME, TRANSCRIPT_FULL_NAME};
        let mut session_metas = Vec::new();
        if session_count >= 2 {
            for i in 0..session_count {
                session_metas.push((
                    i.to_string(),
                    reader
                        .read_artifact(id, &format!("sessions/{i}/metadata.json"))?
                        .unwrap_or_default(),
                ));
            }
        }
        let mut tasks = Vec::with_capacity(task_ids.len());
        for tid in task_ids {
            tasks.push(TaskArtifactBytes {
                tool_use_id: tid.clone(),
                metadata: reader
                    .read_artifact(id, &format!("tasks/{tid}/metadata.json"))?
                    .unwrap_or_default(),
                transcript_compact: reader
                    .read_artifact(id, &format!("tasks/{tid}/{TRANSCRIPT_COMPACT_NAME}"))?,
                transcript_full: reader
                    .read_artifact(id, &format!("tasks/{tid}/{TRANSCRIPT_FULL_NAME}"))?,
            });
        }
        Ok(Self {
            content: reader.read_artifact(id, "content.redacted")?,
            tool_calls: reader.read_artifact(id, "tool_calls.jsonl")?,
            full: reader.read_artifact(id, "full.jsonl")?,
            // The writer's own name constants, so the two sides cannot drift.
            transcript_compact: reader.read_artifact(id, TRANSCRIPT_COMPACT_NAME)?,
            transcript_full: reader.read_artifact(id, TRANSCRIPT_FULL_NAME)?,
            tasks,
            session_metas,
            digest_failures: reader.read_artifact(id, rift_shadow::digest::DIGEST_FAILURES_NAME)?,
            digest_intent: reader.read_artifact(id, rift_shadow::digest::DIGEST_INTENT_NAME)?,
            digest_log: reader.read_artifact(id, rift_shadow::digest::DIGEST_LOG_NAME)?,
            digest_shape: reader.read_artifact(id, rift_shadow::digest::DIGEST_SHAPE_NAME)?,
        })
    }

    /// The per-session-metadata leaf views [`Self::leaves`] borrows.
    fn session_meta_leaves(&self) -> Vec<rift_shadow::backend::SessionMetaLeaf<'_>> {
        self.session_metas
            .iter()
            .map(|(name, bytes)| rift_shadow::backend::SessionMetaLeaf {
                index_name: name,
                metadata: bytes,
            })
            .collect()
    }

    /// The task-subtree leaf views [`Self::leaves`] borrows — a separate call
    /// because the borrowed vector must outlive the `ShardLeafBytes` it feeds.
    fn task_leaves(&self) -> Vec<rift_shadow::backend::TaskLeafBytes<'_>> {
        self.tasks
            .iter()
            .map(|t| rift_shadow::backend::TaskLeafBytes {
                tool_use_id: &t.tool_use_id,
                metadata: &t.metadata,
                transcript_compact: t.transcript_compact.as_deref(),
                transcript_full: t.transcript_full.as_deref(),
            })
            .collect()
    }

    /// The leaf view both passes feed to the writer's own derivations.
    fn leaves<'a>(
        &'a self,
        content: &'a [u8],
        report: &'a [u8],
        tasks: &'a [rift_shadow::backend::TaskLeafBytes<'a>],
        session_metas: &'a [rift_shadow::backend::SessionMetaLeaf<'a>],
    ) -> rift_shadow::backend::ShardLeafBytes<'a> {
        rift_shadow::backend::ShardLeafBytes {
            content_redacted: content,
            full_jsonl: self.full.as_deref(),
            redaction_report: report,
            tool_calls: self.tool_calls.as_deref(),
            transcript_compact: self.transcript_compact.as_deref(),
            transcript_full: self.transcript_full.as_deref(),
            tasks,
            session_metas,
            digests: rift_shadow::backend::DigestLeafBytes {
                failures: self.digest_failures.as_deref(),
                intent: self.digest_intent.as_deref(),
                log: self.digest_log.as_deref(),
                shape: self.digest_shape.as_deref(),
            },
        }
    }
}

/// The **recorded-leaf** recompute: when a shard writes its own leaf list
/// (`merkle_leaf.txt`, the legacy dialect's artifact), the root over those leaves
/// must equal the root the shard records.
///
/// Runs on every shard of every era, which is why it is a named pass rather than
/// a branch inside the loop: a corrupt `merkle_leaf.txt` in a shard with no
/// `canon_version` at all must keep dying.
/// Returns every finding rather than the worst, so a malformed list and a
/// mismatching root are two facts and not one.
fn recorded_leaf_pass(reader: &impl SubstrateReader, id: &str) -> Result<Vec<Verdict>> {
    use rift_recall_substrate::parse;

    let Some(bytes) = reader.read_artifact(id, "merkle_leaf.txt")? else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    let leaves = parse::parse_merkle_leaves(&bytes, id);
    if !leaves.is_clean() {
        found.push(Verdict::Structure(format!("{id}: malformed merkle leaves")));
    }
    let parsed: Vec<Sha256Id> = leaves
        .value
        .iter()
        .filter_map(|h| Sha256Id::parse_hex(h).ok())
        .collect();
    if !parsed.is_empty() {
        let recomputed = merkle_root(&parsed);
        // The same derivation the chain comparison uses — one answer to "what
        // root did this shard record", or the recompute would be checking a
        // different claim than the chain does.
        let recorded = recorded_root(reader, id)?.or_else(|| Sha256Id::parse_hex(id).ok());
        if recorded.is_some_and(|recorded| recomputed != recorded) {
            found.push(Verdict::Tamper(format!(
                "{id}: recomputed Merkle root does not match the recorded root"
            )));
        }
    }
    Ok(found)
}

/// The task-subtree pass: the two claims a nested artifact
/// adds beyond its leaves.
///
/// **Name/bytes agreement** — the subtree name is the one byte sequence of a
/// task that is not itself a leaf; the metadata's `tool_use_id` is, so a
/// renamed subtree is exactly a name/bytes disagreement. No writer mints one:
/// both come from the same `ArchivedTask`.
///
/// **The rollup recompute** — the parent's recorded `task_tokens` must equal
/// the sum over the task subtrees' own (leaf-bound) metadata, including in
/// absence: a stripped rollup hides spend, an invented one claims spend no
/// task reported. The parent metadata is not a leaf, which is exactly why the
/// recompute exists (same treatment as the recorded Merkle root).
fn task_subtree_pass(id: &str, header: &ShardHeader, native: &NativeArtifacts) -> Vec<Verdict> {
    let mut found = Vec::new();
    let mut task_token_sum: Option<(u64, u64)> = None;
    for t in &native.tasks {
        let doc = serde_json::from_slice::<serde_json::Value>(&t.metadata).ok();
        let claimed = doc.as_ref().and_then(|v| {
            v.get("tool_use_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
        if claimed.as_deref().is_some_and(|c| c != t.tool_use_id) {
            found.push(Verdict::Tamper(format!(
                "{id}: task subtree '{}' carries metadata naming a different tool-use id",
                t.tool_use_id
            )));
        }
        let input = doc
            .as_ref()
            .and_then(|v| v.get("input_tokens"))
            .and_then(serde_json::Value::as_u64);
        let output = doc
            .as_ref()
            .and_then(|v| v.get("output_tokens"))
            .and_then(serde_json::Value::as_u64);
        if input.is_some() || output.is_some() {
            let (i, o) = task_token_sum.unwrap_or((0, 0));
            task_token_sum = Some((
                i.saturating_add(input.unwrap_or(0)),
                o.saturating_add(output.unwrap_or(0)),
            ));
        }
    }
    if header.task_tokens != task_token_sum {
        found.push(Verdict::Tamper(format!(
            "{id}: the recorded task-token rollup does not match the sum of the task subtrees"
        )));
    }
    found
}

/// Per-shard passes: re-verify each shard's
/// redaction report, recompute its Merkle root from the recorded leaves (the
/// legacy dialect's `merkle_leaf.txt`), and — for shards of the strict era —
/// re-derive the root from the shard's own artifacts and demand a match.
///
/// **The era gates only the last of those.** The first two run on every shard of
/// every era, because an era split that silenced a check that already worked
/// would be a hole wearing a migration's name (a corrupt `merkle_leaf.txt` in a
/// shard with no `canon_version` at all must keep dying).
fn shard_passes(reader: &impl SubstrateReader) -> Result<ShardPasses> {
    use rift_recall_substrate::parse;
    use rift_shadow::backend::STRICT_MERKLE_MIN_CANON_VERSION;

    let mut worst = Verdict::Ok;
    // Severity: Tamper > Redaction > Structure > Ok. Never downgrade.
    let mut raise = |v: Verdict| {
        let rank = |x: &Verdict| match x {
            Verdict::Tamper(_) => 3,
            Verdict::Redaction(_) => 2,
            Verdict::Structure(_) => 1,
            Verdict::Ok => 0,
        };
        if rank(&v) > rank(&worst) {
            worst = v;
        }
    };

    let shards = reader.checkpoints()?;
    let mut checked = 0usize;
    let mut vacuous_era = 0usize;
    let mut pre_rift = 0usize;
    for shard in &shards {
        let id = shard.checkpoint_id.as_str();
        checked += 1;

        // Redaction re-verification: the report must exist and be valid.
        let report_bytes = reader.read_artifact(id, "redaction_report.json")?;
        match &report_bytes {
            None => raise(Verdict::Redaction(format!(
                "{id}: missing redaction report"
            ))),
            Some(bytes) => {
                let out = parse::parse_redaction_report(bytes, id);
                if !out.is_clean() {
                    raise(Verdict::Structure(format!(
                        "{id}: redaction report is malformed"
                    )));
                } else if !out.value.is_valid() {
                    raise(Verdict::Redaction(format!(
                        "{id}: redaction report is invalid"
                    )));
                }
            }
        }

        for v in recorded_leaf_pass(reader, id)? {
            raise(v);
        }

        // Everything below is a NATIVE-only pass, gated on the DIALECT the
        // shard was read from — the one signal the shard's own bytes cannot
        // rewrite, because it is where the shard IS, not what it says. The
        // pre-Rift dialect is exempt (invariant §1.5: those ids were never
        // minted by any Rift derivation, and read-compat is forever) and
        // counted. This used to gate on the header's `canon_version`, and that
        // was the recorded escape: deleting the field moved a
        // NATIVE shard into the exemption and every native pass declined to
        // look at it. Now a native-ref shard with no era declares epoch 0 and
        // still owes its id — which no era's rule will grant to edited bytes.
        if shard.dialect != Dialect::Rift {
            pre_rift += 1;
            continue;
        }
        let header = shard_header(reader, id)?;

        let native = NativeArtifacts::read(reader, id, &header.tasks, header.sessions.len())?;

        for v in task_subtree_pass(id, &header, &native) {
            raise(v);
        }

        if header.canon_version >= STRICT_MERKLE_MIN_CANON_VERSION {
            if let Some(v) = strict_merkle_pass(reader, id, &native, report_bytes.as_deref())? {
                raise(v);
            }
        } else {
            vacuous_era += 1;
            if header.canon_version == VACUOUS_MERKLE_CANON_VERSION {
                if let Some(v) = vacuous_root_pass(reader, id)? {
                    raise(v);
                }
            }
        }

        // The checkpoint-id pass runs on every native shard, whatever its
        // cardinality. The single-session carve-out (skipped + counted) is
        // RETIRED: the derivation is defined for every
        // session set a writer can mint (N ≥ 1, NUL-free), and a set no rule
        // grants — none at all, or a multi-session shard declaring a
        // single-session era — yields an empty candidate set, which the pass
        // maps to tamper rather than to silence.
        if let Some(v) = checkpoint_id_pass(
            id,
            &header.sessions,
            header.canon_version,
            &native,
            report_bytes.as_deref(),
        ) {
            raise(v);
        }
    }
    Ok(ShardPasses {
        checked,
        vacuous_era,
        pre_rift,
        worst,
    })
}

/// `rift verify`: re-derive the chain from the authoritative source, cross-check
/// the projection, and run the per-shard redaction/Merkle passes. The outcome is
/// **returned**, not thrown — the CLI maps it to the granular exit contract.
///
/// # Errors
/// [`RiftError::Io`](rift_core::RiftError::Io)/[`RiftError::Storage`](rift_core::RiftError::Storage) if the projection cannot be opened
/// or migrated (internal problems — exit 70); never for tamper/redaction
/// findings, which are verdicts.
/// Verify against an explicit repository root — the CLI passes its resolved
/// cwd; the palette dispatcher passes the selected project's path (it must
/// never consult process cwd).
pub(crate) fn run_at(root: &std::path::Path) -> Result<VerifySummary> {
    // `None`: the operator's own `rift verify` witnesses no condensation, so it
    // records a run and NO witness row. That is not a policy check that could be
    // forgotten — it is the only value this call site can pass.
    run_at_with(root, Voice::Loud, None)
}

/// Whether a verification run narrates itself.
///
/// The soak's runs are `Quiet`: they happen inside the detached capture worker,
/// whose stdout is `/dev/null`, so a `println!` there is not merely useless — it
/// is the very silence loud reporting exists to prevent, dressed as output.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Voice {
    /// Print the repo/projection lines (the `rift verify` command).
    Loud,
    /// Say nothing; the caller reports.
    Quiet,
}

/// One soak observation: a verification run made **after a successful condense**,
/// off git's critical path, recorded append-only in `verification_run`.
///
/// This is the native-verify soak accruing from real commits. It is
/// deliberately the same code `rift verify` runs — a soak whose
/// oracle differs from the command operators trust would certify nothing.
pub(crate) fn soak_verify(root: &std::path::Path, witness: &Witness) -> Result<VerifySummary> {
    run_at_with(root, Voice::Quiet, Some(witness))
}

/// The condensation(s) a soak verification witnesses — **non-empty by
/// construction**.
///
/// The soak's population is condensations rather than verify invocations, and
/// this type is where that lives. The two
/// mechanisms this replaced were both provably unable to fire: gating
/// `accrue_soak` on an `Option` whose `None` arm was unreachable, and trusting
/// three separate writers of `verification_run` to agree about which of them
/// counted. Neither survives here, because the ONLY way to reach the witness
/// writer is to hold one of these, and the only way to hold one is to name a
/// checkpoint.
///
/// It is a head plus a tail rather than a `Vec` for the same reason: a `Vec`
/// can be empty, and an empty witness list would silently record an unwitnessed
/// run — the `rift verify` inflation route wearing the soak's own name.
pub(crate) struct Witness {
    head: String,
    rest: Vec<String>,
}

impl Witness {
    /// One condensation: the post-commit archive and the attach verb.
    pub(crate) fn of(checkpoint: &str) -> Self {
        Self {
            head: checkpoint.to_owned(),
            rest: Vec::new(),
        }
    }

    /// Several condensations under one verify: doctor's stale-session sweep
    /// finalizes N sessions and then verifies once.
    ///
    /// `None` when the slice is empty, and that arm is REACHABLE — a sweep that
    /// finalized nothing is the ordinary case, and it must record no run at all
    /// rather than an unwitnessed one.
    pub(crate) fn of_all(checkpoints: &[String]) -> Option<Self> {
        let (head, rest) = checkpoints.split_first()?;
        Some(Self {
            head: head.clone(),
            rest: rest.to_vec(),
        })
    }

    /// Every checkpoint this run witnesses, head first.
    fn checkpoints(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.head.as_str()).chain(self.rest.iter().map(String::as_str))
    }
}

fn run_at_with(
    root: &std::path::Path,
    voice: Voice,
    witness: Option<&Witness>,
) -> Result<VerifySummary> {
    // LEVEL BEFORE COMPARING, on every path into verify, and here rather than in
    // the callers.
    //
    // The archive writes its shard to git; nothing writes the matching row, so a
    // projection is routinely BEHIND git by exactly the checkpoints just
    // condensed — which the comparison below correctly reads as tamper. Those
    // are false positives, and a divergence vetoes the soak
    // PERMANENTLY (`verification_divergence` is append-only), so the answer is
    // to stop generating them rather than to stop counting them.
    //
    // It lives INSIDE the entry point because the last bug here was a caller
    // skipping it: doctor's sweep called `soak_verify` directly, bypassed the
    // levelling its sibling did, and recorded a dirty run every single time. A
    // step that must not be skipped does not belong in the callers — same
    // reasoning as `Witness` being unconstructible without a checkpoint.
    //
    // This is NOT the seeding `run_async` refuses. Levelling is the BRIDGE's
    // derivation from the git shards (`ingest_from_substrate`, an idempotent
    // set-union); the check below is the CHECKER's independent re-derivation of
    // the chain from the same source. Two independent derivations agreeing is
    // the check. What stays forbidden is verify filling the projection from the
    // comparand it is about to compare against, which agrees by construction.
    //
    // Best-effort: a levelling failure leaves the comparison to report the gap
    // it was going to report anyway.
    let _ = crate::hooks::level_projection(root);
    // `main` is synchronous; the store seam is async (Turso-shaped). Drive it on a
    // short-lived current-thread runtime confined to this command — the async
    // readers (Canopy, the serve path) get their own runtime when they land.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    rt.block_on(run_async(root, voice, witness))
}

async fn run_async(
    root: &std::path::Path,
    voice: Voice,
    witness: Option<&Witness>,
) -> Result<VerifySummary> {
    let cwd = root.to_path_buf();
    // The projection this repository's family owns — `rift
    // verify` run from a linked worktree verifies the family's chain, not an
    // empty one keyed by the member's own path.
    let repo = crate::family::resolve_at(&cwd)?.family_id;
    let root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    let db = projection_db_under(&root, &repo);

    // Prefer the git-authoritative source (re-derive from `rift/checkpoints/v1`);
    // fall back to the committed fixture skeleton on a fresh repo with no branch.
    let (chain_outcome, source_label, shard_outcome, order_verdict) = match git_source(&cwd)? {
        Some(source) => {
            // The ORDER first: 1:1 with the branch, both directions. It reads
            // git alone, and it is what says whether the order is complete
            // before anything is compared against it.
            let order = chain_completeness(&cwd);
            // THIS COMPARISON WRITES NOTHING. The projection was levelled at the
            // top of `run_at_with` by an independent
            // derivation — the bridge's ingest of the git shards — and what that
            // levelling cannot do is overwrite a row that disagrees, because
            // `record_checkpoint_if_new` is `insert_or_ignore`. So the branch
            // below is reached only when levelling FAILED or every shard was
            // refused; it is no longer the ordinary state of a fresh repository,
            // and it is still never filled in from the comparand — which is what
            // made a fresh repository verify clean by construction.
            let chain = if projection_checkpoints(&db).await? == 0 {
                None
            } else {
                Some(verify_under(&root, &repo, &source, Seeding::Forbidden).await)
            };
            // The per-shard passes re-open the reader (cheap; object access only).
            let shards = match GixSubstrate::open(&cwd) {
                Ok(reader) => shard_passes(&reader)?,
                Err(_) => ShardPasses {
                    checked: 0,
                    vacuous_era: 0,
                    pre_rift: 0,
                    worst: Verdict::Ok,
                },
            };
            (chain, "git", shards, order)
        }
        None => (
            // The M0 skeleton, and the only place seeding survives: there is no
            // repository content to derive a projection from.
            Some(
                verify_under(
                    &root,
                    &repo,
                    &FixtureSource::parse(FIXTURE),
                    Seeding::Permitted,
                )
                .await,
            ),
            "fixture (no checkpoint branch yet)",
            ShardPasses {
                checked: 0,
                vacuous_era: 0,
                pre_rift: 0,
                worst: Verdict::Ok,
            },
            Verdict::Ok,
        ),
    };

    let (checkpoints, chain_verdict) = match chain_outcome {
        // Nothing was compared, because there was nothing to compare.
        None => (
            0,
            Verdict::Structure(
                "the projection holds no checkpoints; run `rift rebuild`. Verify does not seed it \
                 — a projection filled in from the source it is then compared against agrees by \
                 construction and proves nothing."
                    .to_owned(),
            ),
        ),
        Some(Ok(report)) => (report.checkpoints, Verdict::Ok),
        // A chain divergence is a tamper verdict; it outranks any shard finding.
        Some(Err(rift_core::RiftError::Integrity(m))) => (0, Verdict::Tamper(m)),
        Some(Err(e)) => return Err(e),
    };
    let summary = VerifySummary {
        checkpoints,
        shards_checked: shard_outcome.checked,
        vacuous_era_shards: shard_outcome.vacuous_era,
        pre_rift_shards: shard_outcome.pre_rift,
        source: source_label,
        verdict: worse(worse(order_verdict, chain_verdict), shard_outcome.worst),
    };
    if voice == Voice::Loud {
        println!("  repo_id:    {repo}");
        println!("  projection: {}", db.display());
    }
    record_verification(&root, &cwd, &repo, &summary, witness).await;
    Ok(summary)
}

/// Persist the verification outcome append-only (`verification_run` + one
/// `verification_divergence` per finding + one `verification_witness` per
/// condensation this run witnessed). Best-effort: verify's exit code is the
/// contract; a store hiccup must not turn a clean verify into a failure.
///
/// **The witness rows are what make the soak countable.** `run_id` is minted
/// from `leaf_hash("{exit_code}:{checkpoints}:{now}")` and takes no archive
/// input, so nothing else in either table says which run came from real work —
/// which is why 39 recorded runs could not be read as evidence of anything.
async fn record_verification(
    root: &Path,
    cwd: &Path,
    repo: &RepoId,
    summary: &VerifySummary,
    witness: Option<&Witness>,
) {
    use rift_store::{VerificationDivergenceRow, VerificationRunRow, VerificationWitnessRow};

    let Ok(store) = rift_store::open_capture_projection(projection_db_under(root, repo)) else {
        return;
    };
    let now = crate::hooks::now_ms();
    let (code, detail) = match &summary.verdict {
        Verdict::Ok => (0, None),
        Verdict::Tamper(d) => (1, Some(("CheckpointRoot", d.clone()))),
        Verdict::Redaction(d) => (3, Some(("Redaction", d.clone()))),
        Verdict::Structure(d) => (5, Some(("Leaf", d.clone()))),
    };
    let outcome_sha =
        leaf_hash(format!("{code}:{}:{now}", summary.checkpoints).as_bytes()).to_hex();
    let run_id = format!("vrun-{outcome_sha}");
    let _ = rift_store::record_verification_run(
        &store,
        &VerificationRunRow {
            run_id: run_id.clone(),
            branch: "rift/checkpoints/v1".to_owned(),
            exit_code: code,
            checked_at: now,
            outcome_sha256: outcome_sha,
            schema_version: 1,
        },
    )
    .await;
    // The condensations this run witnessed, with the chain position it saw them
    // at. `None` for the head is UNREADABLE (no chain ref yet, or an unreadable
    // one) and is stored as NULL — never as an empty string standing in for a
    // real position.
    if let Some(w) = witness {
        let chain_head = rift_git::discover(cwd)
            .ok()
            .and_then(|paths| rift_git::chain::tip(&paths.git_dir).ok().flatten());
        for checkpoint in w.checkpoints() {
            let _ = rift_store::record_verification_witness(
                &store,
                &VerificationWitnessRow {
                    run_id: run_id.clone(),
                    checkpoint_id: checkpoint.to_owned(),
                    chain_head: chain_head.clone(),
                    schema_version: 1,
                },
            )
            .await;
        }
    }
    if let Some((layer, d)) = detail {
        let _ = rift_store::record_verification_divergence(
            &store,
            &VerificationDivergenceRow {
                run_id,
                ordinal: 0,
                checkpoint_id: d.split(':').next().unwrap_or_default().to_owned(),
                layer: layer.to_owned(),
                site: "GitVsRecomputed".to_owned(),
                detail: d,
                schema_version: 1,
            },
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::RiftError;

    fn repo() -> RepoId {
        RepoId::new("test-repo").unwrap()
    }

    #[tokio::test]
    async fn creates_projection_and_verifies_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let report = verify_under(
            tmp.path(),
            &repo(),
            &FixtureSource::parse(FIXTURE),
            Seeding::Permitted,
        )
        .await
        .unwrap();
        assert!(report.ok);
        assert_eq!(report.checkpoints, 1);
        // The on-disk projection was created + migrated under <root>/rift/<repo>/.
        assert!(projection_db_under(tmp.path(), &repo()).exists());
    }

    #[tokio::test]
    async fn re_run_on_persisted_projection_still_verifies() {
        let tmp = tempfile::tempdir().unwrap();
        let src = FixtureSource::parse(FIXTURE);
        verify_under(tmp.path(), &repo(), &src, Seeding::Permitted)
            .await
            .unwrap();
        // Second call re-opens the persisted db (no re-insert) and re-verifies.
        let again = verify_under(tmp.path(), &repo(), &src, Seeding::Permitted)
            .await
            .unwrap();
        assert!(again.ok);
    }

    #[tokio::test]
    async fn divergent_source_is_detected_as_integrity_error() {
        let tmp = tempfile::tempdir().unwrap();
        // Populate from the real fixture.
        verify_under(
            tmp.path(),
            &repo(),
            &FixtureSource::parse(FIXTURE),
            Seeding::Permitted,
        )
        .await
        .unwrap();
        // A different fixture → different Merkle root → the stored projection
        // (built from the original) no longer matches the authoritative source.
        let tampered = FixtureSource::parse("different leaf\nanother line\n");
        let err = verify_under(tmp.path(), &repo(), &tampered, Seeding::Permitted).await;
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    #[test]
    fn fixture_parses_to_nonempty_leaves() {
        let src = FixtureSource::parse(FIXTURE);
        assert!(!src.leaves.is_empty(), "committed fixture must have leaves");
        // The genesis link is self-consistent (id == root, no predecessor).
        let link = src.genesis_link();
        assert_eq!(link.checkpoint_id, link.merkle_root);
        assert!(link.prev.is_none());
    }

    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};

    fn hex64(c: char) -> String {
        String::from(c).repeat(64)
    }

    fn shard(id: &str, prev: Option<&str>, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.to_owned(),
            dialect: Dialect::Rift,
            prev: prev.map(str::to_owned),
            ordinal,
        }
    }

    /// A shard as read from the LEGACY checkpoint ref — the dialect whose
    /// exemption from the native passes is invariant §1.5.
    fn legacy_shard(id: &str, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            dialect: Dialect::Legacy,
            ..shard(id, None, ordinal)
        }
    }

    #[test]
    fn git_source_rederives_links_with_artifact_and_fallback_roots() {
        let id_a = hex64('a');
        let id_b = hex64('b');
        let root_a = hex64('c'); // a has an explicit merkle-root artifact
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id_a, None, 1))
            .with_shard(shard(&id_b, Some(&id_a), 2))
            .with_artifact(
                &id_a,
                MERKLE_ROOT_ARTIFACT,
                format!("{root_a}\n").as_bytes(),
            );

        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        let links = source.links().unwrap();
        assert_eq!(links.len(), 2);
        // a: root from the artifact; prev None (genesis)
        assert_eq!(links[0].checkpoint_id, Sha256Id::parse_hex(&id_a).unwrap());
        assert_eq!(links[0].merkle_root, Sha256Id::parse_hex(&root_a).unwrap());
        assert!(links[0].prev.is_none());
        // b: no artifact → self-root; prev links to a (continuity)
        assert_eq!(links[1].merkle_root, Sha256Id::parse_hex(&id_b).unwrap());
        assert_eq!(links[1].prev, Some(Sha256Id::parse_hex(&id_a).unwrap()));
        // the re-derived chain is internally continuous
        rift_hashchain::verify_chain(&links).unwrap();
    }

    // --- the two id eras ---------------------------------------------------

    /// The pre-Rift id shapes, taken from the fixtures this repository already
    /// froze rather than invented here: the sharded-branch fixture's content
    /// address is 12 hex characters and the per-checkpoint-refs fixture's is a
    /// 26-character ULID. Neither is a SHA-256 hex digest, and both are real.
    /// (Both live under the import-fixture tree beside this crate; naming them
    /// by fixture rather than by full path is paragraph 2's shed discipline —
    /// a slash-joined path is one high-entropy token to the redaction layer.)
    const LEGACY_SHORT_HEX_ID: &str = "ab34cd56ef78";
    /// The other frozen shape — see [`LEGACY_SHORT_HEX_ID`].
    const LEGACY_ULID_ID: &str = "01JQZ8T9WQKX3M7VN2P4RB6CDE";

    /// A legacy shard at an explicit walk position.
    fn legacy_shard_after(id: &str, prev: Option<&str>, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            dialect: Dialect::Legacy,
            ..shard(id, prev, ordinal)
        }
    }

    /// One native shard carrying what the BRIDGE requires (a valid report and a
    /// header naming a root), at an explicit walk position.
    fn archived_shard(
        sub: MemorySubstrate,
        id: &str,
        prev: Option<&str>,
        ordinal: u64,
        root: &str,
    ) -> MemorySubstrate {
        sub.with_shard(shard(id, prev, ordinal))
            .with_artifact(id, "redaction_report.json", fixture_report().as_bytes())
            .with_artifact(
                id,
                "metadata.json",
                format!(r#"{{"strategy":"manual-commit","merkle_root":"{root}"}}"#).as_bytes(),
            )
    }

    /// An honest pre-Rift shard: a valid redaction report and a header, and none
    /// of the native artifacts its dialect never promised.
    fn honest_legacy(sub: MemorySubstrate, id: &str, ordinal: u64) -> MemorySubstrate {
        sub.with_shard(legacy_shard_after(id, None, ordinal))
            .with_artifact(id, "redaction_report.json", fixture_report().as_bytes())
            .with_artifact(
                id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{}"}}"#,
                    hex64('9')
                )
                .as_bytes(),
            )
    }

    /// **The abort, closed.** `Sha256Id::parse_hex(&shard.checkpoint_id)?` made
    /// the mere presence of a pre-Rift shard an internal error — exit 70, on a
    /// repository whose readability is invariant §1.5. Both frozen short-id
    /// shapes now build a source instead of destroying one.
    ///
    /// What they do NOT do is take a chain position: a position is a
    /// [`Sha256Id`] by type, and an id that is not one cannot be made into one
    /// without inventing a value the writer never minted.
    #[test]
    fn the_chain_source_accepts_both_pre_rift_id_eras_instead_of_aborting() {
        for id in [LEGACY_SHORT_HEX_ID, LEGACY_ULID_ID] {
            let sub = honest_legacy(MemorySubstrate::local(), id, 1);
            let source = GitCheckpointSource::from_reader(&sub)
                .unwrap_or_else(|e| panic!("{id}: verify aborted on a pre-Rift id: {e:?}"));
            assert!(
                source.links().unwrap().is_empty(),
                "{id}: an id that is not a sha256 must not be given a chain position"
            );
        }
    }

    /// **The regression a DIALECT filter would have caused, pinned.** A
    /// pre-Rift shard whose id happens to be 64 hex characters holds a chain
    /// position today; filtering the legacy dialect out of the source would
    /// have dropped it and turned an honest repository's clean verify into a
    /// missing-link TAMPER. The filter is the id's shape, and only that.
    #[test]
    fn a_64_hex_pre_rift_id_still_takes_a_chain_position() {
        let id = hex64('d');
        let sub = honest_legacy(MemorySubstrate::local(), &id, 1);
        let links = GitCheckpointSource::from_reader(&sub)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(links.len(), 1, "a parseable id keeps its position");
        assert_eq!(links[0].checkpoint_id, Sha256Id::parse_hex(&id).unwrap());
    }

    /// **`prev` is re-derived, and this is the case that forces it.** The walk
    /// hands each shard the previous element's id, so the second native shard
    /// names the dropped legacy one as its predecessor. Copying that through
    /// was the *second* `?` — the one that kept aborting after the first was
    /// fixed. Each retained link points at the last retained id instead, so the
    /// chain the checker re-derives stays continuous across the gap.
    #[test]
    fn a_dropped_pre_rift_shard_does_not_break_chain_continuity() {
        let a = hex64('a');
        let b = hex64('b');
        let sub = MemorySubstrate::local()
            .with_shard(shard(&a, None, 1))
            .with_shard(legacy_shard_after(LEGACY_ULID_ID, Some(&a), 2))
            .with_shard(shard(&b, Some(LEGACY_ULID_ID), 3));
        let links = GitCheckpointSource::from_reader(&sub)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(links.len(), 2, "the two addressable shards");
        assert!(links[0].prev.is_none());
        assert_eq!(
            links[1].prev,
            Some(Sha256Id::parse_hex(&a).unwrap()),
            "prev is the last RETAINED id, not the walk's dropped neighbour"
        );
        rift_hashchain::verify_chain(&links).unwrap();
    }

    /// **Dropping a shard from the chain removes a crash, not a check.** A
    /// NATIVE shard keyed by an id no derivation could mint is dropped from the
    /// chain by the same parseability rule — and the per-shard walk still
    /// convicts it, because the id pass asks whether the id addresses these
    /// bytes and nothing grants an id that is not even a digest.
    #[test]
    fn a_native_shard_with_an_unaddressable_id_is_tamper_not_a_silent_skip() {
        let content = b"the redacted capture".as_slice();
        let sub = era_shard(
            MemorySubstrate::local(),
            "deadbeef",
            2,
            &writer_root(content),
            content,
        );
        assert!(
            GitCheckpointSource::from_reader(&sub)
                .unwrap()
                .links()
                .unwrap()
                .is_empty(),
            "it cannot hold a position"
        );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("a native shard with a forged id must be TAMPER, got {other:?}"),
        }
    }

    /// **The mixed store: verify walks BOTH eras in one repository.** One
    /// native shard that addresses its own bytes and two pre-Rift shards whose
    /// ids no Rift rule ever minted. The chain retains exactly the addressable
    /// one; the per-shard walk sees all three and counts the exemptions, so
    /// nothing is skipped in silence.
    #[test]
    fn verify_walks_a_store_holding_both_id_eras() {
        let content = b"the redacted capture".as_slice();
        let native = native_id(content);
        let sub = era_shard(
            MemorySubstrate::local(),
            &native,
            2,
            &writer_root(content),
            content,
        );
        let sub = honest_legacy(sub, LEGACY_ULID_ID, 2);
        let sub = honest_legacy(sub, LEGACY_SHORT_HEX_ID, 3);

        let links = GitCheckpointSource::from_reader(&sub)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(links.len(), 1, "only the addressable shard is a position");
        assert_eq!(
            links[0].checkpoint_id,
            Sha256Id::parse_hex(&native).unwrap()
        );

        let out = shard_passes(&sub).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "got {:?}", out.worst);
        assert_eq!(out.checked, 3, "every shard is walked, both eras");
        assert_eq!(
            out.pre_rift, 2,
            "…and the exemptions are counted, not hidden"
        );
    }

    /// **The other half of the same abort, and it is a different command.**
    /// `artifact_bridge`'s own `Sha256Id::parse_hex(p)?` over `shard.prev` made
    /// `rift rebuild` — and the `doctor --fix` that drives it — an internal
    /// error on a mixed repository, independently of the checker. Both sides
    /// now derive `prev` as the nearest preceding parseable id, which is what
    /// lets this test assert the thing that actually matters: the rebuild
    /// completes AND the checker agrees with what it wrote.
    #[tokio::test]
    async fn a_rebuild_over_a_mixed_store_chains_across_the_unaddressable_shard() {
        let a = hex64('a');
        let b = hex64('b');
        let mut sub = MemorySubstrate::local();
        sub = archived_shard(sub, &a, None, 1, &hex64('1'));
        sub = honest_legacy(sub, LEGACY_ULID_ID, 2);
        sub = archived_shard(sub, &b, Some(LEGACY_ULID_ID), 3, &hex64('2'));

        let tmp = tempfile::tempdir().unwrap();
        // Used to panic here: `ingest` unwraps, and the bridge returned Err.
        ingest(tmp.path(), &sub).await;
        let db = projection_db_under(tmp.path(), &repo());
        assert_eq!(
            projection_checkpoints(&db).await.unwrap(),
            2,
            "the two addressable shards are projected; the pre-Rift one is not"
        );

        // …and the two derivations agree, which is the property that made one
        // rule necessary rather than two fixes.
        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden)
            .await
            .expect("bridge and checker must derive the same chain");
    }

    /// **A corrupt NATIVE shard is REFUSED, by verdict and not by crash**
    /// — the verify half of the corrupt-shard case; its import half is the
    /// corrupt-shard import fixture.
    ///
    /// Damage in three places at once, because a checker that survives one kind
    /// of malformed byte need not survive three: a `metadata.json` truncated
    /// mid-object, a `full.jsonl` whose middle record is truncated, and a
    /// `content_hash.txt` that is not hex. Every parse on this path is lenient
    /// by design, so nothing here raises — and that is the trap: a lenient
    /// parser reads the truncated header as an EMPTY header, which declares no
    /// era, which is epoch 0, whose id rules still refuse an id no derivation
    /// minted. The shard is tamper, the run completes, and reaching the
    /// assertions at all is the "never panics" half of the claim.
    #[test]
    fn a_corrupt_native_shard_is_refused_by_verdict_not_by_crash() {
        let content = b"the redacted capture".as_slice();
        let id = native_id(content);
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", fixture_report().as_bytes())
            .with_artifact(&id, "content.redacted", content)
            // Truncated mid-object — the lenient parser reads an empty header.
            .with_artifact(
                &id,
                "metadata.json",
                br#"{"canon_version":3,"strategy":"manual-commit","merkle_r"#,
            )
            // A truncated record between two intact ones.
            .with_artifact(
                &id,
                "full.jsonl",
                b"{\"session\":\"s\",\"turn\":0}\n{\"session\":\"s\",\"tu\n{\"session\":\"s\",\"turn\":2}\n",
            )
            // Not a hex digest at all.
            .with_artifact(&id, "content_hash.txt", b"not-a-digest\n");

        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains(&id), "the finding names the shard: {d}"),
            other => panic!("a corrupt native shard must be refused, got {other:?}"),
        }
        // …and the chain derivation survives it too, rather than aborting the
        // whole run over one damaged shard.
        let links = GitCheckpointSource::from_reader(&sub)
            .expect("a corrupt shard must not destroy the chain derivation")
            .links()
            .unwrap();
        assert_eq!(links.len(), 1, "its id still parses, so it keeps its place");
    }

    /// A substrate whose shards carry what a real archive writes: a valid
    /// redaction report and a `metadata.json` naming the checkpoint's Merkle
    /// root. Enough for the BRIDGE to project them, which is the point — the
    /// projection under test has to come from somewhere other than verify.
    ///
    /// Each shard is `(id, prev, merkle_root)`, and **the root is deliberately
    /// not the id**: that is the real shape (measured — every native shard
    /// carries a `merkle_root` that differs from its checkpoint id), and a
    /// fixture where the two coincide cannot tell a metadata read from the
    /// id fallback. This fixture used to do exactly that, and a plant survived
    /// it.
    fn archived(ids: &[(&str, Option<&str>, &str)]) -> MemorySubstrate {
        let mut sub = MemorySubstrate::local();
        for (i, (id, prev, root)) in ids.iter().enumerate() {
            sub = sub
                .with_shard(shard(id, *prev, i as u64 + 1))
                .with_artifact(
                    id,
                    "redaction_report.json",
                    format!(
                        r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
                        "a".repeat(64)
                    )
                    .as_bytes(),
                )
                .with_artifact(
                    id,
                    "metadata.json",
                    format!(r#"{{"strategy":"manual-commit","merkle_root":"{root}"}}"#).as_bytes(),
                );
        }
        sub
    }

    /// The Merkle root every native shard recorded before 2026-08-12:
    /// `rift_hashchain::empty_root()`, because `condense` handed the archive an
    /// empty event set and the root was taken over it. **Measured, not chosen** —
    /// it is the literal value in this repository's 100+ archived shards and in
    /// the parity anchors as they stood at `6ba9157`. Written out here so the
    /// vacuous era is pinned by the bytes it actually produced, not by a
    /// re-derivation that could drift with the code that made it wrong.
    const VACUOUS_ERA_ROOT: &str =
        "e2c3e6f55658a556f4403ed646adbc7326b09fcbb23f3df593e7c0de60e64794";

    /// The session every era fixture archives under.
    const FIXTURE_SESSION: &str = "01JSESSIONFIXTURE";

    /// The checkpoint id a native archive of `content` under `FIXTURE_SESSION`
    /// (no read facts) lands at — **the writer's own derivation**, so a fixture
    /// addresses its own bytes the way a real shard does.
    ///
    /// The era fixtures used to carry `hex64('a')`-style ids and no `sessions`
    /// field, which meant no fixture could tell a shard that addresses its
    /// contents from one that does not. That is the hole the id pass closes, so
    /// the fixtures had to stop having it.
    fn native_id(content: &[u8]) -> String {
        native_id_at(2, content)
    }

    /// The redaction report every fixture shard carries. One definition, because
    /// three copies of it is three chances for the id, the root and the shard to
    /// be computed over different bytes.
    fn fixture_report() -> String {
        format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        )
    }

    /// The checkpoint id a shard of `canon_version` carrying `content` lands at,
    /// taken from the era's own accepted set. Going through
    /// `checkpoint_ids_for_shard` rather than a derivation means a fixture cannot
    /// be built with an id its declared era would reject — which was how the era
    /// fixtures used to lie (`hex64('a')` ids that addressed nothing).
    fn native_id_at(canon_version: u32, content: &[u8]) -> String {
        use rift_shadow::backend::ShardLeafBytes;
        let report = fixture_report();
        let mut ids = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            canon_version,
            &ShardLeafBytes {
                content_redacted: content,
                full_jsonl: None,
                redaction_report: report.as_bytes(),
                tool_calls: None,
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                session_metas: &[],
                digests: rift_shadow::backend::DigestLeafBytes::default(),
            },
        );
        ids.remove(0)
    }

    /// A shard carrying the artifacts a real archive writes, at a declared
    /// `canon_version`. `root` is what its `metadata.json` claims; the shard is
    /// keyed by `id`, which a tamper fixture deliberately leaves pointing at the
    /// bytes it USED to hold.
    fn era_shard(
        sub: MemorySubstrate,
        id: &str,
        canon_version: u32,
        root: &str,
        content: &[u8],
    ) -> MemorySubstrate {
        era_shard_for(sub, id, canon_version, root, content, &[FIXTURE_SESSION])
    }

    /// [`era_shard`] with an explicit session list — the cardinality the
    /// checkpoint-id pass gates on.
    fn era_shard_for(
        sub: MemorySubstrate,
        id: &str,
        canon_version: u32,
        root: &str,
        content: &[u8],
        sessions: &[&str],
    ) -> MemorySubstrate {
        let report = fixture_report();
        let sessions = sessions
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",");
        sub.with_shard(shard(id, None, 1))
            .with_artifact(id, "redaction_report.json", report.as_bytes())
            .with_artifact(id, "content.redacted", content)
            .with_artifact(
                id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{canon_version},"strategy":"manual-commit","merkle_root":"{root}","sessions":[{sessions}]}}"#
                )
                .as_bytes(),
            )
    }

    /// The root the WRITER would record for a shard with this content and the
    /// fixture's redaction report — the archive's own function, which is the
    /// whole point: verify has to agree with the writer, not with a second
    /// implementation of the writer living in a test.
    fn writer_root(content: &[u8]) -> String {
        use rift_shadow::backend::{shard_merkle_root, ShardLeafBytes};
        let report = fixture_report();
        shard_merkle_root(&ShardLeafBytes {
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            session_metas: &[],
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        })
        .to_hex()
    }

    /// **History stays green.** A shard from the vacuous era carries the
    /// empty-set constant; re-deriving a root for it and demanding a match would
    /// redden every repository's whole history for a defect the history could
    /// not have avoided. It is accepted — and COUNTED, so the acceptance is
    /// visible in the summary rather than assumed.
    #[test]
    fn a_vacuous_era_shard_is_accepted_and_counted() {
        let content = b"whatever the capture was".as_slice();
        let id = native_id(content);
        let sub = era_shard(MemorySubstrate::local(), &id, 1, VACUOUS_ERA_ROOT, content);
        let out = shard_passes(&sub).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "got {:?}", out.worst);
        assert_eq!(out.checked, 1);
        assert_eq!(out.vacuous_era, 1, "the era is counted, not hidden");
        assert_eq!(out.pre_rift, 0, "it declares an era, so it is not pre-Rift");

        // And this is what the era costs, RE-STATED after the id pass landed.
        // It used to cost everything: editing the capture of a vacuous-era shard
        // changed nothing, because its root never committed to it. The root
        // still does not — but the shard's own path does, so the same edit is
        // now TAMPER. The era's remaining cost is narrow and nameable: a
        // vacuous-era root proves nothing about the shard; the id does.
        let changed = era_shard(
            MemorySubstrate::local(),
            &id,
            1,
            VACUOUS_ERA_ROOT,
            b"a different capture fully",
        );
        match shard_passes(&changed).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("an edited vacuous-era capture must be TAMPER, got {other:?}"),
        }
    }

    /// The strict era: the recorded root is a claim about these bytes, and
    /// verify re-derives it with the archive writer's own function.
    #[test]
    fn a_strict_era_shard_verifies_against_its_own_artifacts() {
        let content = b"the redacted capture".as_slice();
        let sub = era_shard(
            MemorySubstrate::local(),
            &native_id(content),
            2,
            &writer_root(content),
            content,
        );
        let out = shard_passes(&sub).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "got {:?}", out.worst);
        assert_eq!(out.vacuous_era, 0, "nothing was waved through");
        assert_eq!(out.pre_rift, 0);
    }

    /// The other direction, and the one with teeth: a NEW shard whose leaf bytes
    /// were altered after its root was recorded is TAMPER, and the finding names
    /// the checkpoint.
    #[test]
    fn a_tampered_strict_era_shard_is_named_as_tamper() {
        let id = native_id(b"the redacted capture");
        let recorded = writer_root(b"the redacted capture");
        let sub = era_shard(
            MemorySubstrate::local(),
            &id,
            2,
            &recorded,
            b"the redacted capture, edited",
        );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => {
                assert!(d.contains(&id), "the finding names the shard: {d}");
                assert!(d.contains("re-derived"), "and says what disagreed: {d}");
            }
            other => panic!("a tampered strict-era shard must be TAMPER, got {other:?}"),
        }
    }

    /// Deleting a leaf artifact is not an incompleteness: the shard's own era
    /// says its root commits to those bytes, so their absence is a removal.
    #[test]
    fn a_strict_era_shard_missing_a_leaf_artifact_is_tamper() {
        let id = hex64('d');
        let content = b"the redacted capture".as_slice();
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        // Everything a strict-era shard has, except `content.redacted`.
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":2,"strategy":"manual-commit","merkle_root":"{}","sessions":["{FIXTURE_SESSION}"]}}"#,
                    writer_root(content)
                )
                .as_bytes(),
            );
        assert!(
            matches!(shard_passes(&sub).unwrap().worst, Verdict::Tamper(_)),
            "a missing leaf artifact in the strict era is a removal"
        );
    }

    /// **The dialect exemption must not silence a check that already worked.**
    /// The recorded-leaf recompute predates every native pass and runs on every
    /// shard of every dialect: a LEGACY shard whose `merkle_leaf.txt` does not
    /// produce its recorded root has to keep dying even though the native
    /// passes decline it. If the exemption ever gates the *existing* recompute
    /// rather than only the native-only passes, this reds.
    #[test]
    fn the_legacy_merkle_leaf_recompute_still_fires_on_the_legacy_dialect() {
        let id = hex64('e');
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let leaf = hex64('7');
        let sub = MemorySubstrate::local()
            .with_shard(legacy_shard(&id, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            // The legacy dialect: no canon_version, its own recorded root…
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{}"}}"#,
                    hex64('9')
                )
                .as_bytes(),
            )
            // …and a leaf list that does not produce that root.
            .with_artifact(&id, "merkle_leaf.txt", leaf.as_bytes());
        let out = shard_passes(&sub).unwrap();
        assert!(
            matches!(out.worst, Verdict::Tamper(_)),
            "the pre-existing merkle_leaf pass must run on every dialect, got {:?}",
            out.worst
        );
        assert_eq!(out.pre_rift, 1, "the dialect exemption is counted");
        assert_eq!(out.vacuous_era, 0, "…and it is not the vacuous era's");
    }

    /// **Read-compat is forever (invariant §1.5), pinned from the positive
    /// side.** An honest legacy-dialect shard — no `canon_version`, an id its
    /// own tool minted, no native artifacts at all — is exempt from every
    /// native-only pass and counted, never verdicted for what its dialect
    /// never promised.
    #[test]
    fn an_honest_legacy_dialect_shard_stays_exempt_and_counted() {
        let id = hex64('a');
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let sub = MemorySubstrate::local()
            .with_shard(legacy_shard(&id, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["{FIXTURE_SESSION}"]}}"#,
                    hex64('9')
                )
                .as_bytes(),
            );
        let out = shard_passes(&sub).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "an honest legacy shard must stay green: {:?}",
            out.worst
        );
        assert_eq!(out.pre_rift, 1, "…and the exemption is counted, not silent");
    }

    /// **PLANTED — the attack the strict Merkle pass could not see.**
    ///
    /// The recorded root lives in `metadata.json`, which the tamperer is already
    /// editing, so editing `content.redacted` AND recomputing the root passed
    /// every check this repository had. The shard's PATH is the binding they
    /// cannot recompute: it is `sha256(session ‖ … ‖ sha256_hex(capture) ‖ …)`.
    ///
    /// The plant is the whole attack, not a proxy for it: the root here is the
    /// root of the EDITED bytes, so the strict pass is satisfied and only the
    /// checkpoint-id pass can red this.
    #[test]
    fn editing_the_capture_and_recomputing_the_root_is_still_tamper() {
        let id = native_id(b"the redacted capture");
        let edited = b"the redacted capture, edited".as_slice();
        let sub = era_shard(
            MemorySubstrate::local(),
            &id,
            2,
            &writer_root(edited), // the tamperer did their arithmetic
            edited,
        );
        // The strict Merkle pass is genuinely satisfied — assert that, or the
        // test could be passing for the old reason and prove nothing new.
        assert!(
            strict_merkle_pass(
                &sub,
                &id,
                &NativeArtifacts {
                    content: sub.read_artifact(&id, "content.redacted").unwrap(),
                    tool_calls: None,
                    full: None,
                    transcript_compact: None,
                    transcript_full: None,
                    tasks: Vec::new(),
                    session_metas: Vec::new(),
                    digest_failures: None,
                    digest_intent: None,
                    digest_log: None,
                    digest_shape: None,
                },
                sub.read_artifact(&id, "redaction_report.json")
                    .unwrap()
                    .as_deref(),
            )
            .unwrap()
            .is_none(),
            "the plant must get past the root check, or it is not this attack"
        );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => {
                assert!(d.contains(&id), "the finding names the shard: {d}");
                assert!(d.contains("does not address"), "and says what broke: {d}");
            }
            other => panic!("a recomputed-root forgery must be TAMPER, got {other:?}"),
        }
    }

    /// **PLANTED — relabelling the same forgery into the vacuous era.**
    ///
    /// The follow-up to the path-binding plant above: an edited shard relabelled
    /// `canon_version: 1` with the era's constant escaped the strict pass
    /// fully and merely moved a counter. It no longer does, because the
    /// checkpoint-id pass runs on every native era — that gate is the reason it
    /// is not gated on [`STRICT_MERKLE_MIN_CANON_VERSION`] like the root pass.
    #[test]
    fn relabelling_a_forged_shard_into_the_vacuous_era_does_not_hide_it() {
        let id = native_id(b"the redacted capture");
        let sub = era_shard(
            MemorySubstrate::local(),
            &id,
            VACUOUS_MERKLE_CANON_VERSION,
            VACUOUS_ERA_ROOT,
            b"the redacted capture, edited",
        );
        let out = shard_passes(&sub).unwrap();
        assert_eq!(out.vacuous_era, 1, "the relabel did take effect…");
        match out.worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("…and it still did not help: expected TAMPER, got {other:?}"),
        }
    }

    /// **The stripped-label hole, CLOSED — the flip of the test that held it open.**
    ///
    /// Deleting `canon_version` outright used to move a native shard into the
    /// pre-Rift dialect's exemption, and every native pass declined (this test
    /// asserted `Verdict::Ok`, counted as `pre_rift`, so the hole was
    /// enumerated in code). The exemption is now the DIALECT's — the ref the
    /// shard was read from, which editing its bytes cannot change — so the
    /// same shard stays in the native passes at epoch 0, and no era's id rule
    /// grants a path to edited bytes. What unblocked the flip: the
    /// corrupt-shard fixture stopped being a made-up id with no era on the
    /// native ref and became a real epoch-1 shard minting its own id.
    #[test]
    fn deleting_the_canon_version_no_longer_escapes_the_native_passes() {
        let id = native_id(b"the redacted capture");
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", b"the redacted capture, edited")
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{VACUOUS_ERA_ROOT}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        let out = shard_passes(&sub).unwrap();
        match out.worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("a label-stripped native shard must fail its id pass, got {other:?}"),
        }
        assert_eq!(
            out.pre_rift, 0,
            "the exemption is the legacy DIALECT's; a stripped label does not buy it"
        );
    }

    /// **Its epoch-3 twin, CLOSED by the same gate.** Era 3 made that hole one
    /// epoch wider: an epoch-3 shard whose `canon_version` was
    /// DELETED landed in the pre-Rift exemption and escaped every native pass —
    /// with more to hide, because only the native passes bind `full.jsonl`.
    /// The shard here is byte-honest; only the label is stripped. Epoch 0's
    /// accepted set is the pre-root rules, a root-bound path satisfies neither,
    /// and the dialect gate no longer lets it decline the question.
    #[test]
    fn deleting_a_root_bound_shards_canon_version_is_still_tamper() {
        let content = b"the redacted capture".as_slice();
        let full = br#"{"kind":"session_start","turn":0}"#.as_slice();
        let (_, id) = root_bound_shard(content, full);
        let report = fixture_report();
        let root = rift_shadow::backend::shard_merkle_root(&rift_shadow::backend::ShardLeafBytes {
            content_redacted: content,
            full_jsonl: Some(full),
            redaction_report: report.as_bytes(),
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            session_metas: &[],
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        })
        .to_hex();
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "full.jsonl", full)
            .with_artifact(
                &id,
                "metadata.json",
                // No canon_version — the label deleted outright.
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("a label-stripped root-bound shard must be TAMPER, got {other:?}"),
        }
    }

    /// **The sentence the summary makes is now a check.**
    ///
    /// `rift verify` told operators that vacuous-era shards "are accepted with
    /// their empty-set constant" while nothing asserted any such thing. A shard
    /// that declares epoch 1 and records some other root did not come from any
    /// Rift build: `condense` took the root over an empty event set on every
    /// archive of that era, measured across all 103 shards in this repository.
    #[test]
    fn a_vacuous_era_shard_must_actually_carry_the_empty_set_constant() {
        let content = b"whatever the capture was".as_slice();
        let id = native_id(content);
        let sub = era_shard(
            MemorySubstrate::local(),
            &id,
            VACUOUS_MERKLE_CANON_VERSION,
            &writer_root(content), // a real root — but not that era's
            content,
        );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("empty-leaf-set"), "{d}"),
            other => panic!("an epoch-1 shard with a derived root must be TAMPER, got {other:?}"),
        }
        // The constant itself is the archive's, not a literal this test chose.
        assert_eq!(VACUOUS_ERA_ROOT, rift_hashchain::empty_root().to_hex());
    }

    /// A root-bound (epoch 3) shard carrying `full.jsonl`, at the id its own
    /// artifacts address. `root_bound_shard` writes it; the tests below edit it.
    fn root_bound_shard(content: &[u8], full: &[u8]) -> (MemorySubstrate, String) {
        use rift_shadow::backend::{
            shard_merkle_root, ShardLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
        };
        let report = fixture_report();
        let leaves = ShardLeafBytes {
            content_redacted: content,
            full_jsonl: Some(full),
            redaction_report: report.as_bytes(),
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            session_metas: &[],
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        };
        let root = shard_merkle_root(&leaves).to_hex();
        // The id is the era's own accepted value, not a literal: the claim under
        // test is that the real rule binds these bytes.
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "full.jsonl", full)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        (sub, id)
    }

    /// An epoch-3 shard carrying one `tasks/<id>/` subtree, at the id its own
    /// artifacts (task leaves included) address — the read-side fixture for the
    /// task-subtree pass.
    fn task_bound_shard(content: &[u8], task_id: &str) -> (MemorySubstrate, String, Vec<u8>) {
        use rift_shadow::backend::{
            shard_merkle_root, ShardLeafBytes, TaskLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
        };
        let report = fixture_report();
        // The writer's own renderer, so the fixture's task metadata is the
        // real artifact shape (its embedded tool_use_id is what the rename
        // check below compares against).
        let task_meta = rift_shadow::backend::ArchivedTask {
            tool_use_id: task_id.to_owned(),
            agent_id: "explore".to_owned(),
            started_at_ms: 4,
            ended_at_ms: 8,
            input_tokens: None,
            output_tokens: None,
            transcript: None,
        }
        .metadata_json();
        let task_leaves = [TaskLeafBytes {
            tool_use_id: task_id,
            metadata: &task_meta,
            transcript_compact: None,
            transcript_full: None,
        }];
        let leaves = ShardLeafBytes {
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &task_leaves,
            session_metas: &[],
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        };
        let root = shard_merkle_root(&leaves).to_hex();
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, &format!("tasks/{task_id}/metadata.json"), &task_meta)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"],"tasks":["{task_id}"]}}"#
                )
                .as_bytes(),
            );
        (sub, id, task_meta)
    }

    /// **The task subtree is verified through the same three bindings as every
    /// leaf, plus the one thing a nested artifact adds: its NAME.** (1) An
    /// honest task-carrying shard passes every pass. (2) Hiding the task from
    /// the metadata list and recomputing the recorded root satisfies the
    /// strict Merkle pass — the tamperer controls both files — and reds on the
    /// id pass, because the path still addresses the task-carrying root.
    /// (3) Renaming the subtree (list + tree key) while keeping its bytes
    /// leaves root and id green — the leaves never moved — and reds on the
    /// name/bytes agreement check, which exists for exactly this edit.
    #[test]
    fn a_hidden_or_renamed_task_subtree_is_tamper() {
        let content = b"parent capture".as_slice();
        let (sub, id, task_meta) = task_bound_shard(content, "toolu_v");
        let out = shard_passes(&sub).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
        assert_eq!(out.checked, 1);

        // (2) The hide: metadata drops the task list and records the TASKLESS
        // root (the tamperer's arithmetic done for them).
        let taskless_root = writer_root(content);
        let hidden = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", fixture_report().as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "tasks/toolu_v/metadata.json", &task_meta)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":3,"strategy":"manual-commit","merkle_root":"{taskless_root}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        let out = shard_passes(&hidden).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not address")),
            "a hidden task must fail the id pass: {:?}",
            out.worst
        );

        // (3) The rename: same bytes, listed and stored under another name.
        let (orig, _, _) = task_bound_shard(content, "toolu_v");
        let root = {
            // Recorded root/id are unchanged — re-list under the new name only.
            let meta = orig.read_artifact(&id, "metadata.json").unwrap().unwrap();
            let v: serde_json::Value = serde_json::from_slice(&meta).unwrap();
            v.get("merkle_root")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_owned()
        };
        let renamed = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", fixture_report().as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "tasks/toolu_w/metadata.json", &task_meta)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":3,"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"],"tasks":["toolu_w"]}}"#
                )
                .as_bytes(),
            );
        let out = shard_passes(&renamed).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("different tool-use id")),
            "a renamed subtree is a name/bytes disagreement: {:?}",
            out.worst
        );
    }

    /// **The rollup is tamper-evident though the parent metadata is not a
    /// leaf** — same treatment as the recorded Merkle root: verify re-sums the
    /// task subtrees' own (leaf-bound) metadata against the recorded
    /// `task_tokens`, so stripping the rollup (hiding spend) reds without a
    /// single leaf byte moving, and root + id stay green throughout, which is
    /// exactly why the recompute must exist.
    #[test]
    fn a_forged_task_token_rollup_is_tamper() {
        use rift_shadow::backend::{
            shard_merkle_root, ShardLeafBytes, TaskLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
        };
        let content = b"parent capture".as_slice();
        let report = fixture_report();
        let task_meta = rift_shadow::backend::ArchivedTask {
            tool_use_id: "toolu_t".to_owned(),
            agent_id: "explore".to_owned(),
            started_at_ms: 4,
            ended_at_ms: 8,
            input_tokens: Some(120),
            output_tokens: Some(34),
            transcript: None,
        }
        .metadata_json();
        let task_leaves = [TaskLeafBytes {
            tool_use_id: "toolu_t",
            metadata: &task_meta,
            transcript_compact: None,
            transcript_full: None,
        }];
        let leaves = ShardLeafBytes {
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &task_leaves,
            session_metas: &[],
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        };
        let root = shard_merkle_root(&leaves).to_hex();
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        let with_rollup = |rollup: &str| {
            MemorySubstrate::local()
                .with_shard(shard(&id, None, 1))
                .with_artifact(&id, "redaction_report.json", report.as_bytes())
                .with_artifact(&id, "content.redacted", content)
                .with_artifact(&id, "tasks/toolu_t/metadata.json", &task_meta)
                .with_artifact(
                    &id,
                    "metadata.json",
                    format!(
                        r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"],"tasks":["toolu_t"]{rollup}}}"#
                    )
                    .as_bytes(),
                )
        };
        // Honest: the recorded rollup equals the sum of the leaves.
        let honest = with_rollup(r#","task_tokens":{"input_tokens":120,"output_tokens":34}"#);
        let out = shard_passes(&honest).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "honest rollup: {:?}",
            out.worst
        );

        // Stripped: the leaves report spend, the parent claims none.
        let stripped = with_rollup("");
        let out = shard_passes(&stripped).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("task-token rollup")),
            "a stripped rollup must red: {:?}",
            out.worst
        );

        // Understated: the parent claims a different sum.
        let understated = with_rollup(r#","task_tokens":{"input_tokens":1,"output_tokens":34}"#);
        let out = shard_passes(&understated).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("task-token rollup")),
            "an understated rollup must red: {:?}",
            out.worst
        );
    }

    /// **A stored digest artifact is a leaf like any other**: editing
    /// `digest.shape.json` in place — no recomputed root, the
    /// lazy tamper — reds the strict Merkle pass; recomputing the root too
    /// still reds the id pass. Both arms planted.
    #[test]
    fn an_edited_digest_artifact_is_tamper() {
        use rift_shadow::backend::{
            shard_merkle_root, DigestLeafBytes, ShardLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
        };
        let content = b"parent capture".as_slice();
        let report = fixture_report();
        let shape = rift_shadow::digest::shape_json(&[rift_shadow::backend::ArchivedReadFact {
            tool_use_id: "t1".to_owned(),
            tool_name: "Read".to_owned(),
            path: "src/a.rs".to_owned(),
            content_sha256: "aa".to_owned(),
            input_tokens: None,
        }])
        .unwrap();
        let tool_calls = rift_shadow::backend::tool_calls_jsonl_bytes(&[
            rift_shadow::backend::ArchivedReadFact {
                tool_use_id: "t1".to_owned(),
                tool_name: "Read".to_owned(),
                path: "src/a.rs".to_owned(),
                content_sha256: "aa".to_owned(),
                input_tokens: None,
            },
        ])
        .unwrap();
        let leaves = ShardLeafBytes {
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            tool_calls: Some(&tool_calls),
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            session_metas: &[],
            digests: DigestLeafBytes {
                shape: Some(&shape),
                ..DigestLeafBytes::default()
            },
        };
        let root = shard_merkle_root(&leaves).to_hex();
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        let build = |shape_bytes: &[u8], recorded_root: &str| {
            MemorySubstrate::local()
                .with_shard(shard(&id, None, 1))
                .with_artifact(&id, "redaction_report.json", report.as_bytes())
                .with_artifact(&id, "content.redacted", content)
                .with_artifact(&id, "tool_calls.jsonl", &tool_calls)
                .with_artifact(&id, rift_shadow::digest::DIGEST_SHAPE_NAME, shape_bytes)
                .with_artifact(
                    &id,
                    "metadata.json",
                    format!(
                        r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{recorded_root}","sessions":["{FIXTURE_SESSION}"]}}"#
                    )
                    .as_bytes(),
                )
        };
        // Honest.
        let out = shard_passes(&build(&shape, &root)).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);

        // Edited in place: the recorded root no longer matches.
        let edited = br#"{"by_ext":{"rs":999},"files":[],"schema_version":1}"#;
        let out = shard_passes(&build(edited, &root)).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not match the root")),
            "an in-place digest edit reds the strict pass: {:?}",
            out.worst
        );

        // Edited with the root recomputed: the path stops addressing it.
        let edited_leaves = ShardLeafBytes {
            digests: DigestLeafBytes {
                shape: Some(edited),
                ..DigestLeafBytes::default()
            },
            ..leaves
        };
        let recomputed = shard_merkle_root(&edited_leaves).to_hex();
        let out = shard_passes(&build(edited, &recomputed)).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not address")),
            "a recomputed-root digest edit reds the id pass: {:?}",
            out.worst
        );
    }

    /// **What binding the id to the root actually buys, shown on the artifact
    /// that had no binding at all.**
    ///
    /// Until epoch 3 the id covered `content.redacted` and `tool_calls.jsonl`.
    /// `full.jsonl` was covered by the recorded Merkle root and by nothing else —
    /// and the recorded root lives in `metadata.json`, which the tamperer is
    /// already editing. So editing an event stream and recomputing the root was a
    /// consistent forgery **at the shard's own path**.
    ///
    /// The plant does the tamperer's arithmetic: the recorded root here is the
    /// root of the EDITED bytes, so the strict Merkle pass is satisfied and only
    /// the id pass can red this. That is asserted, not assumed.
    #[test]
    fn editing_a_root_bound_shards_event_stream_is_still_tamper() {
        use rift_shadow::backend::ROOT_BOUND_ID_MIN_CANON_VERSION;
        let content = b"the redacted capture".as_slice();
        let full = br#"{"kind":"session_start","turn":0}"#.as_slice();
        let (honest, id) = root_bound_shard(content, full);
        let out = shard_passes(&honest).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "the honest shard must verify first, or the tamper case proves nothing: {:?}",
            out.worst
        );
        assert_eq!(out.vacuous_era, 0);

        // Now edit the event stream and recompute the recorded root over the
        // edited bytes — everything inside the shard is consistent.
        let edited = br#"{"kind":"session_start","turn":0,"forged":true}"#.as_slice();
        let report = fixture_report();
        let forged_root =
            rift_shadow::backend::shard_merkle_root(&rift_shadow::backend::ShardLeafBytes {
                content_redacted: content,
                full_jsonl: Some(edited),
                redaction_report: report.as_bytes(),
                tool_calls: None,
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                session_metas: &[],
                digests: rift_shadow::backend::DigestLeafBytes::default(),
            })
            .to_hex();
        let forged = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "full.jsonl", edited)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{forged_root}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        assert!(
            strict_merkle_pass(
                &forged,
                &id,
                &NativeArtifacts {
                    content: forged.read_artifact(&id, "content.redacted").unwrap(),
                    tool_calls: None,
                    full: forged.read_artifact(&id, "full.jsonl").unwrap(),
                    transcript_compact: None,
                    transcript_full: None,
                    tasks: Vec::new(),
                    session_metas: Vec::new(),
                    digest_failures: None,
                    digest_intent: None,
                    digest_log: None,
                    digest_shape: None,
                },
                forged
                    .read_artifact(&id, "redaction_report.json")
                    .unwrap()
                    .as_deref(),
            )
            .unwrap()
            .is_none(),
            "the plant must get past the root check, or it is not this attack"
        );
        match shard_passes(&forged).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("an edited event stream at a bound id must be TAMPER, got {other:?}"),
        }
    }

    /// **The transcript pair is verified like every other content artifact**:
    /// an honest transcript-carrying shard passes both
    /// native passes, and editing a transcript — with the tamperer's own
    /// recomputed root, the same shape as the planted forgery — still fails the id pass. The
    /// honest half is the half that reds if `NativeArtifacts` ever stops
    /// reading the pair: the re-derived root would silently lose two leaves
    /// and disagree with the record.
    #[test]
    fn a_transcript_carrying_shard_verifies_and_an_edited_transcript_is_tamper() {
        use rift_shadow::backend::{
            shard_merkle_root, ShardLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
            TRANSCRIPT_COMPACT_NAME, TRANSCRIPT_FULL_NAME,
        };
        let content = b"the redacted capture".as_slice();
        let compact = br#"{"v":1,"role":"user"}"#.as_slice();
        let tfull = br#"{"type":"user","message":"hi"}"#.as_slice();
        let report = fixture_report();
        let leaves = ShardLeafBytes {
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            tool_calls: None,
            transcript_compact: Some(compact),
            transcript_full: Some(tfull),
            tasks: &[],
            session_metas: &[],
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        };
        let root = shard_merkle_root(&leaves).to_hex();
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        let shard_with = |tfull_bytes: &[u8], recorded_root: &str| {
            MemorySubstrate::local()
                .with_shard(shard(&id, None, 1))
                .with_artifact(&id, "redaction_report.json", report.as_bytes())
                .with_artifact(&id, "content.redacted", content)
                .with_artifact(&id, TRANSCRIPT_COMPACT_NAME, compact)
                .with_artifact(&id, TRANSCRIPT_FULL_NAME, tfull_bytes)
                .with_artifact(
                    &id,
                    "metadata.json",
                    format!(
                        r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"compact_transcript":"{TRANSCRIPT_COMPACT_NAME}","compact_transcript_start":0,"merkle_root":"{recorded_root}","sessions":["{FIXTURE_SESSION}"],"strategy":"manual-commit"}}"#
                    )
                    .as_bytes(),
                )
        };
        let out = shard_passes(&shard_with(tfull, &root)).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "the honest transcript shard must verify: {:?}",
            out.worst
        );

        // The tamperer edits the full transcript and recomputes the recorded
        // root over the edited bytes — the strict pass is satisfied; the path
        // is not.
        let edited = br#"{"type":"user","message":"hi, forged"}"#.as_slice();
        let forged_root = shard_merkle_root(&ShardLeafBytes {
            transcript_full: Some(edited),
            ..leaves
        })
        .to_hex();
        match shard_passes(&shard_with(edited, &forged_root))
            .unwrap()
            .worst
        {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("an edited transcript at a bound id must be TAMPER, got {other:?}"),
        }
    }

    /// **Relabelling DOWN does not buy the looser id rule** — measured, because
    /// the design note first said it would.
    ///
    /// Epochs ≤ 2 accept the pre-root derivations, so the obvious attack on an
    /// asymmetric era gate is to rewrite a root-bound shard's `canon_version` to
    /// 2 and edit whatever the older rule did not bind. It does not work, and the
    /// reason is structural rather than lucky: the two eras' rules produce
    /// DIFFERENT ids for the same bytes, and the shard's path can only ever
    /// satisfy one of them. A path minted by the root rule is evidence of the era
    /// it was minted in, so the label cannot be moved out from under it.
    ///
    /// What remains — and it is the forgery plant's known remainder, not this gate's — is a
    /// forger writing a whole new shard at the era-2 path its own bytes address.
    /// That is a new checkpoint, and membership is the chain's job.
    #[test]
    fn relabelling_a_root_bound_shard_into_the_capture_era_does_not_hide_it() {
        let content = b"the redacted capture".as_slice();
        let full = br#"{"kind":"session_start","turn":0}"#.as_slice();
        let (_, id) = root_bound_shard(content, full);
        let edited = br#"{"kind":"session_start","turn":0,"forged":true}"#.as_slice();
        let report = fixture_report();
        let relabelled_root =
            rift_shadow::backend::shard_merkle_root(&rift_shadow::backend::ShardLeafBytes {
                content_redacted: content,
                full_jsonl: Some(edited),
                redaction_report: report.as_bytes(),
                tool_calls: None,
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                session_metas: &[],
                digests: rift_shadow::backend::DigestLeafBytes::default(),
            })
            .to_hex();
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "full.jsonl", edited)
            .with_artifact(
                &id,
                "metadata.json",
                // canon_version 2: the era whose id rule ignores full.jsonl.
                format!(
                    r#"{{"canon_version":2,"strategy":"manual-commit","merkle_root":"{relabelled_root}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("a relabelled root-bound shard must be TAMPER, got {other:?}"),
        }
    }

    /// A multi-session checkpoint is SKIPPED by the id pass and COUNTED, the
    /// same way the vacuous era is: the derivation takes one session, and what
    /// a checkpoint id should be over N of them is the writer's question, not a
    /// thing to guess inside a verifier.
    ///
    /// Measured before it was gated: every one of this repository's 103 shards
    /// names exactly one session, so this path has never run on real data.
    #[test]
    fn a_multi_session_shard_gets_the_id_pass_not_a_skip() {
        use rift_shadow::backend::{
            session_meta_json, shard_merkle_root, SessionMetaLeaf, ShardLeafBytes,
            ROOT_BOUND_ID_MIN_CANON_VERSION,
        };
        let content = b"the redacted capture".as_slice();
        let report = fixture_report();
        let sessions = [FIXTURE_SESSION.to_owned(), "01JSESSIONOTHER".to_owned()];
        // The writer's own layout: sessions/<i>/metadata.json leaves at N ≥ 2.
        let metas: Vec<(String, Vec<u8>)> = sessions
            .iter()
            .enumerate()
            .map(|(i, sid)| (i.to_string(), session_meta_json(i, sid)))
            .collect();
        let meta_leaves: Vec<SessionMetaLeaf<'_>> = metas
            .iter()
            .map(|(name, bytes)| SessionMetaLeaf {
                index_name: name,
                metadata: bytes,
            })
            .collect();
        let leaves = ShardLeafBytes {
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            session_metas: &meta_leaves,
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        };
        let root = shard_merkle_root(&leaves).to_hex();
        // The id is the DEFINED multi-session derivation —
        // the carve-out this test used to pin is retired.
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &sessions,
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        let mut sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}","01JSESSIONOTHER"]}}"#
                )
                .as_bytes(),
            );
        for (name, bytes) in &metas {
            sub = sub.with_artifact(&id, &format!("sessions/{name}/metadata.json"), bytes);
        }
        let out = shard_passes(&sub).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "an honest multi-session shard passes the id pass: {:?}",
            out.worst
        );

        // …and the id really is session-order-bound: the same shard listing
        // its sessions in the other order stops addressing its path.
        let swapped = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "sessions/0/metadata.json", &metas[0].1)
            .with_artifact(&id, "sessions/1/metadata.json", &metas[1].1)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":["01JSESSIONOTHER","{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        let out = shard_passes(&swapped).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not address")),
            "a permuted session list is a different identity: {:?}",
            out.worst
        );
    }

    /// **The zero-session arm, RETIRED from a counted skip into a finding.**
    /// A native shard whose header names no session — an
    /// emptied list or a deleted `sessions` field, two edits with one parse —
    /// used to turn the id pass off and be counted past. No era's rule grants
    /// an id to an empty session set (`from_root_sessions` refuses it; the
    /// pre-root rules were single-session by construction), so the empty
    /// candidate set now reds as tamper: the one thing a session-less label
    /// cannot explain is how the shard got its path.
    #[test]
    fn a_shard_naming_no_session_is_tamper_not_a_counted_skip() {
        let empty_list = b"a capture whose header lists no session".as_slice();
        let no_field = b"a capture whose header has no sessions field".as_slice();
        let sub = era_shard_for(
            MemorySubstrate::local(),
            &native_id(empty_list),
            2,
            &writer_root(empty_list),
            empty_list,
            &[],
        );
        // The field removed fully, not emptied — the shape `serde(default)`
        // turns into the same empty list, reached by a different edit.
        let id = native_id(no_field);
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let sub = sub
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", no_field)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":2,"strategy":"manual-commit","merkle_root":"{}"}}"#,
                    writer_root(no_field)
                )
                .as_bytes(),
            );

        let out = shard_passes(&sub).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not address")),
            "a session-less native shard cannot address its path: {:?}",
            out.worst
        );
    }

    /// Build the projection the way production does — the artifact bridge, the
    /// same walk `rift rebuild` runs.
    async fn ingest(tmp: &Path, sub: &MemorySubstrate) {
        let store = rift_store::open_capture_projection(projection_db_under(tmp, &repo())).unwrap();
        rift_shadow::ingest_from_substrate(&store, sub, "test-repo", "claude-code", 1_700_000_000)
            .await
            .unwrap();
    }

    /// **Green because two derivations agree — not because one seeded the
    /// other.** The projection is written by the BRIDGE; verify then re-derives
    /// the chain from the same substrate and compares. The distinction is not
    /// rhetorical, and the test states it three ways: the rows exist before
    /// verify runs, none of them carries the seeding strategy, and the row count
    /// is unchanged by the run.
    #[tokio::test]
    async fn a_bridge_built_projection_verifies_because_the_derivations_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('b'), Some(&hex64('a')), &hex64('2')),
        ]);
        ingest(tmp.path(), &sub).await;

        let db = projection_db_under(tmp.path(), &repo());
        assert_eq!(
            projection_checkpoints(&db).await.unwrap(),
            2,
            "the bridge wrote the rows, before verify ran at all"
        );
        let store = rift_store::open_capture_projection(&db).unwrap();
        let rows = store
            .query("SELECT strategy FROM checkpoint", &[])
            .await
            .unwrap();
        for r in &rows {
            assert_ne!(
                r.get_text(0).unwrap(),
                SEEDED_STRATEGY,
                "these rows are the bridge's, not verify's"
            );
        }

        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        let report = verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden)
            .await
            .unwrap();
        assert!(report.ok);
        assert_eq!(report.checkpoints, 2);
        assert_eq!(
            projection_checkpoints(&db).await.unwrap(),
            2,
            "and verify wrote nothing"
        );

        // The comparison still bites: a different second checkpoint diverges.
        let tampered = GitCheckpointSource::from_reader(&archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('d'), Some(&hex64('a')), &hex64('3')),
        ]))
        .unwrap();
        let err = verify_under(tmp.path(), &repo(), &tampered, Seeding::Forbidden).await;
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    /// **A projection that is BEHIND its branch reads as TAMPER, not as
    /// "incomplete"** — and that is why `rift chain init` and the worker's
    /// NeedsInit marker both name `rift rebuild` as the follow-up. An operator
    /// who runs init on a repository whose projection was levelled, and stops
    /// there, is handed a tamper verdict for a repository that is fine.
    ///
    /// The messages in `crate::chain` describe this behaviour, so it is pinned
    /// here rather than asserted there: if verify ever learns to distinguish
    /// "behind" from "rewritten", this reds and those sentences must change with
    /// it. (The distinction itself is a carried-open item.)
    #[tokio::test]
    async fn a_projection_behind_its_branch_is_reported_as_tamper_not_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        // The projection is built from ONE shard…
        ingest(tmp.path(), &archived(&[(&hex64('a'), None, &hex64('1'))])).await;
        assert_eq!(
            projection_checkpoints(&projection_db_under(tmp.path(), &repo()))
                .await
                .unwrap(),
            1
        );
        // …while the branch has since grown a second, exactly as it does when a
        // levelled projection meets a repository that kept committing.
        let source = GitCheckpointSource::from_reader(&archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('b'), Some(&hex64('a')), &hex64('2')),
        ]))
        .unwrap();
        let out = verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden).await;
        assert!(
            matches!(out, Err(RiftError::Integrity(_))),
            "a projection behind its git is not distinguishable from a rewritten \
             one, and verify says so: {out:?}"
        );
    }

    /// The self-fulfilment, killed: on the git path an EMPTY projection is
    /// reported, never filled in from the comparand. Under the old code this
    /// call returned `Ok` — green about nothing.
    ///
    /// **This is a property of `verify_under`, not of the `rift verify` COMMAND,
    /// and the difference is real**: the command
    /// levels first, so it rarely meets an empty projection at all. Nothing here
    /// can redden if that command-level property is lost — the test that can is
    /// `levelling_cannot_launder_a_planted_checkpoint_into_agreement`
    /// (`bins/rift/tests/hooks_chain_e2e.rs`), which drives the shipped binary against
    /// a planted disagreeing row.
    #[tokio::test]
    async fn verify_never_seeds_the_projection_on_the_git_path() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('b'), Some(&hex64('a')), &hex64('2')),
        ]);
        let source = GitCheckpointSource::from_reader(&sub).unwrap();

        let out = verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden).await;
        assert!(
            matches!(out, Err(RiftError::Integrity(_))),
            "an empty projection is a finding, not a blank cheque: {out:?}"
        );
        assert_eq!(
            projection_checkpoints(&projection_db_under(tmp.path(), &repo()))
                .await
                .unwrap(),
            0,
            "verify wrote nothing into the projection it was asked to check"
        );
    }

    /// The root a shard RECORDS is the one in its own metadata, not the id.
    ///
    /// Measured on this repository: 102 native shards carry `merkle_root` in
    /// `metadata.json` and none carries `merkle_root.txt`, so the artifact-only
    /// derivation fell through to the id on every row while the bridge recorded
    /// the metadata value.
    #[test]
    fn the_recorded_root_comes_from_the_shard_metadata_the_bridge_reads() {
        let id = hex64('a');
        let root = hex64('c');
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(
                &id,
                "metadata.json",
                format!(r#"{{"merkle_root":"{root}"}}"#).as_bytes(),
            );
        let links = GitCheckpointSource::from_reader(&sub)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(links[0].merkle_root, Sha256Id::parse_hex(&root).unwrap());
        assert_ne!(
            links[0].merkle_root, links[0].checkpoint_id,
            "the id fallback would have hidden the disagreement"
        );

        // With neither metadata root nor artifact, the id is its own root.
        let bare = MemorySubstrate::local().with_shard(shard(&id, None, 1));
        let bare_links = GitCheckpointSource::from_reader(&bare)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(bare_links[0].merkle_root, bare_links[0].checkpoint_id);
    }

    /// **REPLACES `malformed_checkpoint_id_is_validity_error`, which asserted
    /// the defect.** That test said `from_reader(&sub).is_err()` on a shard
    /// keyed `"not-hex"` — it could only pass while a malformed id destroyed
    /// the whole chain derivation, which is the exit-70 abort the two-era id
    /// union exists to remove. Its old form is the restore-the-defect calibration, recorded in
    /// the diff: checking out the previous version of this function against
    /// this tree reds it.
    ///
    /// The contract it is replaced by: a malformed id is SKIPPED, the run
    /// survives, and the shard is still walked by the per-shard passes.
    #[test]
    fn a_malformed_checkpoint_id_is_skipped_rather_than_fatal() {
        let sub = MemorySubstrate::local().with_shard(shard("not-hex", None, 1));
        let source = GitCheckpointSource::from_reader(&sub)
            .expect("a malformed id must not destroy the chain derivation");
        assert!(source.links().unwrap().is_empty());
    }

    /// **A second genesis is still refused — and the layer that refuses it
    /// moved, so the test moved with it.**
    ///
    /// The old fixture made two shards both claim no predecessor and let the
    /// SOURCE hand that broken chain to `verify_chain`. With `prev` re-derived
    /// from the walk, the source cannot emit two genesis links at all — which
    /// is a strictly better world and makes the old fixture unable to plant the
    /// defect it names. So the defect is planted where it actually occurs: in
    /// the PROJECTION, which is what a rebuild by an older binary, or a hand-
    /// written row, leaves behind. The bridge records a linear chain, a second
    /// genesis row is planted beside it, and verify must still refuse.
    #[tokio::test]
    async fn a_chain_with_two_genesis_checkpoints_is_still_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('b'), Some(&hex64('a')), &hex64('2')),
        ]);
        ingest(tmp.path(), &sub).await;

        // The plant: a third checkpoint recorded with NO predecessor, so the
        // stored chain has two beginnings while git has one. Scoped, so the
        // writer's handle is dropped before the reader opens the same file.
        {
            let db = projection_db_under(tmp.path(), &repo());
            let store = rift_store::open_capture_projection(&db).unwrap();
            record_checkpoint(
                &store,
                &CheckpointRecord {
                    checkpoint_id: Sha256Id::parse_hex(&hex64('c')).unwrap(),
                    strategy: "manual-commit".to_owned(),
                    created_at: 1_700_000_000,
                    merkle_root: Sha256Id::parse_hex(&hex64('3')).unwrap(),
                    prev: None,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        }

        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        let err = verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden).await;
        match err {
            Err(RiftError::Integrity(m)) => {
                assert!(m.contains("genesis") || m.contains("chain break"), "{m}");
            }
            other => panic!("a second genesis must be refused, got {other:?}"),
        }
    }

    // --- the 1:1 check, over real git ---------------------------------------

    /// Write order A, B; content-hash order A < B, so a sort agrees here and
    /// the tests below turn on presence rather than order — except the third,
    /// which uses MID (sorting between them) to make the disagreement concrete.
    const CK_A: &str = "4c5c0a053e73b924663009e1602882b6073d3fdd10ad33aa92c988a643e040f3";
    const CK_B: &str = "b970a014c1a3dd6508c5382679051a37f7cc1cd238b66543ecb1472a746d23c7";
    const CK_MID: &str = "93ae336492a162b429cf27a5952d8d8f0a7a878eca243585d5bc33893d7d0d9a";

    /// A checked git call with a fixed identity, answering its trimmed
    /// stdout. Every git the fixtures run goes through here: an unchecked
    /// call that fails transiently (a lock held by a parallel test, a
    /// machine with no git identity) leaves the fixture half-built and the
    /// test asserting about a state it never made.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_AUTHOR_DATE", "@1700000001 +0000")
            .env("GIT_COMMITTER_DATE", "@1700000001 +0000")
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// One shard, one commit, on the checkpoint branch.
    fn archive_shard(dir: &Path, id: &str) {
        let (hi, rest) = id.split_at(2);
        let shard = dir.join(hi).join(rest);
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join("metadata.json"), r#"{"strategy":"x"}"#).unwrap();
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-qm", id]);
    }

    fn checkpoint_repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git_in(d.path(), &["init", "-q", "-b", "rift/checkpoints/v1"]);
        d
    }

    #[test]
    fn a_one_to_one_chain_is_clean() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        for id in [CK_A, CK_B] {
            archive_shard(d.path(), id);
            rift_git::chain::append(&git_dir, id, 1_700_000_000).unwrap();
        }
        assert_eq!(chain_completeness(d.path()), Verdict::Ok);
    }

    /// Direction one: a checkpoint on the branch with no entry. Incomplete, not
    /// falsified — and it names the checkpoint and the count.
    #[test]
    fn an_unchained_checkpoint_is_reported_by_id_and_count() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        archive_shard(d.path(), CK_A);
        rift_git::chain::append(&git_dir, CK_A, 1_700_000_000).unwrap();
        archive_shard(d.path(), CK_B); // its entry never landed

        let Verdict::Structure(m) = chain_completeness(d.path()) else {
            panic!("an un-chained checkpoint is incomplete, not tamper")
        };
        assert!(m.contains(CK_B), "names the checkpoint: {m}");
        assert!(m.contains("1 checkpoint(s)"), "and the count: {m}");
        assert!(m.contains("no chain entry"), "{m}");
    }

    /// An adversarial review's plant, now a permanent guard: a checkpoint recorded at TWO
    /// chain positions while the branch carries it once. The set filters and
    /// the position zip are all blind to multiplicity when every id is
    /// present — this reddened nothing until the duplicate screen landed, and
    /// verify said OK over a duplicated history. Calibrated: with the screen
    /// reverted, this test panics at its Tamper match.
    #[test]
    fn a_checkpoint_recorded_at_two_positions_is_tamper() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        archive_shard(d.path(), CK_A);
        rift_git::chain::append(&git_dir, CK_A, 1_700_000_000).unwrap();
        // The product cannot write this state (`record_archived` no-ops on a
        // recorded id; `init_from_branch` refuses an existing ref) — build it
        // the way an adversary would, with git itself.
        // Checked calls, with an identity: unchecked, a transient failure left
        // the duplicate unwritten and the test reported "not detected".
        let head = git_in(d.path(), &["rev-parse", "refs/rift/chain/v1"]);
        let dup = git_in(
            d.path(),
            &[
                "commit-tree",
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
                "-p",
                &head,
                "-m",
                &format!("rift: chain\n\nRift-Chain-Checkpoint: {CK_A}"),
            ],
        );
        git_in(d.path(), &["update-ref", "refs/rift/chain/v1", &dup]);

        let Verdict::Tamper(m) = chain_completeness(d.path()) else {
            panic!("a duplicated chain entry is a tamper finding")
        };
        assert!(m.contains(CK_A), "names the duplicated checkpoint: {m}");
        assert!(m.contains("more than one position"), "{m}");
    }

    /// Direction two, which the first cannot see: an entry naming a checkpoint
    /// the branch does not carry. Its own function, its own message, and a
    /// tamper verdict — the branch was rewritten or the chain is not this
    /// repository's.
    #[test]
    fn an_orphan_entry_gets_its_own_diagnostic() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        archive_shard(d.path(), CK_A);
        rift_git::chain::append(&git_dir, CK_A, 1_700_000_000).unwrap();
        rift_git::chain::append(&git_dir, CK_B, 1_700_000_000).unwrap(); // never archived

        let Verdict::Tamper(m) = chain_completeness(d.path()) else {
            panic!("an entry naming nothing on the branch is a tamper finding")
        };
        assert!(m.contains(CK_B), "names the entry's checkpoint: {m}");
        assert!(m.contains("not on the branch"), "{m}");
        assert!(
            !m.contains("no chain entry"),
            "not the other diagnostic: {m}"
        );
    }

    /// Both present, positions disagree: say which position EACH source claims.
    #[test]
    fn orders_that_disagree_say_what_each_source_claims_where() {
        let d = checkpoint_repo();
        let git_dir = d.path().join(".git");
        for id in [CK_A, CK_B, CK_MID] {
            archive_shard(d.path(), id);
        }
        // The chain records A, MID, B — the order a content-hash sort produces.
        for id in [CK_A, CK_MID, CK_B] {
            rift_git::chain::append(&git_dir, id, 1_700_000_000).unwrap();
        }
        let Verdict::Tamper(m) = chain_completeness(d.path()) else {
            panic!("two histories disagreeing about a position is tamper")
        };
        assert!(m.contains("position 2"), "the position: {m}");
        assert!(m.contains(CK_MID), "what the chain claims there: {m}");
        assert!(m.contains(CK_B), "what the branch claims there: {m}");
    }
}
