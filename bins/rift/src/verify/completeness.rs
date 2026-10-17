//! The bounded completeness audit: what the client still holds, compared
//! with what the newest checkpoint of each session sealed.
//!
//! The hash and Merkle passes are the integrity check, and they cannot see
//! content that never entered a shard: a transcript sealed short is a shard
//! whose every leaf matches. This pass is additive to them and changes none of
//! them. For every native session the chain names it takes the **newest**
//! checkpoint naming it — an older shard's transcript is a prefix of the newer
//! one by construction, so re-reading it would re-read the same client file —
//! and compares three kinds of sealed artifact with the client's file where
//! the archive found it: the main `transcript.jsonl`, every
//! `tasks/<id>/transcript.jsonl`, and every spill body the `spills` manifests
//! claim. The sealed bytes are the redaction gate's output over the whole
//! client file at archive time, so the client file is re-redacted with the
//! repository's current packs and the two are compared line by line; the
//! shard's report says which pack versions sealed it, and a different list
//! makes the comparison meaningless rather than a finding.
//!
//! **Originals are not audited.** The spool of a rewritten result's original
//! is deleted at seal by design, so there is no client evidence left to
//! compare the sealed original with; counting them would be counting what
//! cannot be checked as checked.
//!
//! **Sources, through the seams the archive used.** The main address is the
//! session cache's `transcript_path`, in the run state of whichever family
//! member captured the session (the chain is the family's; a cache is the
//! capturing member's); a task's is the `SubagentStop` claim for its agent
//! id, else the listing of the session's `subagents/` directory; a spill's is the address the client's own transcript names —
//! read off the bytes already read for that transcript's comparison, since
//! the gate masks a slash-joined path as one entropy token and the sealed
//! copy therefore names no file — resolved by the archive's own containment
//! rule. No directory outside those is listed and no file outside those is
//! opened.
//!
//! **The client's writer is never blocked.** A file is opened read-only and
//! read once; nothing is locked and nothing is held open. The read is bounded
//! in bytes and in sources, never in time: a verdict that read the wall clock
//! would differ between two runs over the same bytes.
//!
//! **Nothing here carries a byte of a transcript.** A finding is a checkpoint
//! id, a session or task id, the artifact's shard-relative name, one word of
//! the closed vocabulary and counts. The vocabulary is [`Word`] and
//! [`Reason`]; each string is spelled once, in `as_str`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use rift_core::{Result, SecretBytes};
use rift_recall_substrate::{Dialect, SubstrateReader};
use rift_shadow::backend::TRANSCRIPT_FULL_NAME;
use rift_shadow::redact::{redact_with, RulePackSet};

use super::artifacts::{shard_header, NativeArtifacts, ShardHeader};
use crate::hooks::subagent_claims::read_claims;
use crate::hooks::{contained, list_subagent_files, subagents_dir};

/// The most of one client file the audit reads.
pub(crate) const PER_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// The most client bytes one run reads in all.
pub(crate) const TOTAL_BYTES: u64 = 128 * 1024 * 1024;
/// The most sources one run opens.
pub(crate) const MAX_SOURCES: usize = 256;

/// The closed vocabulary a source is reported in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Word {
    /// The re-redacted client file is the sealed bytes, line for line.
    Complete,
    /// The client file is gone (`NotFound` at its address).
    ClientFileGone,
    /// The sealed bytes are a strict line-prefix of the client's: the client
    /// wrote on after the seal, which is not a capture omission.
    ClientAppendedAfterSeal,
    /// Neither equal nor a prefix.
    Mismatch,
    /// The comparison could not be made; [`Reason`] says why.
    Unverifiable,
}

impl Word {
    /// Every word, in reporting order.
    pub(crate) const ALL: [Self; 5] = [
        Self::Complete,
        Self::ClientFileGone,
        Self::ClientAppendedAfterSeal,
        Self::Mismatch,
        Self::Unverifiable,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::ClientFileGone => "client_file_gone",
            Self::ClientAppendedAfterSeal => "client_appended_after_seal",
            Self::Mismatch => "mismatch",
            Self::Unverifiable => "unverifiable",
        }
    }
}

/// Why a source was [`Word::Unverifiable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    /// No seam names a client file for the source.
    NoAddress,
    /// The shard was sealed with other pack versions than the repository has
    /// now, so a re-redaction would not reproduce the seal.
    PacksChanged,
    /// The file, or the run, is past its byte or source bound.
    OverBudget,
    /// The address names something that is not a readable regular file.
    Unreadable,
}

