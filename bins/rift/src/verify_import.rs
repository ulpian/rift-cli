//! The three verbs that read the checkpoint branch back against the
//! projection: `verify` re-derives it, `rebuild` projects it, `import`
//! reads a pre-Rift tool's shards (or an agent's transcript store) into it.
//!
//! Together because they share one stance: the branch is the source, the
//! projection is rebuildable, and nothing here writes a chain position.

use std::collections::BTreeMap;
use std::path::Path;

use rift_recall_substrate::{Dialect, GixSubstrate, SubstrateReader as _};
use rift_shadow::legacy_import::{parse_shard, ImportReport, Layout};

use crate::cli::{self, exit, Outcome};
use crate::projection::{not_init_guard, open_projection, runtime};
use crate::{family, hooks, session_verbs, verify};

/// `rift verify`: carries its own granular exit contract — 0 ok,
/// 1 tamper, 3 redaction, 5 structure, 70 internal.
///
/// A chain belongs to a repository family, so "there is no family here" is a
/// precondition, not a bug: outside a repository, or in a member whose
/// identity has never been minted, there is nothing to verify and the refusal
/// names the vantage that fixes it. Checked first rather than caught with the
/// unexpected failures, which are reported as internal — this one is not.
pub(crate) fn verify_outcome(root: &Path) -> Outcome {
    if let Err(e) = family::resolve_at(root) {
        return Outcome::coded(
            exit::NOT_INIT,
            format!("verify: {e}"),
            serde_json::json!({ "verify": "no_family_identity", "detail": e.to_string() }),
        );
    }
    match verify::run_at(root) {
        Ok(summary) => verdict_outcome(&summary),
        Err(e) => Outcome::coded(
            exit::INTERNAL,
            format!("verify: internal error ({e})"),
            serde_json::json!({ "verify": "internal", "detail": e.to_string() }),
        ),
    }
}

/// The verdict as an outcome, each carrying its own exit code.
fn verdict_outcome(summary: &verify::VerifySummary) -> Outcome {
    use verify::Verdict;
    let base = format!(
        "re-derived {} checkpoint(s) from {}; {} shard(s) redaction/Merkle-checked{}",
        summary.checkpoints,
        summary.source,
        summary.shards_checked,
        verify_exemptions(summary)
    );
    match &summary.verdict {
        Verdict::Ok => Outcome::coded(
            exit::OK,
            format!("verify: OK — {base}"),
            serde_json::json!({ "verify": "ok", "checkpoints": summary.checkpoints,
                                "shards_checked": summary.shards_checked,
                                "vacuous_era_shards": summary.vacuous_era_shards,
                                "pre_rift_shards": summary.pre_rift_shards,
                                "held_unchecked": summary.held_unchecked }),
        ),
        Verdict::Tamper(d) => Outcome::coded(
            1,
            format!("verify: TAMPER — {d} ({base})"),
            serde_json::json!({ "verify": "tamper", "detail": d }),
        ),
        Verdict::Redaction(d) => Outcome::coded(
            3,
            format!("verify: REDACTION — {d} ({base})"),
            serde_json::json!({ "verify": "redaction", "detail": d }),
        ),
        Verdict::Structure(d) => Outcome::coded(
            5,
            format!("verify: STRUCTURE — {d} ({base})"),
            serde_json::json!({ "verify": "structure", "detail": d }),
        ),
    }
}

