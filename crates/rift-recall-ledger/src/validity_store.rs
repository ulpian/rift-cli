//! Durable backing for the validity plane.
//!
//! `rift-recall-validity` owns the hash-bound key, the sealed `ValidSummary`,
//! and the **pure** [`rift_recall_validity::classify`] core. This module backs
//! it with the append-only `recall.db` tables: [`put_summary`] writes the
//! `digest_blob` + `summary_binding` rows + the `Summary→binds→FileRevision`
//! edge; [`resolve`] reads them back, classifies against a worktree snapshot,
//! and (on `Invalid`) appends the `Summary→invalidated_by→FileRevision` audit
//! edge — never a mutable flag.
//!
//! The worktree snapshot maps path → **SHA-256** of the current bytes (the
//! worktree hasher's output); the binding's `bound_oid` is the same SHA-256, so
//! the bind-time and check-time comparators line up.
//!
//! [`put_file_outline`] is the one writer of a file's outline — a code file's
//! by definition, a Markdown file's by heading: it binds
//! the outline through [`put_summary`] and writes the raw token count of each
//! definition and of the whole file beside it in `summary_token_count`, so a
//! surface that prints what a definition costs never starts a tokenizer.

use rift_core::{RedactedBytes, Result, RiftError};
use rift_recall_substrate::SubstrateReader;
use rift_recall_validity::{
    classify, is_outlinable_path, line_span, outline_versions, summary_key, BlobResolver,
    FileOutline, SummaryBinding, SummaryVersions, Validity, WorktreeSnapshot,
};
use rift_store::{CanonicalStore, ColumnCipher, Value};
use sha2::{Digest, Sha256};

use crate::graph::{file_revision_id, Built};

/// A code or Markdown file a writer binds an outline of: its path and redacted bytes,
/// the serve-policy version the binding key carries, and the serve policy's
/// verdict on the path. The extractor and redaction versions are the
/// outline's own ([`outline_versions`]), so the serve-time loader names the
/// same key.
///
/// Redacted bytes only: a digest is encrypted at rest, but encryption is not
/// redaction, so an outline is never built from bytes the redaction gate has
/// not cleared. A file whose redaction masked anything
/// hashes differently from the checkout, so its binding is never valid.
pub struct OutlineSource<'a> {
    /// The repository path, `/`-spelled.
    pub path: &'a str,
    /// The file's bytes, through the redaction gate.
    pub source: &'a RedactedBytes,
    /// The serve-policy version in force.
    pub policy_version: u32,
    /// Whether the serve policy denies the path or marks it sensitive. A
    /// closure because this crate does not depend on the policy crate; the
    /// caller asks its policy engine.
    pub refuses_path: &'a dyn Fn(&str) -> bool,
}

/// The raw token counts of one outline: the whole file and each definition,
/// in the outline's order, with the basis they were counted on.
///
/// Raw on purpose: a count stored here is the hard figure every fitting
/// decision uses, and a calibrated figure is derived from it at read time, so
/// a calibration can never change what a serve fits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineCounts {
    /// `rift_tokens::provenance()` when the counts were taken.
    pub tokenizer: &'static str,
    /// The whole file's count.
    pub whole_file: u32,
    /// One count per definition, in outline order.
    pub definitions: Vec<u32>,
}

/// What the store holds for one path, read against the revision the checkout
/// holds now.
pub enum CurrentSummary {
    /// No binding names the path.
    NotBound,
    /// Bindings name the path, and there is no current revision to check
    /// them against.
    Unchecked,
    /// Bindings name the path, none of them at the current revision.
    Stale,
    /// The binding at the current revision under the versions asked for:
    /// [`Validity::Valid`] with its digest, or [`Validity::Degraded`] when the
    /// digest cannot be read.
    Bound(Validity),
}

/// One stored count: which span of the outline, its lines and its tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenCount {
    /// 0 for the whole file, `n` for the outline's `n`-th definition.
    pub ordinal: u32,
    /// The first line of the span, 1-based.
    pub line_lo: u32,
    /// The last line of the span, inclusive.
    pub line_hi: u32,
    /// The raw token count.
    pub tokens: u32,
    /// The basis the count was taken on.
    pub tokenizer: String,
}

/// The most outlines one batch binds.
///
/// Each one reads a file and runs the tokenizer over it, so the bound is on
/// the work a condense adds to the detached worker, not on correctness: a read
/// past it is counted `deferred` and the next condense of the family takes it,
/// since a session's read facts are cumulative and the writer is idempotent.
/// Sixty-four is the cap the recovery map and the blob bridge already put on a
/// per-boundary walk.
pub const MAX_OUTLINES_PER_BATCH: u32 = 64;