impl Reason {
    /// Every reason, in reporting order.
    pub(crate) const ALL: [Self; 4] = [
        Self::NoAddress,
        Self::PacksChanged,
        Self::OverBudget,
        Self::Unreadable,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoAddress => "no_address",
            Self::PacksChanged => "packs_changed",
            Self::OverBudget => "over_budget",
            Self::Unreadable => "unreadable",
        }
    }
}

/// One source's outcome: identifiers, the word, and counts. Never bytes.
#[derive(Clone, Debug)]
pub(crate) struct Finding {
    pub(crate) checkpoint: String,
    pub(crate) session: Option<String>,
    pub(crate) task: Option<String>,
    /// The artifact's shard-relative name.
    pub(crate) artifact: String,
    pub(crate) word: Word,
    pub(crate) reason: Option<Reason>,
    pub(crate) sealed_lines: usize,
    pub(crate) client_lines: usize,
    /// The first line index where the two differ, on a mismatch.
    pub(crate) first_divergent_line: Option<usize>,
    /// The pack versions the shard's report names, on `packs_changed` only.
    pub(crate) sealed_pack_versions: Option<Vec<String>>,
}

/// What one run's audit found, as the summary and the doctor report it.
#[derive(Clone, Debug, Default)]
pub(crate) struct CompletenessSummary {
    pub(crate) findings: Vec<Finding>,
    /// Every source given a word, the unverifiable ones included.
    pub(crate) sources_audited: usize,
    /// Native shards not audited because a newer shard names their session.
    pub(crate) shards_skipped_older: usize,
    /// The pack versions the re-redaction ran with.
    pub(crate) pack_versions: Vec<String>,
}

impl CompletenessSummary {
    /// The audit of a run that audited nothing — the fixture skeleton, and a
    /// repository whose object database will not open.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    pub(crate) fn count(&self, word: Word) -> usize {
        self.findings.iter().filter(|f| f.word == word).count()
    }

    pub(crate) fn reason_count(&self, reason: Reason) -> usize {
        self.findings
            .iter()
            .filter(|f| f.reason == Some(reason))
            .count()
    }

    /// The one-line rendering every surface prints after `completeness:`.
    pub(crate) fn human(&self) -> String {
        let reasons = Reason::ALL
            .iter()
            .map(|r| format!("{} {}", r.as_str(), self.reason_count(*r)))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} complete, {} appended after seal, {} client file gone, {} mismatch, {} \
             unverifiable ({reasons})",
            self.count(Word::Complete),
            self.count(Word::ClientAppendedAfterSeal),
            self.count(Word::ClientFileGone),
            self.count(Word::Mismatch),
            self.count(Word::Unverifiable),
        )
    }

    /// The `--json` object: counts per word and reason, the bounds, and one
    /// object per finding.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let mut doc = serde_json::Map::new();
        for word in Word::ALL {
            doc.insert(word.as_str().to_owned(), self.count(word).into());
        }
        let reasons: serde_json::Map<String, serde_json::Value> = Reason::ALL
            .iter()
            .map(|r| (r.as_str().to_owned(), self.reason_count(*r).into()))
            .collect();
        doc.insert("unverifiable_reasons".to_owned(), reasons.into());
        doc.insert("sources_audited".to_owned(), self.sources_audited.into());
        doc.insert(
            "shards_skipped_older".to_owned(),
            self.shards_skipped_older.into(),
        );
        doc.insert(
            "pack_versions".to_owned(),
            serde_json::json!(self.pack_versions),
        );
        let findings: Vec<serde_json::Value> = self.findings.iter().map(finding_json).collect();
        doc.insert("findings".to_owned(), findings.into());
        doc.into()
    }
}