/// Every exemption is named in the line, never merely counted in the JSON:
/// those shards did not get a pass the line otherwise implies they got, so a
/// bare "N shards Merkle-checked" would be an overstatement by exactly that
/// many. The two counts are two different exemptions and are printed apart on
/// purpose: one sentence used to cover both, and it was only true of the
/// vacuous era — a pre-Rift shard's root is whatever its own dialect wrote,
/// not the empty-set constant. The single-session id-pass carve-out is
/// retired: the id derivation is defined for every session set a writer can
/// mint, so the pass runs on every native shard and a set no era's rule
/// grants reds as tamper instead of being counted past.
fn verify_exemptions(summary: &verify::VerifySummary) -> String {
    let mut notes = Vec::new();
    if summary.vacuous_era_shards > 0 {
        notes.push(format!(
            "{} predate the derived Merkle root and carry the empty-set constant, \
             asserted, with their capture bound by the checkpoint id instead",
            summary.vacuous_era_shards
        ));
    }
    if summary.pre_rift_shards > 0 {
        notes.push(format!(
            "{} are the pre-Rift dialect and are exempt from the native passes",
            summary.pre_rift_shards
        ));
    }
    if summary.held_unchecked > 0 {
        notes.push(format!(
            "{} rewritten range(s) are held substitutions whose held revision was not read \
             (held_unchecked)",
            summary.held_unchecked
        ));
    }
    if notes.is_empty() {
        return String::new();
    }
    format!(" ({})", notes.join("; "))
}

/// `rift import --from-legacy` — read a pre-Rift capture tool's checkpoints out
/// of this repository, in **both** on-disk layouts it shipped — or, with an
/// agent named, bulk-import that agent's transcript store as read-only
/// checkpoints.
///
/// The first version of this verb claimed both layouts and read only one: the
/// per-checkpoint-refs shards were never enumerated and the layout was
/// hardcoded, so a repository carrying only that layout got "no pre-Rift
/// checkpoints found" and exit 0. The walk now exists
/// (`ShardWalker::legacy_ref_shard_ids`) and each shard's layout is **carried
/// from the enumeration that produced it**, never assumed and never re-derived
/// from a ref name a foreign writer chose.
///
/// **Read-only on the source, and untrusting of it.** Every shard is parsed
/// leniently, its redaction report is required before it counts as importable
/// (the same precondition the artifact bridge applies to Rift's own shards),
/// and its claimed checkpoint root is **re-derived** from its own events before
/// anything is said about it. What that re-derivation can conclude — and why an
/// imported checkpoint is never a chain anchor — is in
/// `rift_shadow::legacy_import`'s module header, not paraphrased here.
///
/// Imported bytes reach durable storage only through the ordinary redaction
/// gate; this verb reports, and the projection write is the existing bridge.
pub(crate) fn cmd_import(root: &Path, a: &cli::ImportArgs) -> rift_core::Result<Outcome> {
    if let Some(agent) = &a.agent {
        return import_agent_store(root, agent, a);
    }
    if !a.from_legacy {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "import: nothing to do — name an agent (e.g. `rift import claude-code`) \
             or pass --from-legacy to read pre-Rift checkpoints",
            serde_json::json!({ "error": "no_source_selected" }),
        ));
    }
    let reader = match legacy_reader(root) {
        Ok(reader) => reader,
        Err(refusal) => return Ok(refusal),
    };
    let scan = scan_legacy_shards(&reader)?;
    let written = write_legacy_import(root, &reader, &scan.importable)?;
    Ok(Outcome::ok(
        import_lines(&scan, written.as_ref()),
        import_json(&scan, written.as_ref()),
    ))
}

/// The agent arm: the agent's transcript store as read-only checkpoints. Same
/// init guard as every query surface, after the hook-environment refusal:
/// where the command runs is judged before whether the repository is set up
/// (see the attach route).
fn import_agent_store(root: &Path, agent: &str, a: &cli::ImportArgs) -> rift_core::Result<Outcome> {
    if let Some(refusal) = session_verbs::refuse_in_hooks("import") {
        return Ok(refusal);
    }
    if let Some(o) = not_init_guard(root) {
        return Ok(o);
    }
    let store = open_projection(root)?;
    let rt = runtime()?;
    session_verbs::import(
        &store,
        &rt,
        root,
        agent,
        a.dry_run,
        a.session.as_deref(),
        a.path.as_deref(),
    )
}