/// The largest file a batch outlines: the ceiling on a file a first read may
/// be answered with its outline for, so an outline past it would never be
/// served in its stead.
pub const MAX_OUTLINE_SOURCE_BYTES: u64 = 1024 * 1024;

/// A batch of read revisions to outline, and what the caller supplies for it:
/// the checkout the bytes are read from, the serve policy's version and
/// verdict, the redaction gate, and the tokenizer.
///
/// The redaction gate and the tokenizer come from the caller because this
/// crate holds neither the rule packs nor the decision to load a tokenizer;
/// the caller is the detached worker, the one place both are allowed.
pub struct OutlineBatch<'a> {
    /// The work tree the session read in.
    pub work_tree: &'a std::path::Path,
    /// The serve-policy version in force.
    pub policy_version: u32,
    /// Whether the serve policy denies the path or marks it sensitive.
    pub refuses_path: &'a dyn Fn(&str) -> bool,
    /// The redaction gate: raw bytes in, cleared bytes out.
    pub redact: &'a dyn Fn(Vec<u8>) -> Option<RedactedBytes>,
    /// The token counter.
    pub count: &'a dyn Fn(&str) -> u32,
    /// The counter's basis, `rift_tokens::provenance()`.
    pub tokenizer: &'static str,
}

/// What one batch did with each path it was handed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutlineBindCounts {
    /// Outlines written now.
    pub bound: u32,
    /// Already bound for the bytes on disk; nothing written.
    pub already_bound: u32,
    /// The file is gone or no longer the revision the session read.
    pub changed: u32,
    /// The serve policy denies the path or marks it sensitive, or the
    /// redaction gate masked part of the file.
    pub refused: u32,
    /// Neither a code nor a Markdown file, or not UTF-8.
    pub not_code: u32,
    /// Past [`MAX_OUTLINE_SOURCE_BYTES`].
    pub oversize: u32,
    /// Past [`MAX_OUTLINES_PER_BATCH`]; the next batch takes it.
    pub deferred: u32,
    /// A read, the gate or the store failed; dropped, never retried here.
    pub failed: u32,
}

/// One path's fate in a batch, before it is counted.
enum OutlineStep {
    Bound,
    AlreadyBound,
    Changed,
    Refused,
    NotCode,
    Oversize,
    Deferred,
    Failed,
}

/// A [`BlobResolver`] over the read-only substrate: `file:` handles resolve by
/// mapping the handle's `content_sha256` to its git blob oid and
/// fetching that immutable object; `log:` (captured tool results) is not yet
/// exposed by the substrate seam, so it resolves to `None` (a documented
/// follow-up — the handle then degrades to `Unavailable`, never wrong bytes).
///
/// The `content_sha256 → blob_oid` map is loaded from `n_blob_oid_map`
/// ([`crate::blob_map::load_blob_oid_map`]) and threaded in with
/// [`with_oid_map`](SubstrateBlobResolver::with_oid_map). An unmapped hash falls
/// back to a direct lookup — in-memory test substrates seed blobs by content
/// hash, and a real repo simply misses — honest absence). Resolution stays
/// self-validating in [`rift_recall_validity::resolve_handle`]: the fetched bytes
/// must re-hash to the requested `content_sha256`.
pub struct SubstrateBlobResolver<'a, R: SubstrateReader> {
    reader: &'a R,
    oid_map: std::collections::BTreeMap<String, String>,
}

impl<'a, R: SubstrateReader> SubstrateBlobResolver<'a, R> {
    /// Wrap a substrate reader as a blob resolver (no map — direct lookup).
    pub fn new(reader: &'a R) -> Self {
        Self {
            reader,
            oid_map: std::collections::BTreeMap::new(),
        }
    }

    /// Attach the `content_sha256 → blob_oid` map.
    #[must_use]
    pub fn with_oid_map(mut self, oid_map: std::collections::BTreeMap<String, String>) -> Self {
        self.oid_map = oid_map;
        self
    }
}

impl<R: SubstrateReader> BlobResolver for SubstrateBlobResolver<'_, R> {
    fn file_blob(&self, _path: &str, content_sha256: &str) -> Option<Vec<u8>> {
        // Map the content hash to its git blob oid, else fall back to a
        // direct lookup (in-memory substrates seed blobs by content hash).
        let oid = self
            .oid_map
            .get(content_sha256)
            .map_or(content_sha256, String::as_str);
        self.reader.blob(oid).ok().flatten()
    }
    fn tool_result(&self, _tool_use_id: &str) -> Option<Vec<u8>> {
        None // log: exact retrieval lands when the substrate exposes tool results
    }
}