/// One finding as the JSON document carries it; optional members are absent,
/// not null.
fn finding_json(f: &Finding) -> serde_json::Value {
    let mut doc = serde_json::Map::new();
    doc.insert("checkpoint".to_owned(), f.checkpoint.clone().into());
    if let Some(session) = &f.session {
        doc.insert("session".to_owned(), session.clone().into());
    }
    if let Some(task) = &f.task {
        doc.insert("task".to_owned(), task.clone().into());
    }
    doc.insert("artifact".to_owned(), f.artifact.clone().into());
    doc.insert("outcome".to_owned(), f.word.as_str().into());
    if let Some(reason) = f.reason {
        doc.insert("reason".to_owned(), reason.as_str().into());
    }
    doc.insert("sealed_lines".to_owned(), f.sealed_lines.into());
    doc.insert("client_lines".to_owned(), f.client_lines.into());
    if let Some(at) = f.first_divergent_line {
        doc.insert("first_divergent_line".to_owned(), at.into());
    }
    if let Some(versions) = &f.sealed_pack_versions {
        doc.insert(
            "sealed_pack_versions".to_owned(),
            serde_json::json!(versions),
        );
    }
    doc.into()
}

/// How much one run reads.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    pub(crate) per_file: u64,
    pub(crate) total_bytes: u64,
    pub(crate) sources: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            per_file: PER_FILE_BYTES,
            total_bytes: TOTAL_BYTES,
            sources: MAX_SOURCES,
        }
    }
}

/// Where the client's files are found and what they are re-redacted with.
pub(crate) struct ClientSources {
    /// The git dir of every family member, the verifying one first. Session
    /// caches and claim markers are the run state of the MEMBER whose hooks
    /// captured the session, while the chain is the family's: a session
    /// captured in the main checkout is found from a linked worktree only by
    /// asking the main checkout's run state, through the same resolver the
    /// archive used.
    pub(crate) member_git_dirs: Vec<PathBuf>,
    /// The repository's redactor packs, loaded as the archive loads them.
    pub(crate) packs: RulePackSet,
    pub(crate) budget: Budget,
}

impl ClientSources {
    /// The sources of the repository at `root`, through the same discovery,
    /// the same member listing and the same pack loader as the archive —
    /// never process cwd.
    ///
    /// # Errors
    /// When `root` is not inside a repository.
    pub(crate) fn resolve(root: &Path) -> Result<Self> {
        let repo = rift_git::discover(root)?;
        let (packs, _notes) = crate::redactor_packs::capture_rule_packs(&repo.work_tree);
        let mut member_git_dirs = vec![repo.git_dir.clone()];
        for member in rift_git::family_members(&repo) {
            if !member_git_dirs.contains(&member.git_dir) {
                member_git_dirs.push(member.git_dir);
            }
        }
        Ok(Self {
            member_git_dirs,
            packs,
            budget: Budget::default(),
        })
    }

    /// The main transcript's address, as the session cache of whichever
    /// member captured the session remembers it.
    fn main_address(&self, session: &str) -> Option<PathBuf> {
        self.member_git_dirs.iter().find_map(|git_dir| {
            let sessions = crate::family::run_state::sessions_dir(git_dir);
            rift_shadow::hooks::read_cache(&sessions, session)
                .ok()
                .flatten()?
                .transcript_path
                .map(PathBuf::from)
        })
    }

    /// A task's address: the claim for its agent id in any member's run
    /// state, else the listing beside the main transcript.
    fn task_address(&self, session: &str, main: Option<&Path>, agent_id: &str) -> Option<PathBuf> {
        let claimed = self.member_git_dirs.iter().find_map(|git_dir| {
            read_claims(git_dir, session)
                .remove(agent_id)
                .map(|claim| PathBuf::from(claim.transcript_path))
        });
        if claimed.is_some() {
            return claimed;
        }
        let dir = subagents_dir(main?, session)?;
        list_subagent_files(&dir)
            .into_iter()
            .find(|(id, _)| id == agent_id)
            .map(|(_, path)| path)
    }
}

/// The pack versions `packs` would write into a report: the gate's own
/// statement of them, taken over no input, so the comparison with a shard's
/// `rule_pack_versions` is between two values the same code produced.
///
/// # Errors
/// Never for the empty input; the `Result` is the gate's contract.
pub(crate) fn pack_versions(packs: &RulePackSet) -> Result<Vec<String>> {
    Ok(redact_with(&SecretBytes::new(Vec::new()), packs)?
        .report
        .rule_pack_versions)
}

/// One sealed artifact and the client file it is held to.
struct Source<'a> {
    checkpoint: &'a str,
    session: &'a str,
    task: Option<&'a str>,
    artifact: String,
    sealed: &'a [u8],
    address: Option<PathBuf>,
    /// The versions the shard's report names.
    sealed_versions: &'a [String],
    kind: Kind,
}