/// The substrate the legacy shards are read from, or the refusal that stands
/// in for it. Local only, for the same reason the bridge is: the source shards
/// are another tool's raw output and must not be pulled across a network.
fn legacy_reader(root: &Path) -> Result<GixSubstrate, Outcome> {
    let not_a_repo = || {
        Outcome::coded(
            exit::NOT_INIT,
            "import: not a git repository",
            serde_json::json!({ "error": "not_a_repo" }),
        )
    };
    let Ok(repo) = rift_git::discover(root) else {
        return Err(not_a_repo());
    };
    let Ok(reader) = GixSubstrate::open(&repo.work_tree) else {
        return Err(not_a_repo());
    };
    if !reader.is_local_only() {
        return Err(Outcome::coded(
            exit::PRECONDITION,
            "import: refusing a non-local repository",
            serde_json::json!({ "error": "not_local" }),
        ));
    }
    Ok(reader)
}

/// **The walk that found a shard is what knows its layout.** The previous
/// version asked `layout_of_shard_ref` to re-derive it from the ref NAME and
/// fell back to a hardcoded `ShardedBranch` whenever the name did not
/// classify — so a junk-named ref in a repository with no branch at all
/// printed `layout sharded-branch: 3`, which an adversarial review planted.
/// Naming a layout the repository does not contain is worse than declining to
/// name one, and neither is necessary: the two enumerations here are exactly
/// the two `checkpoints()` unions to build the list being labelled, so between
/// them they attribute every legacy shard it can yield.
///
/// Deliberately NOT re-classified by name: `legacy_ref_shard_ids` already
/// takes each id from the shard TREE rather than the ref name, precisely
/// because a writer is free to spell the name differently. A shard whose tree
/// hangs off a per-checkpoint ref is in that layout however the ref is
/// spelled; the name's spelling is a fact about the ref, not the layout.
/// Nothing from the name reaches the output either way — the tokens are this
/// build's own `&'static str`s. Refs win on overlap, matching the order
/// `shards_all` unions them in.
fn legacy_layouts(reader: &GixSubstrate) -> rift_core::Result<BTreeMap<String, Layout>> {
    let walker = reader.walker();
    let mut layout_by_id = BTreeMap::new();
    for id in walker.shard_ids(Dialect::Legacy)? {
        layout_by_id.insert(id, Layout::ShardedBranch);
    }
    for (id, _ref_name) in walker.legacy_ref_shard_ids()? {
        layout_by_id.insert(id, Layout::PerCheckpointRefs);
    }
    Ok(layout_by_id)
}

/// What the read half of the import found.
struct LegacyScan {
    report: ImportReport,
    /// Shards in the per-checkpoint-refs layout, counted off the map so one
    /// id carried by two refs counts once.
    refs_layout_shards: usize,
    /// Legacy shards no enumeration claimed. Structurally unreachable (the
    /// layout map and the shard list come from the same two walks) and
    /// therefore an ARGUMENT, not a tested claim — which is why it is COUNTED
    /// and reported rather than folded into whichever layout is alphabetically
    /// first. A number a reader can see beats a label a reader can believe.
    unattributed: usize,
    /// Every IMPORTABLE shard (valid redaction report — the same gate
    /// `ImportReport::observe` uses for `importable`/`root_checks`), kept for
    /// the durable-claim write. This is the layout-aware pass: only here does
    /// the CLI know which ref namespace a shard came from, a fact
    /// `rift_shadow::ingest_dialect`'s `SubstrateReader`-based walk cannot see
    /// (see `imported_checkpoint`'s doc comment in `schema.rs`).
    importable: Vec<rift_shadow::legacy_import::Shard>,
}

/// Parse every legacy shard the substrate holds. Only the legacy dialect:
/// `--from-legacy` must never re-report this repository's own native
/// checkpoints as an import.
fn scan_legacy_shards(reader: &GixSubstrate) -> rift_core::Result<LegacyScan> {
    let layout_by_id = legacy_layouts(reader)?;
    let mut scan = LegacyScan {
        report: ImportReport::default(),
        refs_layout_shards: layout_by_id
            .values()
            .filter(|l| **l == Layout::PerCheckpointRefs)
            .count(),
        unattributed: 0,
        importable: Vec::new(),
    };
    for shard in reader.checkpoints()? {
        if shard.dialect != Dialect::Legacy {
            continue;
        }
        let Some(layout) = layout_by_id.get(&shard.checkpoint_id).copied() else {
            scan.unattributed += 1;
            continue;
        };
        let artifacts = shard_artifacts(reader, &shard.checkpoint_id);
        let parsed = parse_shard(&shard.checkpoint_id, layout, &artifacts);
        scan.report.observe(&parsed);
        if parsed.report_is_valid() {
            scan.importable.push(parsed);
        }
    }
    Ok(scan)
}