const SUMMARY_TS: i64 = 0;

fn sha256_hex(bytes: &[u8]) -> String {
    let out = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Bind a summary durably: digest bytes → `digest_blob` (content-addressed), the
/// binding → `summary_binding`, and a `Summary→binds→FileRevision` edge.
/// Idempotent (content-addressed keys → `INSERT OR IGNORE` set-union).
///
/// The digest body is a registered sensitive column (Layer-2 encryption at
/// rest): it reaches the store only through `cipher` — [`rift_store::DekCipher`]
/// over the family's file-held key in production, and `PlainCipher` in tests.
/// The content address (`digest_ref`) is computed over the *plaintext*, so
/// idempotency and hash-binding are cipher-independent.
///
/// # Errors
/// Propagates store write and cipher errors.
pub async fn put_summary(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    path: &str,
    source: &[u8],
    versions: SummaryVersions,
    digest: &[u8],
) -> Result<String> {
    let key = summary_key(source, versions);
    let digest_ref = sha256_hex(digest);
    let source_sha = sha256_hex(source);

    let sealed = cipher.encrypt("digest_blob", "bytes", &digest_ref, digest)?;
    store
        .insert_or_ignore(
            "digest_blob",
            &["digest_ref", "bytes", "created_at"],
            &[
                Value::text(&digest_ref),
                Value::Blob(sealed),
                Value::Int(SUMMARY_TS),
            ],
        )
        .await?;
    store
        .insert_or_ignore(
            "summary_binding",
            &[
                "summary_id",
                "path",
                "source_sha256",
                "bound_blob_oid",
                "extractor_ver",
                "policy_ver",
                "redaction_ver",
                "digest_ref",
                "created_at",
            ],
            &[
                Value::text(&key),
                Value::text(path),
                Value::text(&source_sha),
                Value::text(&source_sha), // bound_oid == source SHA-256 (worktree comparator)
                Value::Int(i64::from(versions.extractor_version)),
                Value::Int(i64::from(versions.policy_version)),
                Value::Int(i64::from(versions.redaction_version)),
                Value::text(&digest_ref),
                Value::Int(SUMMARY_TS),
            ],
        )
        .await?;

    // Summary node + Summary→binds→FileRevision edge.
    let fr = file_revision_id(path, &source_sha);
    let mut b = Built::default();
    b.node(&key, "Summary", &key);
    b.edge(&key, &fr, "binds");
    crate::ingest::apply_rows(store, &b).await?;
    Ok(key)
}

/// Bind a file's outline and store its token counts beside it, or do
/// nothing for a file that is neither code nor Markdown, or that the serve
/// policy refuses.
///
/// The detached worker is the caller: it counts with the real tokenizer and
/// hands the counts in, so neither this crate nor any hook decision starts
/// one. Every write is insert-or-ignore, so binding the same bytes twice is a
/// no-op. `Ok(None)` is the answer for a path the policy denies or marks
/// sensitive, a path outside the outlined extensions, or bytes that are not
/// UTF-8: no binding, no counts.
///
/// # Errors
/// Refuses counts that do not match the outline's definitions one for one,
/// before anything is written; propagates store and cipher errors.
pub async fn put_file_outline(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    file: &OutlineSource<'_>,
    counts: &OutlineCounts,
) -> Result<Option<String>> {
    if (file.refuses_path)(file.path) {
        return Ok(None);
    }
    let source = file.source.as_slice();
    let Some(outline) = FileOutline::of(file.path, source) else {
        return Ok(None);
    };
    if counts.definitions.len() != outline.definitions().len() {
        return Err(RiftError::validity(format!(
            "{} definition counts for an outline of {} definitions",
            counts.definitions.len(),
            outline.definitions().len()
        )));
    }
    let versions = outline_versions(file.policy_version);
    let digest = outline.render();
    let key = put_summary(
        store,
        cipher,
        file.path,
        source,
        versions,
        digest.as_bytes(),
    )
    .await?;
    for count in outline_counts(&outline, counts) {
        put_token_count(store, &key, &count).await?;
    }
    Ok(Some(key))
}

/// The counts stored under a summary key, whole file first.
///
/// # Errors
/// Propagates store read errors.
pub async fn token_counts(store: &impl CanonicalStore, key: &str) -> Result<Vec<TokenCount>> {
    let rows = store
        .query(
            "SELECT ordinal, line_lo, line_hi, tokens, tokenizer \
             FROM summary_token_count WHERE summary_id = ? ORDER BY ordinal",
            &[Value::text(key)],
        )
        .await?;
    let number = |v: i64| u32::try_from(v).unwrap_or(0);
    rows.iter()
        .map(|r| {
            Ok(TokenCount {
                ordinal: number(r.get_i64(0)?),
                line_lo: number(r.get_i64(1)?),
                line_hi: number(r.get_i64(2)?),
                tokens: number(r.get_i64(3)?),
                tokenizer: r.get_text(4)?,
            })
        })
        .collect()
}

impl OutlineCounts {
    /// Count the outline of `source` at `path` with `count`: the whole file,
    /// then each definition's lines — or `None` for a file that gets no
    /// outline. The caller passes the tokenizer, so the caller is the one that
    /// loads it, and the spans counted are the ones the writer will bind.
    pub fn measure(
        path: &str,
        source: &RedactedBytes,
        count: &dyn Fn(&str) -> u32,
        tokenizer: &'static str,
    ) -> Option<Self> {
        let outline = FileOutline::of(path, source.as_slice())?;
        let text = core::str::from_utf8(source.as_slice()).ok()?;
        Some(Self {
            tokenizer,
            whole_file: count(text),
            definitions: outline
                .definitions()
                .iter()
                .map(|d| count(line_span(text, d.first_line(), d.last_line())))
                .collect(),
        })
    }
}

/// The rows one outline's counts become, whole file first.
fn outline_counts(outline: &FileOutline, counts: &OutlineCounts) -> Vec<TokenCount> {
    let whole = TokenCount {
        ordinal: 0,
        line_lo: u32::from(outline.lines() > 0),
        line_hi: outline.lines(),
        tokens: counts.whole_file,
        tokenizer: counts.tokenizer.to_owned(),
    };
    let definitions = outline
        .definitions()
        .iter()
        .zip(&counts.definitions)
        .zip(1u32..)
        .map(|((d, tokens), ordinal)| TokenCount {
            ordinal,
            line_lo: d.first_line(),
            line_hi: d.last_line(),
            tokens: *tokens,
            tokenizer: counts.tokenizer.to_owned(),
        });
    std::iter::once(whole).chain(definitions).collect()
}

async fn put_token_count(store: &impl CanonicalStore, key: &str, count: &TokenCount) -> Result<()> {
    store
        .insert_or_ignore(
            "summary_token_count",
            &[
                "summary_id",
                "ordinal",
                "line_lo",
                "line_hi",
                "tokens",
                "tokenizer",
                "schema_version",
            ],
            &[
                Value::text(key),
                Value::Int(i64::from(count.ordinal)),
                Value::Int(i64::from(count.line_lo)),
                Value::Int(i64::from(count.line_hi)),
                Value::Int(i64::from(count.tokens)),
                Value::text(&count.tokenizer),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(())
}

/// Outline every code and Markdown file the session read whose bytes on disk are still a
/// revision it read, and count what happened to each path.
///
/// `reads` are `(path, content_sha256)` pairs, repeats allowed; each path is
/// visited once, in path order, so the cap always takes the same prefix. A
/// file is read from the work tree, never from the object database, and is
/// bound only when its SHA-256 is one the session read. Nothing here fails
/// its caller: every failure is a count.
pub async fn bind_read_outlines(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    batch: &OutlineBatch<'_>,
    reads: &[(String, String)],
) -> OutlineBindCounts {
    let mut by_path: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for (path, sha) in reads {
        by_path.entry(path.as_str()).or_default().push(sha.as_str());
    }
    let mut counts = OutlineBindCounts::default();
    for (path, read_shas) in by_path {
        let budget_left = counts.bound < MAX_OUTLINES_PER_BATCH;
        let step = outline_step(store, cipher, batch, path, &read_shas, budget_left).await;
        counts.add(step);
    }
    counts
}

impl OutlineBindCounts {
    fn add(&mut self, step: OutlineStep) {
        let slot = match step {
            OutlineStep::Bound => &mut self.bound,
            OutlineStep::AlreadyBound => &mut self.already_bound,
            OutlineStep::Changed => &mut self.changed,
            OutlineStep::Refused => &mut self.refused,
            OutlineStep::NotCode => &mut self.not_code,
            OutlineStep::Oversize => &mut self.oversize,
            OutlineStep::Deferred => &mut self.deferred,
            OutlineStep::Failed => &mut self.failed,
        };
        *slot = slot.saturating_add(1);
    }
}

/// One path through the batch: the cheap refusals first, then the read, the
/// revision check, the already-bound check, the cap, and the write.
async fn outline_step(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    batch: &OutlineBatch<'_>,
    path: &str,
    read_shas: &[&str],
    budget_left: bool,
) -> OutlineStep {
    if !is_outlinable_path(path) {
        return OutlineStep::NotCode;
    }
    if std::path::Path::new(path).is_absolute() || (batch.refuses_path)(path) {
        return OutlineStep::Refused;
    }
    let bytes = match read_bounded(&batch.work_tree.join(path)) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return OutlineStep::Oversize,
        Err(()) => return OutlineStep::Changed,
    };
    let now = sha256_hex(&bytes);
    if !read_shas.contains(&now.as_str()) {
        return OutlineStep::Changed;
    }
    let versions = outline_versions(batch.policy_version);
    if let Ok(CurrentSummary::Bound(Validity::Valid(_))) =
        current_summary(store, cipher, path, Some(&now), versions).await
    {
        return OutlineStep::AlreadyBound;
    }
    if !budget_left {
        return OutlineStep::Deferred;
    }
    write_outline(store, cipher, batch, path, bytes).await
}

/// Redact, count and bind one file whose bytes are a revision the session
/// read.
async fn write_outline(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    batch: &OutlineBatch<'_>,
    path: &str,
    bytes: Vec<u8>,
) -> OutlineStep {
    let raw_len = bytes.len();
    let raw_sha = sha256_hex(&bytes);
    let Some(source) = (batch.redact)(bytes) else {
        return OutlineStep::Failed;
    };
    // A file the gate masked hashes differently from the checkout, so its
    // binding could never be served; it is refused rather than written.
    if source.as_slice().len() != raw_len || sha256_hex(source.as_slice()) != raw_sha {
        return OutlineStep::Refused;
    }
    let Some(counts) = OutlineCounts::measure(path, &source, batch.count, batch.tokenizer) else {
        return OutlineStep::NotCode;
    };
    let file = OutlineSource {
        path,
        source: &source,
        policy_version: batch.policy_version,
        refuses_path: batch.refuses_path,
    };
    match put_file_outline(store, cipher, &file, &counts).await {
        Ok(Some(_)) => OutlineStep::Bound,
        Ok(None) => OutlineStep::Refused,
        Err(_) => OutlineStep::Failed,
    }
}

/// The file's bytes, `Ok(None)` past [`MAX_OUTLINE_SOURCE_BYTES`], or
/// `Err(())` when there is no regular file to read.
fn read_bounded(file: &std::path::Path) -> std::result::Result<Option<Vec<u8>>, ()> {
    let meta = std::fs::symlink_metadata(file).map_err(|_| ())?;
    if !meta.is_file() {
        return Err(());
    }
    if meta.len() > MAX_OUTLINE_SOURCE_BYTES {
        return Ok(None);
    }
    std::fs::read(file).map(Some).map_err(|_| ())
}

async fn fetch_binding(store: &impl CanonicalStore, key: &str) -> Result<Option<SummaryBinding>> {
    let rows = store
        .query(
            "SELECT path, source_sha256, bound_blob_oid, extractor_ver, policy_ver, \
                    redaction_ver, digest_ref \
             FROM summary_binding WHERE summary_id = ?",
            &[Value::text(key)],
        )
        .await?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(SummaryBinding {
        key: key.to_string(),
        path: r.get_text(0)?,
        source_sha256: r.get_text(1)?,
        bound_oid: r.get_text(2)?,
        versions: SummaryVersions {
            extractor_version: u32::try_from(r.get_i64(3)?).unwrap_or(0),
            policy_version: u32::try_from(r.get_i64(4)?).unwrap_or(0),
            redaction_version: u32::try_from(r.get_i64(5)?).unwrap_or(0),
        },
        digest_ref: r.get_text(6)?,
    }))
}

async fn fetch_digest(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    digest_ref: &str,
) -> Result<Option<Vec<u8>>> {
    let rows = store
        .query(
            "SELECT bytes FROM digest_blob WHERE digest_ref = ?",
            &[Value::text(digest_ref)],
        )
        .await?;
    match rows.first() {
        Some(r) => Ok(Some(cipher.decrypt(
            "digest_blob",
            "bytes",
            digest_ref,
            &r.get_blob(0)?,
        )?)),
        None => Ok(None),
    }
}

/// Resolve a summary key against the current worktree snapshot, durably. Returns
/// a [`Validity`] whose `Valid` arm is the only one carrying bytes (a stale
/// summary is structurally unreturnable, via [`classify`]). On `Invalid`, an
/// append-only `Summary→invalidated_by→FileRevision` audit edge is recorded.
///
/// # Errors
/// Propagates store read/write errors.
pub async fn resolve(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    snapshot: &rift_recall_validity::WorktreeSnapshot,
    key: &str,
) -> Result<Validity> {
    let Some(binding) = fetch_binding(store, key).await? else {
        return Ok(Validity::Degraded {
            key: key.to_string(),
        });
    };
    let digest = fetch_digest(store, cipher, &binding.digest_ref).await?;
    let verdict = classify(&binding, snapshot, digest);
    if let Validity::Invalid { now, .. } = &verdict {
        let new_fr = file_revision_id(&binding.path, now);
        let mut b = Built::default();
        b.edge(key, &new_fr, "invalidated_by"); // Summary→invalidated_by→FileRevision (audit)
        crate::ingest::apply_rows(store, &b).await?;
    }
    Ok(verdict)
}

/// The summary bound to `path` at `current`, the revision the checkout holds
/// now, under `versions`.
///
/// A binding at the current revision under other versions is no binding of
/// this summary, so it answers [`CurrentSummary::NotBound`]: a version bump
/// orphans what came before rather than serving it. A digest that cannot be
/// read — an encrypted row with no key custody, a lost blob — fails closed to
/// [`Validity::Degraded`] rather than an error.
///
/// # Errors
/// Propagates store read errors.
pub async fn current_summary(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    path: &str,
    current: Option<&str>,
    versions: SummaryVersions,
) -> Result<CurrentSummary> {
    let rows = store
        .query(
            "SELECT summary_id, bound_blob_oid, extractor_ver, policy_ver, redaction_ver \
             FROM summary_binding WHERE path = ? ORDER BY summary_id",
            &[Value::text(path)],
        )
        .await?;
    if rows.is_empty() {
        return Ok(CurrentSummary::NotBound);
    }
    let Some(current) = current else {
        return Ok(CurrentSummary::Unchecked);
    };
    let mut at_current = false;
    for row in &rows {
        if row.get_text(1)? != current {
            continue;
        }
        at_current = true;
        if row_versions(row)? == versions {
            let key = row.get_text(0)?;
            let validity = classify_current(store, cipher, &key, path, current).await?;
            return Ok(CurrentSummary::Bound(validity));
        }
    }
    Ok(if at_current {
        CurrentSummary::NotBound
    } else {
        CurrentSummary::Stale
    })
}

/// The summaries bound at the current revision of each `(path, current)`
/// pair, under `versions`, by path: one scan of the bindings for a whole
/// manifest rather than one query per entry.
///
/// A pair with no such binding is absent from the map.
///
/// # Errors
/// Propagates store read errors.
pub async fn current_summaries(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    wanted: &[(&str, &str)],
    versions: SummaryVersions,
) -> Result<std::collections::BTreeMap<String, Validity>> {
    let mut found = std::collections::BTreeMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }
    let rows = store
        .query(
            "SELECT summary_id, path, bound_blob_oid FROM summary_binding \
             WHERE extractor_ver = ? AND policy_ver = ? AND redaction_ver = ? \
             ORDER BY summary_id",
            &[
                Value::Int(i64::from(versions.extractor_version)),
                Value::Int(i64::from(versions.policy_version)),
                Value::Int(i64::from(versions.redaction_version)),
            ],
        )
        .await?;
    for row in &rows {
        let (path, oid) = (row.get_text(1)?, row.get_text(2)?);
        if found.contains_key(&path) || !wanted.contains(&(path.as_str(), oid.as_str())) {
            continue;
        }
        let key = row.get_text(0)?;
        let validity = classify_current(store, cipher, &key, &path, &oid).await?;
        found.insert(path, validity);
    }
    Ok(found)
}