/// How a source is compared.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A transcript grows: a sealed prefix is an append.
    Transcript,
    /// A spill is written once: anything but equality is a mismatch.
    Whole,
}

/// One run's state: what was found and what of the budget is left.
struct Run<'a> {
    sources: &'a ClientSources,
    versions: Vec<String>,
    spent: u64,
    summary: CompletenessSummary,
}

/// Audit every native session the chain names against the client's files.
///
/// Held under the run state's report-only guard: resolving an address finds
/// where a hook left it and records nothing, which is what a verification
/// owes a repository.
///
/// # Errors
/// Substrate read errors; the gate's contract error from a re-redaction.
pub(crate) fn audit(
    reader: &impl SubstrateReader,
    sources: &ClientSources,
) -> Result<CompletenessSummary> {
    let _report = crate::family::run_state::report_only();
    let mut run = Run {
        sources,
        versions: pack_versions(&sources.packs)?,
        spent: 0,
        summary: CompletenessSummary::empty(),
    };
    run.summary.pack_versions.clone_from(&run.versions);
    let mut shards: Vec<_> = reader
        .checkpoints()?
        .into_iter()
        .filter(|s| s.dialect == Dialect::Rift)
        .collect();
    // Newest first: the walk's ordinal grows along the chain.
    shards.sort_by_key(|s| std::cmp::Reverse(s.ordinal));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for shard in &shards {
        let id = shard.checkpoint_id.as_str();
        let header = shard_header(reader, id)?;
        // The transcript a shard seals belongs to the first session it names;
        // a shard whose first session a newer shard already named sealed a
        // prefix of what that shard sealed.
        let Some(session) = header.sessions.first().cloned() else {
            continue;
        };
        if seen.contains(&session) {
            run.summary.shards_skipped_older += 1;
            continue;
        }
        seen.extend(header.sessions.iter().cloned());
        run.audit_shard(reader, id, &header, &session)?;
    }
    Ok(run.summary)
}