/// The artifacts one shard carries, by name — absent ones simply absent.
fn shard_artifacts(reader: &GixSubstrate, checkpoint_id: &str) -> BTreeMap<String, Vec<u8>> {
    let mut artifacts = BTreeMap::new();
    for name in rift_shadow::legacy_import::SHARD_ARTIFACTS {
        if let Ok(Some(bytes)) = reader.read_artifact(checkpoint_id, name) {
            artifacts.insert((*name).to_owned(), bytes);
        }
    }
    artifacts
}

/// What the write half of the import did.
struct LegacyWrite {
    bridge: rift_shadow::BridgeReport,
    /// Durable rows written to `imported_checkpoint`, one per importable
    /// shard not already claimed.
    claims_written: usize,
}

/// THE WRITE. This verb used to report and stop, and "imported history" had
/// no referent anywhere in the product. Measured before it was written, and
/// the measurement narrows the finding: the artifact bridge's
/// event-projection half is NOT inside its `Sha256Id::parse_hex(id)` guard,
/// so `rift rebuild` has always projected a pre-Rift shard's events — the very
/// rows an import would write — while the verb named "import" wrote nothing.
///
/// Dialect-scoped, so import stays import: an unfiltered ingest would also
/// sweep this repository's own native checkpoints, which is `rift rebuild`'s
/// job. Idempotent by construction, not by a cache: every row id is
/// content-addressed and every writer is `INSERT … OR IGNORE`, so a second
/// run over unchanged shards inserts zero rows —
/// `a_second_legacy_import_inserts_no_rows` proves it by comparing the store's
/// own row COUNTS across the two runs.
///
/// The durable claim — one row per importable shard, from the layout-aware
/// pass — is written BEFORE the event projection, so a crash between the two
/// still leaves the claim durable (the event write is separately idempotent
/// and re-running costs nothing). Never a `checkpoint` row, never a chain
/// position: see `imported_checkpoint`'s doc comment.
///
/// `None` when the repository has no projection: it was never `rift init`ed.
/// Reporting what is there is still useful and still true, so the read half
/// runs and the write half says it did not happen — rather than the verb
/// failing outright on a repository it can perfectly well read.
fn write_legacy_import(
    root: &Path,
    reader: &GixSubstrate,
    importable: &[rift_shadow::legacy_import::Shard],
) -> rift_core::Result<Option<LegacyWrite>> {
    let Ok(store) = open_projection(root) else {
        return Ok(None);
    };
    let repo_id = family::resolve_at(root)?.family_id;
    let rt = runtime()?;
    let imported_at = hooks::now_ms();
    rt.block_on(async {
        let mut claims_written = 0usize;
        for shard in importable {
            let rec = rift_shadow::legacy_import::imported_checkpoint_record(shard, imported_at);
            if rift_store::record_imported_checkpoint_if_new(&store, &rec).await? {
                claims_written += 1;
            }
        }
        let bridge = rift_shadow::ingest_dialect(
            &store,
            reader,
            &repo_id.to_string(),
            "claude-code",
            imported_at,
            Some(Dialect::Legacy),
        )
        .await?;
        Ok(Some(LegacyWrite {
            bridge,
            claims_written,
        }))
    })
}

