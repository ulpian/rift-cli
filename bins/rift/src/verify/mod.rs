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
//! read-only gix [`SubstrateReader`](rift_recall_substrate::SubstrateReader) and
//! cross-checks it against the projection. On
//! a fresh repo with no checkpoint branch yet, it falls back to the committed
//! fixture skeleton so the wiring (projection create/migrate, record, cross-check,
//! report) still runs end-to-end (the M0 path).
//!
//! The command is split by concern: [`source`] says where a chain comes from,
//! [`projection`] compares the stored chain against it and records the run,
//! [`order`] checks the recorded order against the branch, [`artifacts`] reads
//! a shard once for the passes in [`merkle`] and [`identity`], [`passes`]
//! drives them over every shard, and [`verdict`] owns the vocabulary they all
//! report in.

mod artifacts;
mod boundary_gaps;
pub(crate) mod fallback;
mod identity;
mod merkle;
mod order;
mod originals;
mod passes;
mod projection;
mod source;
mod spills;
mod verdict;

use rift_core::Result;
use rift_recall_substrate::GixSubstrate;
use rift_store::paths::projection_db_under;

use self::order::chain_completeness;
use self::passes::{shard_passes, ShardPasses};
use self::projection::{projection_checkpoints, record_verification, verify_under, Seeding};
use self::source::{git_source, FixtureSource, GitCheckpointSource, FIXTURE};
use self::verdict::worse;
pub(crate) use self::verdict::{Verdict, VerifySummary};

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

/// Where one run reads and writes: the repository being verified and the
/// family-keyed projection it is compared against.
///
/// One struct rather than four parameters threaded through every step, and
/// resolved once at the top so no step can key a path by a different id than
/// the one the run started with.
struct RunPaths {
    /// The repository root the operator (or the palette) named.
    cwd: std::path::PathBuf,
    /// This machine's cache root, the only place a projection lives.
    cache_root: std::path::PathBuf,
    /// The FAMILY the repository belongs to — never a member's own path.
    repo: rift_core::RepoId,
    /// The family's projection database under `cache_root`.
    db: std::path::PathBuf,
}

impl RunPaths {
    /// Resolve the family and its projection for the repository at `root`.
    ///
    /// # Errors
    /// If the family cannot be resolved for `root`.
    fn from_repository_root(root: &std::path::Path) -> Result<Self> {
        let cwd = root.to_path_buf();
        // The projection this repository's family owns — `rift
        // verify` run from a linked worktree verifies the family's chain, not an
        // empty one keyed by the member's own path.
        let repo = crate::family::resolve_at(&cwd)?.family_id;
        let cache_root = crate::family::cache_root();
        let db = projection_db_under(&cache_root, &repo);
        Ok(Self {
            cwd,
            cache_root,
            repo,
            db,
        })
    }
}

/// What the authoritative source yielded, before it is folded into a summary.
struct SourceOutcome {
    /// The chain comparison, or `None` when there was nothing to compare.
    chain: Option<Result<rift_store::VerifyReport>>,
    /// Which source answered, as the report names it.
    label: &'static str,
    /// The per-shard tally.
    shards: ShardPasses,
    /// What the recorded order said about itself.
    order: Verdict,
}

async fn run_async(
    root: &std::path::Path,
    voice: Voice,
    witness: Option<&Witness>,
) -> Result<VerifySummary> {
    let paths = RunPaths::from_repository_root(root)?;

    // Prefer the git-authoritative source (re-derive from `rift/checkpoints/v1`);
    // fall back to the committed fixture skeleton on a fresh repo with no branch.
    let outcome = match git_source(&paths.cwd)? {
        Some(source) => verify_against_the_checkpoint_branch(&paths, &source).await?,
        None => verify_against_the_fixture_skeleton(&paths).await,
    };

    let (checkpoints, chain_verdict) = chain_outcome_verdict(outcome.chain)?;
    let summary = VerifySummary {
        checkpoints,
        shards_checked: outcome.shards.checked,
        vacuous_era_shards: outcome.shards.vacuous_era,
        pre_rift_shards: outcome.shards.pre_rift,
        held_unchecked: outcome.shards.held_unchecked,
        source: outcome.label,
        verdict: worse(worse(outcome.order, chain_verdict), outcome.shards.worst),
    };
    if voice == Voice::Loud {
        println!("  repo_id:    {}", paths.repo);
        println!("  projection: {}", paths.db.display());
    }
    record_verification(
        &paths.cache_root,
        &paths.cwd,
        &paths.repo,
        &summary,
        witness,
    )
    .await;
    Ok(summary)
}