impl Run<'_> {
    /// Every source one shard seals for `session`.
    fn audit_shard(
        &mut self,
        reader: &impl SubstrateReader,
        id: &str,
        header: &ShardHeader,
        session: &str,
    ) -> Result<()> {
        let sealed_versions = sealed_versions(reader, id)?;
        let native = NativeArtifacts::read(reader, id, header)?;
        let main = self.sources.main_address(session);
        // The client bytes each transcript comparison read, kept for the
        // spill addresses they name; the key is the task, `None` the session's.
        let mut raw: BTreeMap<Option<String>, SecretBytes> = BTreeMap::new();
        if let Some(sealed) = native.transcript_full.as_deref() {
            let read = self.audit_source(Source {
                checkpoint: id,
                session,
                task: None,
                artifact: TRANSCRIPT_FULL_NAME.to_owned(),
                sealed,
                address: main.clone(),
                sealed_versions: &sealed_versions,
                kind: Kind::Transcript,
            });
            raw.extend(read.map(|bytes| (None, bytes)));
        }
        for task in &native.tasks {
            let Some(sealed) = task.transcript_full.as_deref() else {
                continue;
            };
            let address = agent_id_of(&task.metadata)
                .and_then(|agent| self.sources.task_address(session, main.as_deref(), &agent));
            let read = self.audit_source(Source {
                checkpoint: id,
                session,
                task: Some(&task.tool_use_id),
                artifact: format!("tasks/{}/{TRANSCRIPT_FULL_NAME}", task.tool_use_id),
                sealed,
                address,
                sealed_versions: &sealed_versions,
                kind: Kind::Transcript,
            });
            raw.extend(read.map(|bytes| (Some(task.tool_use_id.clone()), bytes)));
        }
        // The archive's containment root: `<dir of the transcript>/<session>`.
        let session_dir = main
            .as_deref()
            .and_then(|m| Some(m.parent()?.join(session)));
        self.audit_spills(
            id,
            session,
            &native,
            session_dir.as_deref(),
            &raw,
            &sealed_versions,
        );
        Ok(())
    }

    /// Every spill body the shard seals, at the address the client's own
    /// transcript names for it.
    fn audit_spills(
        &mut self,
        id: &str,
        session: &str,
        native: &NativeArtifacts,
        session_dir: Option<&Path>,
        raw: &BTreeMap<Option<String>, SecretBytes>,
        sealed_versions: &[String],
    ) {
        let mut by_transcript: BTreeMap<Option<String>, BTreeMap<String, String>> = BTreeMap::new();
        for spill in &native.spills {
            let Some(sealed) = spill.body.as_deref() else {
                continue;
            };
            let address = session_dir.and_then(|dir| {
                let transcript = raw.get(&spill.task)?;
                let names = by_transcript
                    .entry(spill.task.clone())
                    .or_insert_with(|| named_spills(transcript.as_slice()));
                contained(dir, names.get(&spill.claim.tool_use_id)?)
            });
            self.audit_source(Source {
                checkpoint: id,
                session,
                task: spill.task.as_deref(),
                artifact: spill.path(),
                sealed,
                address,
                sealed_versions,
                kind: Kind::Whole,
            });
        }
    }

    /// Give one source its word, within the budget. The client bytes come
    /// back when a file was read, for the addresses a transcript names.
    fn audit_source(&mut self, src: Source<'_>) -> Option<SecretBytes> {
        let sealed_lines = lines(src.sealed).len();
        let mut finding = Finding {
            checkpoint: src.checkpoint.to_owned(),
            session: Some(src.session.to_owned()),
            task: src.task.map(str::to_owned),
            artifact: src.artifact.clone(),
            word: Word::Unverifiable,
            reason: None,
            sealed_lines,
            client_lines: 0,
            first_divergent_line: None,
            sealed_pack_versions: None,
        };
        let mut read = None;
        match self.compare_source(&src) {
            Ok((compared, bytes)) => {
                finding.word = compared.word;
                finding.client_lines = compared.client_lines;
                finding.first_divergent_line = compared.first_divergent_line;
                read = bytes;
            }
            Err(Reason::PacksChanged) => {
                finding.reason = Some(Reason::PacksChanged);
                finding.sealed_pack_versions = Some(src.sealed_versions.to_vec());
            }
            Err(reason) => finding.reason = Some(reason),
        }
        self.summary.findings.push(finding);
        self.summary.sources_audited += 1;
        read
    }

    /// Read and compare, or say why not. A gone file is a word of its own,
    /// carried through the `Ok` arm with no bytes.
    fn compare_source(
        &mut self,
        src: &Source<'_>,
    ) -> std::result::Result<(Compared, Option<SecretBytes>), Reason> {
        if src.sealed_versions != self.versions.as_slice() {
            return Err(Reason::PacksChanged);
        }
        let Some(address) = &src.address else {
            return Err(Reason::NoAddress);
        };
        if self.summary.sources_audited >= self.sources.budget.sources {
            return Err(Reason::OverBudget);
        }
        let remaining = self.sources.budget.total_bytes.saturating_sub(self.spent);
        let bound = self.sources.budget.per_file.min(remaining);
        let bytes = match read_client(address, bound) {
            Ok(bytes) => bytes,
            Err(ReadFailure::Gone) => {
                let gone = Compared {
                    word: Word::ClientFileGone,
                    client_lines: 0,
                    first_divergent_line: None,
                };
                return Ok((gone, None));
            }
            Err(ReadFailure::OverBudget) => return Err(Reason::OverBudget),
            Err(ReadFailure::Unreadable) => return Err(Reason::Unreadable),
        };
        self.spent = self
            .spent
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let raw = SecretBytes::new(bytes);
        let current = redact_with(&raw, &self.sources.packs).map_err(|_| Reason::Unreadable)?;
        let compared = compare(src.sealed, current.redacted.as_slice(), src.kind);
        Ok((compared, Some(raw)))
    }
}

/// What a comparison concluded.
struct Compared {
    word: Word,
    client_lines: usize,
    first_divergent_line: Option<usize>,
}

/// The lines of a document, a final line break read as a terminator rather
/// than as an empty last line, so a file that ends in one compares with a
/// file that ends in two lines as two lines against one.
fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Line by line: equal is complete; a sealed strict prefix is an append for
/// a transcript and a mismatch for a spill; anything else a mismatch naming
/// the first index where the two differ or one ends.
fn compare(sealed: &[u8], current: &[u8], kind: Kind) -> Compared {
    let (sealed, current) = (lines(sealed), lines(current));
    let divergent = sealed
        .iter()
        .zip(current.iter())
        .position(|(a, b)| a != b)
        .or_else(|| (sealed.len() != current.len()).then_some(sealed.len().min(current.len())));
    let word = match divergent {
        None => Word::Complete,
        Some(at) if at == sealed.len() && kind == Kind::Transcript => Word::ClientAppendedAfterSeal,
        Some(_) => Word::Mismatch,
    };
    Compared {
        word,
        client_lines: current.len(),
        first_divergent_line: (word == Word::Mismatch).then_some(divergent).flatten(),
    }
}