/// The report as the operator reads it. An importable shard now gets a
/// durable row in `imported_checkpoint` — said on every run that wrote (or
/// confirmed) one, because the row's existence is the part a reader will
/// otherwise assume away. What that row does NOT grant is said in the same
/// breath: no `checkpoint` row, no chain position. Its id is not a SHA-256,
/// and minting a Rift one would put a row in the projection the git side
/// cannot produce — which `rift verify` reads as tamper from then on.
fn import_lines(scan: &LegacyScan, written: Option<&LegacyWrite>) -> String {
    let report = &scan.report;
    let mut line = if report.importable == 0 && report.refused == 0 && scan.unattributed == 0 {
        "import: no pre-Rift checkpoints found in this repository".to_owned()
    } else {
        report.render().trim_end().to_owned()
    };
    if scan.unattributed > 0 {
        line.push_str(&format!(
            "\nlayout unattributed: {} checkpoint(s) — \
             found, but no enumeration claimed them",
            scan.unattributed
        ));
    }
    let Some(written) = written else {
        line.push_str(
            "\nnothing was written: this repository has no projection yet (run `rift init`)",
        );
        return line;
    };
    if report.importable > 0 || written.bridge.events_derived > 0 {
        line.push_str(&format!(
            "\nwrote {} event row(s) ({} derived); re-running writes none — \
             every row id is content-addressed",
            written.bridge.events_inserted, written.bridge.events_derived
        ));
    }
    if report.importable > 0 {
        line.push_str(&format!(
            "\nwrote {} durable checkpoint claim(s) \
             ({} importable); re-running writes none — every row id is \
             content-addressed\
             \nno chain position is recorded for an imported checkpoint — \
             the row is a durable claim, never an anchor",
            written.claims_written, report.importable
        ));
    }
    line
}

/// The `--json` document. Counted per LAYOUT, which is a number that means
/// something: the previous `sources` field counted refs that had passed
/// through `branch_tips()` — a list that excludes the legacy namespace by
/// contract — so it was structurally always 0. `chain_positions` is named
/// explicitly so a consumer cannot infer one from the claims count going
/// non-zero.
fn import_json(scan: &LegacyScan, written: Option<&LegacyWrite>) -> serde_json::Value {
    serde_json::json!({
        "by_layout": scan.report.seen,
        "refs_layout_shards": scan.refs_layout_shards,
        "unattributed": scan.unattributed,
        "importable": scan.report.importable,
        "refused": scan.report.refused,
        "events": scan.report.events,
        "root_checks": scan.report.root_checks,
        "trust": "claim",
        "wrote": written.map_or_else(
            || serde_json::json!(null),
            |w| serde_json::json!({
                "shards_projected": w.bridge.shards_projected,
                "events_derived": w.bridge.events_derived,
                "events_inserted": w.bridge.events_inserted,
                "refused": w.bridge.refused.len(),
                "imported_checkpoint_claims_written": w.claims_written,
                "chain_positions": 0,
            }),
        ),
    })
}

/// `rift rebuild`: run the artifact bridge — rebuild the projection from the
/// redacted checkpoint branch alone (delete `projection.db` first for a
/// from-scratch rebuild; over a live projection it is a set-union no-op).
pub(crate) fn cmd_rebuild(root: &Path) -> rift_core::Result<Outcome> {
    let cwd = root.to_path_buf();
    // The substrate first: "not a git repository" is the plainer answer, and
    // asking for a family identity before it would refuse in the vocabulary of
    // the thing that is missing second.
    let Ok(reader) = rift_recall_substrate::GixSubstrate::open(&cwd) else {
        return Ok(Outcome::coded(
            exit::NOT_INIT,
            "rebuild: not a git repository",
            serde_json::json!({ "error": "not_a_repo" }),
        ));
    };
    let repo_id = family::resolve_at(&cwd)?.family_id;
    let store = open_projection(root)?;
    let rt = runtime()?;
    let report = rt.block_on(rift_shadow::ingest_from_substrate(
        &store,
        &reader,
        &repo_id.to_string(),
        "claude-code",
        hooks::now_ms(),
    ))?;
    let json = serde_json::json!({
        "branch": report.branch,
        "shards_seen": report.shards_seen,
        "shards_projected": report.shards_projected,
        "shards_skipped": report.shards_skipped,
        "refused": report.refused.len(),
        "parse_issues": report.parse_issues,
        "events_derived": report.events_derived,
        "events_inserted": report.events_inserted,
        "watermark": report.tip,
    });
    Ok(Outcome::ok(report.render().trim_end().to_owned(), json))
}