/// The git-authoritative run: the recorded order, the chain comparison, and
/// the per-shard passes over `rift/checkpoints/v1`.
///
/// It is this step, and only this step, that opens the object database, so the
/// one place a verification touches git is the one place named after it.
///
/// # Errors
/// Storage errors from the projection read; substrate errors from the passes.
async fn verify_against_the_checkpoint_branch(
    paths: &RunPaths,
    source: &GitCheckpointSource,
) -> Result<SourceOutcome> {
    // The ORDER first: 1:1 with the branch, both directions. It reads
    // git alone, and it is what says whether the order is complete
    // before anything is compared against it.
    let order = chain_completeness(&paths.cwd);
    // THIS COMPARISON WRITES NOTHING. The projection was levelled at the
    // top of `run_at_with` by an independent
    // derivation — the bridge's ingest of the git shards — and what that
    // levelling cannot do is overwrite a row that disagrees, because
    // `record_checkpoint_if_new` is `insert_or_ignore`. So the branch
    // below is reached only when levelling FAILED or every shard was
    // refused; it is no longer the ordinary state of a fresh repository,
    // and it is still never filled in from the comparand — which is what
    // made a fresh repository verify clean by construction.
    let chain = if projection_checkpoints(&paths.db).await? == 0 {
        None
    } else {
        Some(verify_under(&paths.cache_root, &paths.repo, source, Seeding::Forbidden).await)
    };
    // The per-shard passes re-open the reader (cheap; object access only).
    let shards = match GixSubstrate::open(&paths.cwd) {
        Ok(reader) => shard_passes(&reader)?,
        Err(_) => ShardPasses::nothing_walked(),
    };
    Ok(SourceOutcome {
        chain,
        label: "git",
        shards,
        order,
    })
}

/// The M0 skeleton, and the only place seeding survives: there is no
/// repository content to derive a projection from, so the run proves the
/// wiring and says which source it proved it against.
async fn verify_against_the_fixture_skeleton(paths: &RunPaths) -> SourceOutcome {
    let chain = verify_under(
        &paths.cache_root,
        &paths.repo,
        &FixtureSource::parse(FIXTURE),
        Seeding::Permitted,
    )
    .await;
    SourceOutcome {
        chain: Some(chain),
        label: "fixture (no checkpoint branch yet)",
        shards: ShardPasses::nothing_walked(),
        order: Verdict::Ok,
    }
}