/// Why a client file yielded no bytes.
enum ReadFailure {
    Gone,
    OverBudget,
    Unreadable,
}

/// The client file's bytes, at most `bound` of them, by one read-only open.
/// A link is not followed and a non-file is not read: the archive sealed
/// neither, so neither is evidence. Nothing is locked and nothing is held.
fn read_client(path: &Path, bound: u64) -> std::result::Result<Vec<u8>, ReadFailure> {
    let inspected = std::fs::symlink_metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ReadFailure::Gone,
        _ => ReadFailure::Unreadable,
    })?;
    if !inspected.file_type().is_file() {
        return Err(ReadFailure::Unreadable);
    }
    if inspected.len() > bound {
        return Err(ReadFailure::OverBudget);
    }
    let file = std::fs::File::open(path).map_err(|_| ReadFailure::Unreadable)?;
    let mut bytes = Vec::new();
    file.take(bound.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ReadFailure::Unreadable)?;
    // The file grew between the inspection and the read: still bounded.
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > bound {
        return Err(ReadFailure::OverBudget);
    }
    Ok(bytes)
}

/// The pack versions the shard's redaction report names; empty when the
/// report is absent or does not parse, which no current pack list equals.
fn sealed_versions(reader: &impl SubstrateReader, id: &str) -> Result<Vec<String>> {
    let Some(bytes) = reader.read_artifact(id, "redaction_report.json")? else {
        return Ok(Vec::new());
    };
    Ok(
        rift_recall_substrate::parse::parse_redaction_report(&bytes, id)
            .value
            .rule_pack_versions,
    )
}

/// The `agent_id` a task's sealed metadata records.
fn agent_id_of(metadata: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(metadata)
        .ok()?
        .get("agent_id")?
        .as_str()
        .map(str::to_owned)
}

/// Tool-use id → the address its result names in the client's transcript,
/// read as the archive read it: the structured `persistedOutputPath` when the
/// record holds one result, else the front of the client's envelope. The
/// client's bytes rather than the sealed ones, because the gate masks a path
/// as one entropy token and the sealed copy names no file.
fn named_spills(transcript: &[u8]) -> BTreeMap<String, String> {
    const ENVELOPE_OPEN: &str = "<persisted-output>";
    const ENVELOPE_SAVED: &str = "Full output saved to: ";
    let mut named = BTreeMap::new();
    for line in transcript.split(|b| *b == b'\n') {
        let mentions = |needle: &str| line.windows(needle.len()).any(|w| w == needle.as_bytes());
        if !mentions("persistedOutputPath") && !mentions(ENVELOPE_OPEN) {
            continue;
        }
        let Ok(serde_json::Value::Object(record)) = serde_json::from_slice(line) else {
            continue;
        };
        let structured = record
            .get("toolUseResult")
            .and_then(|r| r.get("persistedOutputPath"))
            .and_then(serde_json::Value::as_str);
        let results: Vec<&serde_json::Value> = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("tool_result"))
            .collect();
        let single = results.len() == 1;
        for block in results {
            let Some(id) = block.get("tool_use_id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let envelope = || {
                let text = match block.get("content")? {
                    serde_json::Value::String(s) => s.as_str(),
                    serde_json::Value::Array(blocks) => blocks.first()?.get("text")?.as_str()?,
                    _ => return None,
                };
                let mut lines = text
                    .strip_prefix(ENVELOPE_OPEN)?
                    .strip_prefix('\n')?
                    .lines();
                Some(lines.next()?.split_once(ENVELOPE_SAVED)?.1.to_owned())
            };
            let Some(address) = structured
                .filter(|_| single)
                .map(str::to_owned)
                .or_else(envelope)
            else {
                continue;
            };
            named.entry(id.to_owned()).or_insert(address);
        }
    }
    named
}

#[cfg(test)]
pub(crate) mod tests;