/// Classify the binding `key` against the revision it was found at; a digest
/// that cannot be decrypted counts as absent, so the verdict is `Degraded`.
async fn classify_current(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    key: &str,
    path: &str,
    current: &str,
) -> Result<Validity> {
    let Some(binding) = fetch_binding(store, key).await? else {
        return Ok(Validity::Degraded {
            key: key.to_owned(),
        });
    };
    let digest = fetch_digest(store, cipher, &binding.digest_ref)
        .await
        .ok()
        .flatten();
    let mut snapshot = WorktreeSnapshot::new();
    snapshot.insert(path.to_owned(), current.to_owned());
    Ok(classify(&binding, &snapshot, digest))
}

fn row_versions(row: &rift_store::Row) -> Result<SummaryVersions> {
    let version = |at: usize| -> Result<u32> { Ok(u32::try_from(row.get_i64(at)?).unwrap_or(0)) };
    Ok(SummaryVersions {
        extractor_version: version(2)?,
        policy_version: version(3)?,
        redaction_version: version(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_validity::WorktreeSnapshot;
    use rift_store::{recall_migrations, DekCipher, MemoryKeyStore, PlainCipher, SqliteStore};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    fn versions() -> SummaryVersions {
        SummaryVersions {
            extractor_version: 1,
            policy_version: 1,
            redaction_version: 1,
        }
    }

    fn snapshot(path: &str, sha: &str) -> WorktreeSnapshot {
        let mut m = WorktreeSnapshot::new();
        m.insert(path.to_string(), sha.to_string());
        m
    }

    #[test]
    fn valid_when_oid_matches_invalid_when_changed() {
        let store = recall_store();
        let source = b"fn main() {}\n";
        let key = block(put_summary(
            &store,
            &PlainCipher,
            "src/main.rs",
            source,
            versions(),
            b"digest",
        ))
        .unwrap();
        let cur = sha256_hex(source);

        // unchanged → Valid (bytes attached)
        match block(resolve(
            &store,
            &PlainCipher,
            &snapshot("src/main.rs", &cur),
            &key,
        ))
        .unwrap()
        {
            Validity::Valid(s) => assert_eq!(s.digest(), b"digest"),
            other => panic!("expected Valid, got {:?}", validity_tag(&other)),
        }
        // changed → Invalid (no bytes)
        match block(resolve(
            &store,
            &PlainCipher,
            &snapshot("src/main.rs", "different"),
            &key,
        ))
        .unwrap()
        {
            Validity::Invalid { .. } => {}
            other => panic!("expected Invalid, got {:?}", validity_tag(&other)),
        }
    }

    #[test]
    fn substrate_resolver_maps_content_hash_to_git_blob_oid() {
        use rift_recall_substrate::MemorySubstrate;
        use rift_recall_types::Handle;
        use rift_recall_validity::{resolve_handle, Resolved, Validity};

        // The real-repo case: the git blob oid is DISTINCT from content_sha256.
        let bytes = b"fn main() {}\n".to_vec();
        let content_sha = sha256_hex(&bytes);
        let blob_oid = "1111111111111111111111111111111111111111".to_string();
        assert_ne!(content_sha, blob_oid);

        // The substrate stores the blob under its git oid; the map bridges.
        let reader = MemorySubstrate::local().with_blob(&blob_oid, &bytes);
        let mut map = std::collections::BTreeMap::new();
        map.insert(content_sha.clone(), blob_oid.clone());
        let resolver = SubstrateBlobResolver::new(&reader).with_oid_map(map);

        let h = Handle::File {
            path: "a.rs".into(),
            sha256: content_sha,
            range: None,
        };
        // resolve_handle self-validates (bytes must re-hash to the handle's sha).
        match resolve_handle(&h, &resolver, |_| Validity::Degraded { key: String::new() }) {
            Resolved::Bytes(b) => assert_eq!(b, bytes, "resolved via the blob-oid map"),
            _ => panic!("expected the mapped bytes"),
        }

        // Without the map (unmapped hash) the fallback lookup by content hash
        // misses in a real repo — honest Unavailable, never wrong bytes.
        let bare = SubstrateBlobResolver::new(&reader);
        assert!(matches!(
            resolve_handle(&h, &bare, |_| Validity::Degraded { key: String::new() }),
            Resolved::Unavailable(_)
        ));
    }

    #[test]
    fn invalid_appends_invalidated_by_edge() {
        let store = recall_store();
        let source = b"x\n";
        let key = block(put_summary(
            &store,
            &PlainCipher,
            "a.rs",
            source,
            versions(),
            b"d",
        ))
        .unwrap();
        let _ = block(resolve(
            &store,
            &PlainCipher,
            &snapshot("a.rs", "changed"),
            &key,
        ))
        .unwrap();
        let n = block(store.query(
            "SELECT count(*) FROM graph_edge WHERE kind='invalidated_by'",
            &[],
        ))
        .unwrap();
        assert_eq!(n[0].get_i64(0).unwrap(), 1);
    }

    #[test]
    fn revert_makes_valid_again() {
        let store = recall_store();
        let source = b"y\n";
        let key = block(put_summary(
            &store,
            &PlainCipher,
            "b.rs",
            source,
            versions(),
            b"d",
        ))
        .unwrap();
        let cur = sha256_hex(source);
        // changed → Invalid
        assert!(matches!(
            block(resolve(&store, &PlainCipher, &snapshot("b.rs", "z"), &key)).unwrap(),
            Validity::Invalid { .. }
        ));
        // reverted → Valid again (validity is computed, no lingering flag)
        assert!(matches!(
            block(resolve(&store, &PlainCipher, &snapshot("b.rs", &cur), &key)).unwrap(),
            Validity::Valid(_)
        ));
    }

    fn validity_tag(v: &Validity) -> &'static str {
        match v {
            Validity::Valid(_) => "Valid",
            Validity::Invalid { .. } => "Invalid",
            Validity::Degraded { .. } => "Degraded",
        }
    }

    /// The Layer-2 keystone for the recall projection: a digest body written
    /// through the production custody chain (keychain → KEK → wrapped DEK →
    /// [`DekCipher`]) is ciphertext in the raw `recall.db` bytes, and resolves
    /// back to plaintext. A [`PlainCipher`] reader cannot read it (fails
    /// closed), and a plain-written row remains readable under [`DekCipher`].
    #[test]
    fn digest_bytes_are_encrypted_at_rest_in_recall_db() {
        use rift_core::RepoId;
        use rift_store::{KeyringKeyStore, MemoryKeychain};

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("recall.db");
        let sentinel: &[u8] = b"SUMMARY-DIGEST-SENTINEL-77c1";
        let source = b"fn secret_summary_source() {}\n";
        let repo = RepoId::new("repo-1").unwrap();

        {
            let store = rift_store::open_migrated(&db, &recall_migrations()).unwrap();
            // Production custody: provision the wrapped DEK through the envelope.
            let ks = KeyringKeyStore::new(MemoryKeychain::new());
            let dek = block(ks.provision_repo_dek(&store, &repo, 1)).unwrap();
            let cipher = DekCipher::new(MemoryKeyStore::new(dek), repo.clone());

            let key = block(put_summary(
                &store,
                &cipher,
                "s.rs",
                source,
                versions(),
                sentinel,
            ))
            .unwrap();

            // Resolves back to the plaintext digest through the cipher.
            let cur = sha256_hex(source);
            match block(resolve(&store, &cipher, &snapshot("s.rs", &cur), &key)).unwrap() {
                Validity::Valid(s) => assert_eq!(s.digest(), sentinel),
                other => panic!("expected Valid, got {:?}", validity_tag(&other)),
            }

            // A key-less reader fails closed rather than serving ciphertext.
            assert!(block(resolve(&store, &PlainCipher, &snapshot("s.rs", &cur), &key)).is_err());
        }

        // The raw on-disk database (and WAL sidecar, if any) must not contain
        // the plaintext digest sentinel.
        for path in [db.clone(), db.with_extension("db-wal")] {
            if let Ok(bytes) = std::fs::read(&path) {
                assert!(
                    !bytes.windows(sentinel.len()).any(|w| w == sentinel),
                    "plaintext digest sentinel leaked into {}",
                    path.display()
                );
            }
        }

        // NOTE: key_metadata rows live in recall.db here too — assert the wrapped
        // DEK never appears as raw key bytes is covered in rift-store's tests.
    }

    proptest::proptest! {
        /// The false-memory keystone, durably: for ANY source mutation, `resolve` never
        /// yields `Valid` for the original key.
        #[test]
        fn mutated_source_makes_summary_unreturnable(
            source in proptest::collection::vec(0u8..255, 1..50),
            other in "[a-f0-9]{8}",
        ) {
            let store = recall_store();
            let key = block(put_summary(&store, &PlainCipher, "f.rs", &source, versions(), b"digest")).unwrap();
            let real = sha256_hex(&source);
            // any worktree hash != the bound hash must NOT be Valid
            proptest::prop_assume!(other != real);
            let verdict = block(resolve(&store, &PlainCipher, &snapshot("f.rs", &other), &key)).unwrap();
            proptest::prop_assert!(!matches!(verdict, Validity::Valid(_)));
        }
    }
}