/// How many checkpoints were cross-checked, and what the cross-check said.
///
/// # Errors
/// A non-integrity storage error is the run's error; an integrity one is a
/// tamper verdict, because that is the finding this command exists to make.
fn chain_outcome_verdict(
    chain: Option<Result<rift_store::VerifyReport>>,
) -> Result<(usize, Verdict)> {
    match chain {
        // Nothing was compared, because there was nothing to compare.
        None => Ok((
            0,
            Verdict::Structure(
                "the projection holds no checkpoints; run `rift rebuild`. Verify does not seed it \
                 — a projection filled in from the source it is then compared against agrees by \
                 construction and proves nothing."
                    .to_owned(),
            ),
        )),
        Some(Ok(report)) => Ok((report.checkpoints, Verdict::Ok)),
        // A chain divergence is a tamper verdict; it outranks any shard finding.
        Some(Err(rift_core::RiftError::Integrity(m))) => Ok((0, Verdict::Tamper(m))),
        Some(Err(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use rift_core::RepoId;
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};

    pub(super) fn repo() -> RepoId {
        RepoId::new("test-repo").unwrap()
    }

    pub(super) fn hex64(c: char) -> String {
        String::from(c).repeat(64)
    }

    pub(super) fn shard(id: &str, prev: Option<&str>, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.to_owned(),
            dialect: Dialect::Rift,
            prev: prev.map(str::to_owned),
            ordinal,
        }
    }

    /// A shard as read from the LEGACY checkpoint ref — the dialect whose
    /// exemption from the native passes is invariant §1.5.
    pub(super) fn legacy_shard(id: &str, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            dialect: Dialect::Legacy,
            ..shard(id, None, ordinal)
        }
    }

    // --- the two id eras ---------------------------------------------------

    /// The pre-Rift id shapes, taken from the fixtures this repository already
    /// froze rather than invented here: the sharded-branch fixture's content
    /// address is 12 hex characters and the per-checkpoint-refs fixture's is a
    /// 26-character ULID. Neither is a SHA-256 hex digest, and both are real.
    /// (Both live under the import-fixture tree beside this crate; naming them
    /// by fixture rather than by full path is paragraph 2's shed discipline —
    /// a slash-joined path is one high-entropy token to the redaction layer.)
    pub(super) const LEGACY_SHORT_HEX_ID: &str = "ab34cd56ef78";

    /// The other frozen shape — see [`LEGACY_SHORT_HEX_ID`].
    pub(super) const LEGACY_ULID_ID: &str = "01JQZ8T9WQKX3M7VN2P4RB6CDE";

    /// A legacy shard at an explicit walk position.
    pub(super) fn legacy_shard_after(
        id: &str,
        prev: Option<&str>,
        ordinal: u64,
    ) -> CheckpointShard {
        CheckpointShard {
            dialect: Dialect::Legacy,
            ..shard(id, prev, ordinal)
        }
    }

    /// One native shard carrying what the BRIDGE requires (a valid report and a
    /// header naming a root), at an explicit walk position.
    pub(super) fn archived_shard(
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
    pub(super) fn honest_legacy(sub: MemorySubstrate, id: &str, ordinal: u64) -> MemorySubstrate {
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
    pub(super) fn archived(ids: &[(&str, Option<&str>, &str)]) -> MemorySubstrate {
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
    pub(super) const VACUOUS_ERA_ROOT: &str =
        "e2c3e6f55658a556f4403ed646adbc7326b09fcbb23f3df593e7c0de60e64794";

    /// The session every era fixture archives under.
    pub(super) const FIXTURE_SESSION: &str = "01JSESSIONFIXTURE";

    /// The checkpoint id a native archive of `content` under `FIXTURE_SESSION`
    /// (no read facts) lands at — **the writer's own derivation**, so a fixture
    /// addresses its own bytes the way a real shard does.
    ///
    /// The era fixtures used to carry `hex64('a')`-style ids and no `sessions`
    /// field, which meant no fixture could tell a shard that addresses its
    /// contents from one that does not. That is the hole the id pass closes, so
    /// the fixtures had to stop having it.
    pub(super) fn native_id(content: &[u8]) -> String {
        native_id_at(2, content)
    }

    /// The redaction report every fixture shard carries. One definition, because
    /// three copies of it is three chances for the id, the root and the shard to
    /// be computed over different bytes.
    pub(super) fn fixture_report() -> String {
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
    pub(super) fn native_id_at(canon_version: u32, content: &[u8]) -> String {
        use rift_shadow::backend::ShardLeafBytes;
        let report = fixture_report();
        let mut ids = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            canon_version,
            &ShardLeafBytes {
                boundary_gaps: None,
                content_redacted: content,
                full_jsonl: None,
                redaction_report: report.as_bytes(),
                serve_audit: None,
                sessions: None,
                usage: None,
                window_journal: None,
                session_worktrees: None,
                tool_results: None,
                tool_calls: None,
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                spills: &[],
                originals: &[],
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
    pub(super) fn era_shard(
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
    pub(super) fn era_shard_for(
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
    pub(super) fn writer_root(content: &[u8]) -> String {
        use rift_shadow::backend::{shard_merkle_root, ShardLeafBytes};
        let report = fixture_report();
        shard_merkle_root(&ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            serve_audit: None,
            sessions: None,
            usage: None,
            window_journal: None,
            session_worktrees: None,
            tool_results: None,
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            spills: &[],
            originals: &[],
            session_metas: &[],
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        })
        .to_hex()
    }

    /// A root-bound (epoch 3) shard carrying `full.jsonl`, at the id its own
    /// artifacts address. `root_bound_shard` writes it; the tests below edit it.
    pub(super) fn root_bound_shard(content: &[u8], full: &[u8]) -> (MemorySubstrate, String) {
        use rift_shadow::backend::{
            shard_merkle_root, ShardLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
        };
        let report = fixture_report();
        let leaves = ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: content,
            full_jsonl: Some(full),
            redaction_report: report.as_bytes(),
            serve_audit: None,
            sessions: None,
            usage: None,
            window_journal: None,
            session_worktrees: None,
            tool_results: None,
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            spills: &[],
            originals: &[],
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
    pub(super) fn task_bound_shard(
        content: &[u8],
        task_id: &str,
    ) -> (MemorySubstrate, String, Vec<u8>) {
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
            provenance: None,
        }
        .metadata_json();
        let task_leaves = [TaskLeafBytes {
            tool_use_id: task_id,
            metadata: &task_meta,
            transcript_compact: None,
            transcript_full: None,
        }];
        let leaves = ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            serve_audit: None,
            sessions: None,
            usage: None,
            window_journal: None,
            session_worktrees: None,
            tool_results: None,
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &task_leaves,
            spills: &[],
            originals: &[],
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

    /// Build the projection the way production does — the artifact bridge, the
    /// same walk `rift rebuild` runs.
    pub(super) async fn ingest(tmp: &Path, sub: &MemorySubstrate) {
        let store = rift_store::open_capture_projection(projection_db_under(tmp, &repo())).unwrap();
        rift_shadow::ingest_from_substrate(&store, sub, "test-repo", "claude-code", 1_700_000_000)
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod boundary_gaps_tests;

#[cfg(test)]
mod native_read_tests;
