//! `rift bundle export|import` — the offline transfer unit.
//!
//! A bundle is one file carrying three parts: a **manifest** (what this is and
//! what it claims), a **chain segment** (the tamper-evidence, `rift-store`'s
//! `rift-chain-segment/1`), and a **git bundle** (the objects and refs). It is
//! the air-gapped transfer story today and the rehearsal for a streaming wire
//! format later.
//!
//! # Why there is a container rather than just a git bundle
//!
//! A git bundle can carry only git objects, so the manifest and segment would
//! have to ride *inside* it as an object — and reading an object means
//! unbundling first. That inverts the ordering `docs/sync-format.md` states as
//! a constraint: **verify before trust, and the cheap offline verify comes
//! first**. With a container, every claim a bundle makes is checkable from
//! plain bytes before a single git object is written; the git side is only
//! opened once the segment has verified and the digests match.
//!
//! # The container
//!
//! ```text
//! rift-bundle/1\n
//! manifest <len>\n<len bytes>\n
//! segment <len>\n<len bytes>\n
//! gitbundle <len>\n<len bytes>\n
//! ```
//!
//! Fixed part order, explicit byte lengths (the git-bundle part is binary), one
//! `\n` after each part's bytes. Every integer in the frame is **canonical** —
//! ASCII digits, no leading zero unless the value is zero, no sign — decided by
//! [`rift_store::parse_canonical_u64`], the same one writer the stream cursor
//! and the chain segment call. An unknown header version, a non-canonical
//! integer, an unknown part name, a missing part, a wrong length or a part out
//! of order is a **refusal** — never something to skip past, because everything
//! after a mis-framed part is unaligned garbage that must not be interpreted.
//!
//! # What export bundles, and why it is always whole
//!
//! `--since <cursor>` scopes the **segment** — the window being attested. The
//! git side always carries the refs' full history, because a bundle with
//! prerequisites cannot be applied to a repository that does not already have
//! them, and "import into a fresh clone" is the case this format exists for.
//! Self-contained is a requirement here, not an optimization to trade away.
//!
//! # Import is all-or-nothing
//!
//! Verification order: frame → manifest → part digests → segment (pure, offline)
//! → segment-vs-manifest agreement → git bundle integrity → objects present →
//! every ref a fast-forward. **Only then** do refs move, and they move in one
//! `git update-ref --stdin` transaction, so a failure at any point leaves every
//! ref exactly where it was. Applying is set-union: a ref already at or ahead
//! of the bundled target is a no-op, so re-importing the same bundle converges.

use std::collections::BTreeSet;
use std::path::Path;

use rift_core::{Result, RiftError};
use rift_store::{ChainSegment, StreamCursor};

use crate::cli::{exit, Outcome};

/// Container format version.
pub(crate) const BUNDLE_SCHEMA_VERSION: u32 = 1;
/// The container's first line, without the version.
const CONTAINER_PREFIX: &str = "rift-bundle/";
/// The three parts, in the only order a bundle may carry them.
const PART_NAMES: [&str; 3] = ["manifest", "segment", "gitbundle"];
/// A refusal rather than an allocation: no legitimate bundle part is this big,
/// and a corrupt length header must not be believed far enough to matter.
const MAX_PART_BYTES: usize = 512 * 1024 * 1024;

/// The checkpoint branch and the chain ref — the two refs a bundle carries.
const BUNDLED_REFS: [&str; 2] = [
    rift_shadow::backend::NATIVE_CHECKPOINT_REF,
    rift_git::chain::CHAIN_REF,
];

/// The all-zero oid: `update-ref`'s spelling of "this ref must not exist".
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// One ref carried by a bundle.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundledRef {
    /// The fully-qualified ref name.
    pub name: String,
    /// The oid it points at inside this bundle.
    pub target: String,
}

/// What a bundle claims about itself.
///
/// Field order here **is** the serialization order: the manifest is written
/// compact with `serde_json`, so identical inputs produce identical bytes and a
/// bundle regenerated from the same window diffs empty.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundleManifest {
    /// Contract version. A reader accepts `1..=current` and refuses anything
    /// else WHOLE.
    pub schema_version: u32,
    /// The exporting repository's id — provenance, never a trust decision.
    pub repo_id: String,
    /// The window's lower bound (exclusive).
    pub from_cursor: String,
    /// The window's upper bound (inclusive).
    pub to_cursor: String,
    /// Every ref in the git-bundle part, with the oid it points at.
    pub ref_list: Vec<BundledRef>,
    /// The checkpoint id at `to_cursor` — empty ONLY when `to_cursor` is the
    /// origin. **From the blind review of this format:** an empty window at a
    /// NON-origin cursor (`--since <head>`, the head unchanged) still reports
    /// the real checkpoint id sitting there; it is not the segment's own
    /// "last link id", which would be empty for zero links regardless of
    /// cursor. `an_empty_window_at_a_non_origin_cursor_reports_the_real_head`
    /// pins both directions of this — the shape the doc self-contradicted on
    /// (`docs/sync-format.md`'s field table vs. its verification step 5)
    /// before the document was reconciled onto this reading, the one
    /// this field already implements.
    pub chain_head: String,
    /// `SHA-256` of the segment part's bytes, lowercase hex.
    pub segment_sha256: String,
    /// `SHA-256` of the git-bundle part's bytes, lowercase hex.
    pub gitbundle_sha256: String,
}

/// Lowercase-hex SHA-256 of `bytes`.
///
/// Plain SHA-256 with no domain prefix, deliberately: these digests bind a
/// container's own parts and must be reproducible by any implementation from
/// the specification alone, with nothing to know about Rift's Merkle domains.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Encode the container.
fn encode_container(manifest: &[u8], segment: &[u8], gitbundle: &[u8]) -> Vec<u8> {
    let mut out = format!("{CONTAINER_PREFIX}{BUNDLE_SCHEMA_VERSION}\n").into_bytes();
    for (name, bytes) in PART_NAMES.iter().zip([manifest, segment, gitbundle]) {
        out.extend_from_slice(format!("{name} {}\n", bytes.len()).as_bytes());
        out.extend_from_slice(bytes);
        out.push(b'\n');
    }
    out
}

/// Decode the container into its three parts, in order.
///
/// Total: every malformed input is an `Err`, never a panic and never a partial
/// read that a caller might mistake for a bundle.
///
/// **Every integer in the frame is canonical**, decided by the *one* writer of
/// that rule ([`rift_store::parse_canonical_u64`]) rather than re-derived here.
/// `rift-bundle/01`, `rift-bundle/+1`, `manifest 02` and `manifest +2` are
/// REFUSALS, not second spellings: a frame whose integers have more than one
/// encoding is a frame that cannot be byte-compared or digest-bound to itself.
/// This function accepted all four until the rule became callable — which is
/// the argument for calling it instead of writing `.parse()` again.
///
/// **No refusal here echoes a byte of the input**, part names included. A part
/// name is attacker-chosen UTF-8 up to the first newline: it can carry CSI/OSC
/// sequences that rewrite the terminal of whoever ran `rift bundle import`. The
/// position and the expected name — both compile-time constants — say
/// everything a legitimate operator needs.
fn decode_container(bytes: &[u8]) -> Result<[Vec<u8>; 3]> {
    let mut pos = 0usize;
    let header = read_line(bytes, &mut pos)?;
    let version = header
        .strip_prefix(CONTAINER_PREFIX)
        .ok_or_else(|| RiftError::validity("bundle: not a rift bundle (bad header)"))?;
    let version = rift_store::parse_canonical_u64(version)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| {
            RiftError::validity("bundle: container version is not a canonical base-10 integer")
        })?;
    if version == 0 || version > BUNDLE_SCHEMA_VERSION {
        return Err(RiftError::validity(format!(
            "bundle: unsupported container version {version} (this build reads \
             1..={BUNDLE_SCHEMA_VERSION})"
        )));
    }
    let mut parts: Vec<Vec<u8>> = Vec::with_capacity(3);
    for (index, expected) in PART_NAMES.into_iter().enumerate() {
        let position = index + 1;
        let line = read_line(bytes, &mut pos)?;
        let (name, len) = line
            .split_once(' ')
            .ok_or_else(|| RiftError::validity("bundle: malformed part header"))?;
        if name != expected {
            return Err(RiftError::validity(format!(
                "bundle: part {position} is not '{expected}' (part order is fixed)"
            )));
        }
        let len = rift_store::parse_canonical_u64(len)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| {
                RiftError::validity("bundle: part length is not a canonical base-10 integer")
            })?;
        if len > MAX_PART_BYTES {
            return Err(RiftError::validity("bundle: part length is implausible"));
        }
        let end = pos
            .checked_add(len)
            .ok_or_else(|| RiftError::validity("bundle: part length overflows"))?;
        let body = bytes
            .get(pos..end)
            .ok_or_else(|| RiftError::validity(format!("bundle: part '{expected}' is truncated")))?
            .to_vec();
        pos = end;
        if bytes.get(pos) != Some(&b'\n') {
            return Err(RiftError::validity(format!(
                "bundle: part '{expected}' is not terminated where its length says it ends"
            )));
        }
        pos += 1;
        parts.push(body);
    }
    if pos != bytes.len() {
        return Err(RiftError::validity(
            "bundle: trailing bytes after the last part",
        ));
    }
    let [m, s, g] = <[Vec<u8>; 3]>::try_from(parts)
        .map_err(|_| RiftError::validity("bundle: wrong part count"))?;
    Ok([m, s, g])
}

/// Whether `name` is a legal, fully-qualified git ref name.
///
/// **From the blind review of this format.** Step 6 of the transfer-bundle entry
/// promises "every `ref_list` name is a legal ref", but before this function
/// existed nothing checked it: [`rift_git::validate_refspec`] is an
/// argv-safety screen only (no leading `-`, no shell metacharacters, no
/// whitespace/NUL), and anything past it was hand to git's `update-ref`,
/// which happily creates a ref anywhere under `.git/`. Demonstrated: a bundle
/// naming `not/a/ref` reported "1 ref(s) applied" and created
/// `.git/not/a/ref` — a ref outside `refs/` fully.
///
/// This replicates `git check-ref-format`'s rule set in pure Rust — no git
/// invocation, so it stays inside the offline steps 1-6 — narrowed to
/// FULLY-QUALIFIED names only (the field table already says `ref_list`
/// entries are "fully-qualified"; `--allow-onelevel` is never legal here):
/// starts with `refs/`; no empty path component; no component starting with
/// `.` or ending in `.lock`; no `..` or `@{` anywhere; none of the characters
/// git's own format refuses (ASCII control, space, `~ ^ : ? * [ \`); does not
/// end in `/` or `.`.
// Not a filesystem extension check (clippy's suggested Path-based
// case-insensitive form would be wrong here): git's own rule is exactly this
// literal, case-sensitive suffix on a ref path component.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_legal_ref_name(name: &str) -> bool {
    if !name.starts_with("refs/") || name.ends_with('/') || name.ends_with('.') {
        return false;
    }
    if name.contains("..") || name.contains("@{") {
        return false;
    }
    if name.bytes().any(|b| {
        b.is_ascii_control() || matches!(b, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    }) {
        return false;
    }
    name.split('/')
        .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// Read one `\n`-terminated ASCII line, advancing `pos` past it.
fn read_line(bytes: &[u8], pos: &mut usize) -> Result<String> {
    let rest = bytes
        .get(*pos..)
        .ok_or_else(|| RiftError::validity("bundle: truncated"))?;
    let nl = rest
        .iter()
        .position(|b| *b == b'\n')
        .ok_or_else(|| RiftError::validity("bundle: truncated (no newline)"))?;
    let line = rest
        .get(..nl)
        .ok_or_else(|| RiftError::validity("bundle: truncated line"))?;
    let text = std::str::from_utf8(line)
        .map_err(|_| RiftError::validity("bundle: header line is not UTF-8"))?
        .to_owned();
    *pos += nl + 1;
    Ok(text)
}

/// `rift bundle export --since <cursor> [--out FILE]`.
///
/// # Errors
/// [`RiftError`] when the repository or projection cannot be read, when the
/// cursor does not name a position this chain holds, or when git fails.
pub(crate) fn cmd_export(
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    root: &Path,
    since: Option<&str>,
    out: Option<&Path>,
) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    let from = match since {
        Some(text) => StreamCursor::parse(text)?,
        None => StreamCursor::origin(),
    };
    let segment = rt.block_on(rift_store::export_segment(store, &from))?;

    let ref_list = present_refs(&repo)?;
    if ref_list.is_empty() {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "bundle export: this repository has no checkpoint or chain refs to export",
            serde_json::json!({ "error": "no_refs" }),
        ));
    }

    let path = out.map_or_else(|| root.join("rift-bundle.riftbundle"), Path::to_path_buf);
    let gitbundle = create_git_bundle(&repo, &path, &ref_list)?;

    let segment_bytes = segment.encode().into_bytes();
    let manifest = BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        repo_id: rift_git::repo_id_for(&repo.work_tree)?.as_str().to_owned(),
        from_cursor: segment.from_cursor().to_string(),
        to_cursor: segment.to_cursor().to_string(),
        ref_list,
        chain_head: segment.to_cursor().checkpoint_id().to_owned(),
        segment_sha256: sha256_hex(&segment_bytes),
        gitbundle_sha256: sha256_hex(&gitbundle),
    };
    let manifest_bytes =
        serde_json::to_vec(&manifest).map_err(|e| RiftError::Serde(e.to_string()))?;
    std::fs::write(
        &path,
        encode_container(&manifest_bytes, &segment_bytes, &gitbundle),
    )?;
    Ok(exported_outcome(&manifest, segment.links().len(), &path))
}

/// Which of the two bundled refs this repository actually has.
///
/// A repository with no chain ref yet exports a bundle carrying only the
/// checkpoint branch, which is honest rather than an error.
fn present_refs(repo: &rift_git::RepoPaths) -> Result<Vec<BundledRef>> {
    let mut ref_list: Vec<BundledRef> = Vec::new();
    for name in BUNDLED_REFS {
        if let Some(oid) = rift_git::resolve_commit(repo, name)? {
            ref_list.push(BundledRef {
                name: name.to_owned(),
                target: oid,
            });
        }
    }
    Ok(ref_list)
}

/// Have git write the git-bundle part for `ref_list`, and read its bytes back.
///
/// Git writes to a scratch file beside the container's destination, which is
/// removed before and after, so a stale scratch file from an interrupted run
/// never becomes a part.
fn create_git_bundle(
    repo: &rift_git::RepoPaths,
    path: &Path,
    ref_list: &[BundledRef],
) -> Result<Vec<u8>> {
    let tmp = path.with_extension("gitbundle.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut args = vec![
        "bundle".to_owned(),
        "create".to_owned(),
        "--quiet".to_owned(),
        tmp.display().to_string(),
    ];
    for r in ref_list {
        args.push(r.name.clone());
    }
    let (ok, _) = rift_git::run_git_status(repo, &args, &[], &[])?;
    if !ok {
        return Err(RiftError::storage(
            "bundle export: git could not write a bundle",
        ));
    }
    let gitbundle = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(gitbundle)
}

/// The human line and the JSON document for one written bundle.
fn exported_outcome(manifest: &BundleManifest, links: usize, path: &Path) -> Outcome {
    Outcome::ok(
        format!(
            "bundle export: {} -> {} ({} ref(s), {} link(s)) -> {}",
            manifest.from_cursor,
            manifest.to_cursor,
            manifest.ref_list.len(),
            links,
            path.display()
        ),
        serde_json::json!({
            "out": path.display().to_string(),
            "schema_version": BUNDLE_SCHEMA_VERSION,
            "from_cursor": manifest.from_cursor,
            "to_cursor": manifest.to_cursor,
            "chain_head": manifest.chain_head,
            "refs": manifest.ref_list.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            "links": links,
        }),
    )
}

/// Everything a bundle claims, checked without touching the object store.
#[derive(Debug)]
struct Verified {
    manifest: BundleManifest,
    gitbundle: Vec<u8>,
    /// The verified segment itself — `cmd_import` still has to ask whether it
    /// attaches to THIS repository's chain (step 6b), which the offline half
    /// cannot know.
    segment: ChainSegment,
}

/// One ref move: the ref, the oid it moves to, and the oid it must be at now
/// ([`ZERO_OID`] when it must not exist yet).
struct RefUpdate {
    name: String,
    new: String,
    old: String,
}

/// What applying a verified bundle will do to this repository's refs.
struct UpdatePlan {
    /// The refs that move, every one a fast-forward or a creation.
    updates: Vec<RefUpdate>,
    /// The refs already at the bundled target.
    already: usize,
}

/// The offline half of import: frame, digests, segment, and agreement between
/// the segment and the manifest. No git, no store, no clock, no network.
fn verify_offline(bytes: &[u8]) -> Result<Verified> {
    let [manifest_bytes, segment_bytes, gitbundle] = decode_container(bytes)?;
    // The reason, never the input — the same rule the frame parses under, and
    // it binds here for a sharper reason than symmetry. `BundleManifest` is
    // `deny_unknown_fields`, so a `serde_json` error over a hostile manifest
    // QUOTES THE REJECTED FIELD NAME ("unknown field `…`") and a wrong-typed
    // field quotes its value. Both are attacker-chosen bytes, and the operator
    // reading them is running `rift bundle import` in a terminal that obeys
    // CSI/OSC. A parse diagnostic is a debugging convenience; painting an
    // importer's terminal is not a debugging convenience.
    let manifest: BundleManifest = serde_json::from_slice(&manifest_bytes).map_err(|_| {
        RiftError::validity(
            "bundle: manifest is not readable (bad JSON, an unknown field, or a wrong field type)",
        )
    })?;
    if manifest.schema_version == 0 || manifest.schema_version > BUNDLE_SCHEMA_VERSION {
        return Err(RiftError::validity(format!(
            "bundle: unsupported manifest schema_version {} (this build reads \
             1..={BUNDLE_SCHEMA_VERSION})",
            manifest.schema_version
        )));
    }
    if sha256_hex(&segment_bytes) != manifest.segment_sha256 {
        return Err(RiftError::Integrity(
            "bundle: the chain segment does not match the digest its manifest claims".to_owned(),
        ));
    }
    if sha256_hex(&gitbundle) != manifest.gitbundle_sha256 {
        return Err(RiftError::Integrity(
            "bundle: the git bundle does not match the digest its manifest claims".to_owned(),
        ));
    }
    let text = std::str::from_utf8(&segment_bytes)
        .map_err(|_| RiftError::validity("bundle: segment is not UTF-8"))?;
    let segment = ChainSegment::decode(text)?;
    segment.verify()?;
    // The manifest must say what the segment says. Two texts describing one
    // window is exactly where a second implementation drifts, so they are
    // cross-checked rather than one being trusted.
    if segment.from_cursor().to_string() != manifest.from_cursor
        || segment.to_cursor().to_string() != manifest.to_cursor
    {
        return Err(RiftError::Integrity(
            "bundle: the manifest's window does not match the segment's".to_owned(),
        ));
    }
    if segment.to_cursor().checkpoint_id() != manifest.chain_head {
        // From the blind review of this format: this message used to say "is not the
        // segment's last link", which is a DIFFERENT rule (empty ⇒ "" always)
        // from the one actually checked here — the checkpoint id AT
        // to_cursor, empty only at the origin. For a non-empty segment the
        // two coincide (the last link's id IS the id at to_cursor, by segment
        // rule 4), which is exactly why the wrong wording went unnoticed: no
        // fixture in this tree exercised an empty window at a non-origin
        // cursor, the one case where they diverge.
        return Err(RiftError::Integrity(
            "bundle: the manifest's chain_head is not the checkpoint id at the segment's \
             to_cursor"
                .to_owned(),
        ));
    }
    for r in &manifest.ref_list {
        rift_git::validate_refspec(&r.name)?;
        if !is_legal_ref_name(&r.name) {
            return Err(RiftError::validity(
                "bundle: a ref_list name is not a fully-qualified, legal git ref",
            ));
        }
        rift_git::validate_oid(&r.target)?;
    }
    Ok(Verified {
        manifest,
        gitbundle,
        segment,
    })
}

/// `rift bundle import FILE`.
///
/// **Refs only, deliberately (a blind review of this format found both): this does
/// not open the projection or call [`rift_store::import_segment`].** A fresh
/// clone with no `.rift/` at all is the case a bundle exists to reach, and
/// `rift rebuild` — which re-derives every checkpoint's real strategy from
/// its own `metadata.json` via the substrate walk — is what projects what a
/// bundle brought in (`bundle_e2e.rs`'s round-trip test drives exactly that
/// sequence: import, then rebuild, then `rift verify` green). Measured, not
/// assumed: `import_segment` is this crate's correct, tested reference
/// implementation of the chain-segment attach rule, but a [`ChainSegment`]
/// carries no per-link strategy, `record_checkpoint_if_new` is
/// insert-or-ignore, and canonical tables have no `UPDATE` — so a bundle-side
/// writer landing a placeholder strategy first would permanently shadow the
/// substrate walk's real one. **This function's OWN refs-only behavior is
/// pinned, not just the mechanism** —
/// `bundle_e2e.rs`'s
/// `a_bundle_round_trips_into_a_fresh_repository_and_re_imports_idempotently`
/// reads the imported checkpoint's `strategy` back through the real binary
/// after `rebuild` and asserts it is the real, metadata-derived value; a
/// verifier caught that the crate-level
/// `rift_store::bundle::tests::a_second_writer_with_the_true_strategy_is_silently_dropped_after_the_first`
/// (which measures the underlying mechanism) never calls this function and so
/// cannot see whether THIS function is the one left unwired — the e2e
/// assertion closes that gap, and was verified BY MUTATION to red under the
/// literal "call `import_segment` from `cmd_import`" fix (the fixing
/// commit's message quotes the panic). The chain segment's own tamper-evidence
/// (steps 1-6, all pure) is still fully checked here, offline, before any
/// object or ref moves; only the *durable projection* of what it attests is
/// left to `rebuild`, which has the real data to write.
///
/// **Step 6b — the segment attaches to this repository's chain.**
/// Refs-only does not mean attach-blind: the chain ref's checkpoint ids
/// (oldest first, [`rift_git::chain::read_order`]) are the local chain, and
/// [`rift_store::attaches_by`] — the SAME predicate `import_segment` runs
/// against the projection — refuses an incremental window into a repository
/// with no chain, a `from_cursor` the chain does not hold, and an overlap
/// that names a different chain. It runs after step 6 and before any object
/// is written, so a refusal leaves the object store untouched
/// (`an_incremental_bundle_does_not_bootstrap_an_empty_repository` proves it
/// with a git-bundle part that is not even a bundle).
///
/// # Errors
/// [`RiftError`] on any verification failure — with nothing applied — or when
/// git fails.
pub(crate) fn cmd_import(root: &Path, file: &Path) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    let bytes = std::fs::read(file)?;
    let verified = verify_offline(&bytes)?;

    // 6b. The segment must attach to THIS repository's chain — the one attach
    // rule (`rift_store::attaches_by`), shared with the projection's
    // `import_segment` rather than re-implemented here. The chain ref's
    // checkpoint ids, oldest first (empty when the ref is absent), are the
    // local chain: an incremental window cannot bootstrap a repository with
    // no chain, `from_cursor` must name a position the chain holds, and where
    // the two overlap they must be the same chain. Before this check an
    // incremental bundle imported cleanly into an empty repository
    // (found by a blind review) because nothing on the ref side asked. Still
    // refs-only — the projection is not opened — and it precedes the tmp
    // write below, so a refusal never writes an object.
    let local = rift_git::chain::read_order(&repo.git_dir)?;
    rift_store::attaches_by(&local, &verified.segment, Clone::clone)?;

    // Only now may objects be written. They are inert until a ref names them:
    // `unbundle` indexes the pack and prints the bundle's refs, it does not
    // move any local ref.
    let tmp = repo.git_dir.join("rift-import.gitbundle.tmp");
    std::fs::write(&tmp, &verified.gitbundle)?;
    verify_git_bundle(&repo, &tmp)?;
    check_heads(&repo, &tmp, &verified.manifest.ref_list)?;
    unbundle(&repo, &tmp)?;

    let plan = plan_updates(&repo, &verified.manifest.ref_list)?;
    apply_updates(&repo, &plan.updates)?;
    Ok(imported_outcome(&verified, &plan))
}

/// Ask git whether the written git-bundle part is whole; the scratch file is
/// removed on a refusal.
fn verify_git_bundle(repo: &rift_git::RepoPaths, tmp: &Path) -> Result<()> {
    let verify_args = vec![
        "bundle".to_owned(),
        "verify".to_owned(),
        "--quiet".to_owned(),
        tmp.display().to_string(),
    ];
    let (ok, _) = rift_git::run_git_status(repo, &verify_args, &[], &[])?;
    if !ok {
        let _ = std::fs::remove_file(tmp);
        return Err(RiftError::Integrity(
            "bundle: git refused the bundle as damaged or incomplete".to_owned(),
        ));
    }
    Ok(())
}

/// The manifest's `ref_list` must be exactly the heads the git bundle carries.
///
/// From the blind review of this format: the field table asserts `ref_list` IS
/// "every ref the gitbundle part carries", but nothing checked that claim
/// before this — steps 1-6 read `ref_list` from the manifest's own JSON,
/// and steps 8-9 only ever WALK the declared list, never cross-check
/// it against what the bundle itself contains. Demonstrated: an empty
/// `ref_list` accompanying a git-bundle part that actually carries two
/// heads was ACCEPTED. `git bundle list-heads` reads the same plaintext
/// header `bundle verify` just accepted; comparing its output to
/// `ref_list` as a set costs one more git invocation and closes the gap
/// between what the manifest claims and what the bundle carries.
fn check_heads(repo: &rift_git::RepoPaths, tmp: &Path, ref_list: &[BundledRef]) -> Result<()> {
    let heads_args = vec![
        "bundle".to_owned(),
        "list-heads".to_owned(),
        tmp.display().to_string(),
    ];
    let (ok, out) = rift_git::run_git_status(repo, &heads_args, &[], &[])?;
    if !ok {
        let _ = std::fs::remove_file(tmp);
        return Err(RiftError::Integrity(
            "bundle: git could not list the git bundle's heads".to_owned(),
        ));
    }
    let actual_heads = parse_heads(&out)?;
    let claimed_heads: BTreeSet<(String, String)> = ref_list
        .iter()
        .map(|r| (r.name.clone(), r.target.clone()))
        .collect();
    if actual_heads != claimed_heads {
        let _ = std::fs::remove_file(tmp);
        return Err(RiftError::Integrity(
            "bundle: the manifest's ref_list does not match the heads the git bundle \
             actually carries"
                .to_owned(),
        ));
    }
    Ok(())
}

/// `git bundle list-heads` output as a set of `(name, oid)` pairs.
fn parse_heads(out: &[u8]) -> Result<BTreeSet<(String, String)>> {
    let text = std::str::from_utf8(out)
        .map_err(|_| RiftError::Integrity("bundle: git bundle heads are not UTF-8".to_owned()))?;
    let mut actual_heads = BTreeSet::new();
    for line in text.lines() {
        let (oid, name) = line.split_once(' ').ok_or_else(|| {
            RiftError::Integrity(
                "bundle: a git bundle head line has an unexpected shape".to_owned(),
            )
        })?;
        actual_heads.insert((name.to_owned(), oid.to_owned()));
    }
    Ok(actual_heads)
}

/// Write the bundle's objects into this repository; the scratch file is
/// removed whether git succeeds or not.
fn unbundle(repo: &rift_git::RepoPaths, tmp: &Path) -> Result<()> {
    let unbundle_args = vec![
        "bundle".to_owned(),
        "unbundle".to_owned(),
        tmp.display().to_string(),
    ];
    let (ok, _) = rift_git::run_git_status(repo, &unbundle_args, &[], &[])?;
    let _ = std::fs::remove_file(tmp);
    if !ok {
        return Err(RiftError::Integrity(
            "bundle: git could not read the bundle's objects".to_owned(),
        ));
    }
    Ok(())
}

/// Every ref must be a fast-forward before ANY of them moves.
///
/// A bundle whose history diverges from this repository's is refused whole:
/// these refs are append-only, so a non-fast-forward is a disagreement about
/// what happened, not an update.
fn plan_updates(repo: &rift_git::RepoPaths, ref_list: &[BundledRef]) -> Result<UpdatePlan> {
    let mut plan = UpdatePlan {
        updates: Vec::new(),
        already: 0,
    };
    for r in ref_list {
        let (present, _) = rift_git::run_git_status(
            repo,
            &["cat-file".to_owned(), "-e".to_owned(), r.target.clone()],
            &[],
            &[],
        )?;
        if !present {
            return Err(RiftError::Integrity(format!(
                "bundle: the bundle names {} but does not carry its objects",
                r.name
            )));
        }
        let old = match rift_git::resolve_commit(repo, &r.name)? {
            None => ZERO_OID.to_owned(),
            Some(cur) if cur == r.target => {
                plan.already += 1;
                continue;
            }
            Some(cur) => fast_forward_from(repo, r, cur)?,
        };
        plan.updates.push(RefUpdate {
            name: r.name.clone(),
            new: r.target.clone(),
            old,
        });
    }
    Ok(plan)
}

/// `cur`, when the bundled target descends from it; a refusal otherwise.
fn fast_forward_from(repo: &rift_git::RepoPaths, r: &BundledRef, cur: String) -> Result<String> {
    let (ff, _) = rift_git::run_git_status(
        repo,
        &[
            "merge-base".to_owned(),
            "--is-ancestor".to_owned(),
            cur.clone(),
            r.target.clone(),
        ],
        &[],
        &[],
    )?;
    if !ff {
        return Err(RiftError::Integrity(format!(
            "bundle: {} would not fast-forward — the bundle's history and this \
             repository's disagree, so nothing was applied",
            r.name
        )));
    }
    Ok(cur)
}

/// Move every planned ref in one transaction: `update-ref --stdin` applies
/// every line or none, so a concurrent writer cannot leave half a bundle
/// applied.
fn apply_updates(repo: &rift_git::RepoPaths, updates: &[RefUpdate]) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    let mut stdin = String::new();
    for RefUpdate { name, new, old } in updates {
        stdin.push_str(&format!("update {name} {new} {old}\n"));
    }
    let (ok, _) = rift_git::run_git_status(
        repo,
        &["update-ref".to_owned(), "--stdin".to_owned()],
        stdin.as_bytes(),
        &[],
    )?;
    if !ok {
        return Err(RiftError::Integrity(
            "bundle: the ref transaction was refused; nothing was applied".to_owned(),
        ));
    }
    Ok(())
}

/// The human line and the JSON document for one applied bundle.
fn imported_outcome(verified: &Verified, plan: &UpdatePlan) -> Outcome {
    Outcome::ok(
        format!(
            "bundle import: {} ref(s) applied, {} already present ({} link(s) verified, \
             window {} -> {})",
            plan.updates.len(),
            plan.already,
            verified.segment.links().len(),
            verified.manifest.from_cursor,
            verified.manifest.to_cursor
        ),
        serde_json::json!({
            "applied": plan.updates.iter().map(|update| update.name.clone()).collect::<Vec<_>>(),
            "already_present": plan.already,
            "links": verified.segment.links().len(),
            "from_cursor": verified.manifest.from_cursor,
            "to_cursor": verified.manifest.to_cursor,
            "chain_head": verified.manifest.chain_head,
            "repo_id": verified.manifest.repo_id,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> BundleManifest {
        BundleManifest {
            schema_version: 1,
            repo_id: "r".to_owned(),
            from_cursor: StreamCursor::origin().to_string(),
            to_cursor: StreamCursor::origin().to_string(),
            ref_list: vec![BundledRef {
                name: "refs/rift/chain/v1".to_owned(),
                target: "a".repeat(40),
            }],
            chain_head: String::new(),
            segment_sha256: String::new(),
            gitbundle_sha256: String::new(),
        }
    }

    /// A well-formed container round-trips byte-for-byte, binary part included.
    #[test]
    fn the_container_round_trips_including_binary_bytes() {
        // A git bundle is binary and WILL contain newlines and zero bytes; a
        // framing that only worked for text would pass every text test and fail
        // on the first real bundle.
        let git: Vec<u8> = (0u8..=255).chain(b"\n\n\n".iter().copied()).collect();
        let bytes = encode_container(b"{}", b"seg\nment\n", &git);
        let [m, s, g] = decode_container(&bytes).unwrap();
        assert_eq!(m, b"{}");
        assert_eq!(s, b"seg\nment\n");
        assert_eq!(g, git);
    }

    /// Every framing defect is a refusal, and each names its own reason.
    #[test]
    fn a_misframed_container_is_refused_by_reason() {
        let good = encode_container(b"{}", b"s", b"g");
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (b"not-a-bundle\n".to_vec(), "bad header"),
            (b"rift-bundle/2\nmanifest 0\n\n".to_vec(), "unsupported"),
            (b"rift-bundle/0\n".to_vec(), "unsupported"),
            (
                b"rift-bundle/1\nsegment 1\ns\n".to_vec(),
                "part order is fixed",
            ),
            (b"rift-bundle/1\nmanifest x\n".to_vec(), "canonical"),
            (b"rift-bundle/1\nmanifest 99\nab\n".to_vec(), "truncated"),
            (
                {
                    // Length lies by one: the terminator is not where it says.
                    let mut b = b"rift-bundle/1\nmanifest 1\n".to_vec();
                    b.extend_from_slice(b"ab\n");
                    b
                },
                "not terminated",
            ),
            (
                {
                    let mut b = good.clone();
                    b.push(b'x');
                    b
                },
                "trailing bytes",
            ),
        ];
        for (input, needle) in cases {
            let err = decode_container(&input)
                .map(|_| ())
                .expect_err("a misframed container must be refused");
            let text = err.to_string();
            assert!(
                text.contains(needle),
                "refusal must name the reason ({needle}); got: {text}"
            );
        }
        // …and the control: the good one decodes.
        assert!(decode_container(&good).is_ok());
    }

    /// **One artifact, one name.** Every integer in the frame has exactly one
    /// spelling; a second spelling is a refusal and not a synonym.
    ///
    /// This is a regression pin, not a hypothetical: `decode_container` parsed
    /// its version with `v.parse::<u32>()` and its lengths with `len.parse()`,
    /// and a verifier planted all four of these shapes and had **4 of 4
    /// ACCEPTED** — while `rift-cursor/01` and `rift-chain-segment/01` were
    /// refused two crates away. Injectivity is the property at stake: a frame
    /// that can be written two ways cannot be byte-compared with itself, and a
    /// digest-bound container that re-encodes canonically stops matching the
    /// bytes it was read from.
    ///
    /// The control matters as much as the plants: the canonical spelling of
    /// each of these decodes, so the rule refuses second spellings rather than
    /// merely refusing.
    #[test]
    fn a_non_canonical_integer_anywhere_in_the_frame_is_refused() {
        fn refusal(input: &[u8], what: &str) -> String {
            match decode_container(input) {
                Ok(_) => panic!("{what}: must be refused, was ACCEPTED"),
                Err(e) => e.to_string(),
            }
        }

        // The four shapes the verifier planted, each differing from a decodable
        // frame by the spelling of exactly ONE integer.
        let plants: [(&[u8], &str); 4] = [
            (
                b"rift-bundle/01\nmanifest 1\na\n",
                "version with a leading zero",
            ),
            (b"rift-bundle/+1\nmanifest 1\na\n", "signed version"),
            (
                b"rift-bundle/1\nmanifest 02\nab\n",
                "length with a leading zero",
            ),
            (b"rift-bundle/1\nmanifest +2\nab\n", "signed length"),
        ];
        for (input, what) in plants {
            let err = refusal(input, what);
            assert!(
                err.contains("canonical"),
                "{what}: the refusal must name canonicity; got: {err}"
            );
        }

        // The controls: the canonical spelling of each frame clears the version
        // and length rules and fails only on what comes after, so the refusals
        // above are about the spelling and not about the fixture.
        for control in [
            &b"rift-bundle/1\nmanifest 1\na\n"[..],
            &b"rift-bundle/1\nmanifest 2\nab\n"[..],
        ] {
            let err = refusal(control, "canonical control");
            assert!(
                !err.contains("canonical"),
                "the canonical control must clear the integer rules; got: {err}"
            );
        }
    }

    /// A refusal names the reason and quotes **no byte of the input** — the
    /// part name included.
    ///
    /// A part name is attacker-chosen UTF-8: `decode_container` used to echo it
    /// (`found '{name}'`), so a hostile bundle could print CSI/OSC sequences
    /// into the terminal of whoever ran `rift bundle import`, and `docs/
    /// sync-format.md` claims "every refusal names the reason and never the
    /// input". This asserts the sentence rather than trusting it.
    #[test]
    fn a_refusal_never_echoes_a_part_name_from_the_frame() {
        // A name carrying a CSI sequence, an OSC title-setter and an RTL
        // override — the three shapes that do something when printed.
        let hostile = "\u{1b}[2J\u{1b}]0;pwned\u{7}\u{202e}manifest";
        let mut input = b"rift-bundle/1\n".to_vec();
        input.extend_from_slice(format!("{hostile} 1\n").as_bytes());
        input.extend_from_slice(b"a\n");
        let err = decode_container(&input)
            .map(|_| ())
            .expect_err("an unknown part name must be refused");
        let text = err.to_string();
        assert!(
            text.contains("part 1 is not 'manifest'"),
            "the refusal must name the reason positionally; got: {text}"
        );
        for needle in ["\u{1b}", "pwned", "\u{202e}"] {
            assert!(
                !text.contains(needle),
                "the refusal echoed input bytes ({needle:?}); got: {text:?}"
            );
        }
    }

    /// The frame is not the only place a bundle's own bytes reach the importer's
    /// terminal: the **manifest** is attacker-authored JSON, and `serde`'s own
    /// error text quotes what it rejected.
    #[test]
    fn a_refusal_never_echoes_the_manifest_it_rejected() {
        let hostile = "\u{1b}[2J\u{1b}]0;pwned\u{7}\u{202e}evil";
        let seg = ChainSegment::between(&[], &StreamCursor::origin())
            .unwrap()
            .encode()
            .into_bytes();
        let git = b"PACKish-bytes".to_vec();

        // (a) an unknown FIELD NAME — `deny_unknown_fields` puts it in the
        //     error verbatim ("unknown field `…`").
        let mut m = serde_json::to_value(manifest()).unwrap();
        m.as_object_mut()
            .unwrap()
            .insert(hostile.to_owned(), serde_json::json!(1));
        // (b) a wrong-typed field — serde quotes the VALUE it found, so a
        //     hostile STRING where a number belongs is the same leak by
        //     another door.
        let mut n = serde_json::to_value(manifest()).unwrap();
        n.as_object_mut()
            .unwrap()
            .insert("schema_version".to_owned(), serde_json::json!(hostile));

        for (case, bad) in [("unknown field", m), ("wrong type", n)] {
            let bytes = encode_container(&serde_json::to_vec(&bad).unwrap(), &seg, &git);
            let err = verify_offline(&bytes)
                .map(|_| ())
                .expect_err("a manifest serde failure must be a refusal");
            let text = err.to_string();
            assert!(
                text.contains("manifest is not readable"),
                "{case}: the refusal must still name the reason; got: {text}"
            );
            for needle in ["\u{1b}", "pwned", "\u{202e}"] {
                assert!(
                    !text.contains(needle),
                    "{case}: the refusal echoed the manifest ({needle:?}); got: {text:?}"
                );
            }
        }
    }

    /// The digests are the reason a flipped byte anywhere in the payload is
    /// caught **before** git is asked to read anything.
    #[test]
    fn a_flipped_byte_in_any_part_is_caught_offline() {
        let seg = ChainSegment::between(&[], &StreamCursor::origin())
            .unwrap()
            .encode()
            .into_bytes();
        let git = b"PACKish-bytes".to_vec();
        let mut m = manifest();
        m.segment_sha256 = sha256_hex(&seg);
        m.gitbundle_sha256 = sha256_hex(&git);
        let mbytes = serde_json::to_vec(&m).unwrap();
        let good = encode_container(&mbytes, &seg, &git);
        assert!(verify_offline(&good).is_ok(), "the control must verify");

        // Flip one byte of the git-bundle part in place — the length framing
        // still holds, so only the digest can catch it.
        let mut tampered = good.clone();
        let idx = tampered.len() - 2;
        tampered[idx] ^= 0x01;
        let err = verify_offline(&tampered).expect_err("a flipped payload byte must be refused");
        assert!(
            err.to_string().contains("does not match the digest"),
            "got: {err}"
        );
    }

    /// **What the segment digest uniquely buys**, measured rather than assumed.
    ///
    /// `docs/sync-format.md` states plainly that a chain segment binds ORDER and
    /// MEMBERSHIP and does **not** authenticate a `merkle_root`: a flipped root
    /// is invisible to `ChainSegment::verify`. This test flips one hex digit of
    /// a root and shows both halves of that sentence at once — with the digest
    /// recomputed the tampered segment verifies **clean**, and with the digest
    /// left as the manifest recorded it the bundle is refused.
    ///
    /// This is the calibration that matters for the container, and it exists
    /// because a coarser one lied: removing the *git-bundle* digest check did
    /// not redden the end-to-end tamper test, since git's own pack integrity
    /// catches a flipped pack byte anyway. Nothing but this digest catches a
    /// flipped root.
    #[test]
    fn only_the_digest_catches_a_flipped_merkle_root() {
        use rift_hashchain::ChainLink;
        let id = rift_core::Sha256Id::from_bytes([7u8; 32]);
        let root = rift_core::Sha256Id::from_bytes([9u8; 32]);
        let links = vec![ChainLink {
            checkpoint_id: id,
            merkle_root: root,
            prev: None,
        }];
        let seg = ChainSegment::between(&links, &StreamCursor::origin()).unwrap();
        let good = seg.encode();
        let git = b"g".to_vec();

        let mut m = manifest();
        m.from_cursor = seg.from_cursor().to_string();
        m.to_cursor = seg.to_cursor().to_string();
        m.chain_head = seg.to_cursor().checkpoint_id().to_owned();
        m.segment_sha256 = sha256_hex(good.as_bytes());
        m.gitbundle_sha256 = sha256_hex(&git);

        // The tamper: one hex digit of the merkle_root, nothing else.
        let tampered = good.replacen("0909", "0908", 1);
        assert_ne!(tampered, good, "the tamper must have bitten");

        // Half one — the segment's own verification is BLIND to it.
        let reparsed = ChainSegment::decode(&tampered).unwrap();
        assert!(
            reparsed.verify().is_ok(),
            "order and membership are unchanged by a flipped root: the segment \
             cannot catch this, which is why the digest exists"
        );

        // Half two — the digest catches it.
        let bytes = encode_container(&serde_json::to_vec(&m).unwrap(), tampered.as_bytes(), &git);
        let err = verify_offline(&bytes).expect_err("a flipped root must be refused");
        assert!(
            err.to_string()
                .contains("chain segment does not match the digest"),
            "got: {err}"
        );
    }

    /// A manifest that disagrees with its own segment is a refusal — the two
    /// texts describe one window and both are read.
    #[test]
    fn a_manifest_that_contradicts_its_segment_is_refused() {
        let seg = ChainSegment::between(&[], &StreamCursor::origin())
            .unwrap()
            .encode()
            .into_bytes();
        let git = b"g".to_vec();
        let mut m = manifest();
        m.segment_sha256 = sha256_hex(&seg);
        m.gitbundle_sha256 = sha256_hex(&git);
        m.chain_head = "b".repeat(64);
        let bytes = encode_container(&serde_json::to_vec(&m).unwrap(), &seg, &git);
        let err = verify_offline(&bytes).expect_err("a contradicted manifest must be refused");
        assert!(err.to_string().contains("chain_head"), "got: {err}");
    }

    /// **The headline finding of the format's blind review, now closed.**
    /// The doc used to say two different things about `chain_head` on an
    /// empty window: the field table said "the checkpoint id at `to_cursor`",
    /// but verification step 5 said "the segment's last link id" — and those
    /// two readings diverge exactly here, an empty window at a NON-origin
    /// cursor (`--since <head>`, produced whenever the head has not moved).
    /// The document now picks the first reading and drops the second;
    /// this test is the worked vector proving the implementation already
    /// matches the reading the doc now states, in both directions: the real
    /// tip id is ACCEPTED, and the empty string the other reading would have
    /// written is REFUSED.
    #[test]
    fn an_empty_window_at_a_non_origin_cursor_reports_the_real_head() {
        use rift_hashchain::ChainLink;

        let id = rift_core::Sha256Id::from_bytes([3u8; 32]);
        let root = rift_core::Sha256Id::from_bytes([4u8; 32]);
        let links = vec![ChainLink {
            checkpoint_id: id,
            merkle_root: root,
            prev: None,
        }];
        // The head cursor of a one-link chain: position 1, that link's id —
        // exporting `--since` that cursor is what produces an empty window at
        // a NON-origin position (`from == to`, both at chain_seq 1).
        let head = StreamCursor::at(1, &id.to_hex()).unwrap();
        let seg = ChainSegment::between(&links, &head).unwrap();
        assert!(seg.links().is_empty(), "the window must be empty");
        assert_ne!(
            seg.to_cursor().chain_seq(),
            0,
            "must be a NON-origin cursor — the case the two doc readings disagreed on"
        );
        let git = b"g".to_vec();
        let seg_bytes = seg.encode().into_bytes();

        // ACCEPT: chain_head is the real id sitting at to_cursor.
        let mut good = manifest();
        good.from_cursor = seg.from_cursor().to_string();
        good.to_cursor = seg.to_cursor().to_string();
        good.chain_head = seg.to_cursor().checkpoint_id().to_owned();
        assert!(
            !good.chain_head.is_empty(),
            "the tip id must be real, not empty, at a non-origin cursor"
        );
        good.segment_sha256 = sha256_hex(&seg_bytes);
        good.gitbundle_sha256 = sha256_hex(&git);
        let bytes = encode_container(&serde_json::to_vec(&good).unwrap(), &seg_bytes, &git);
        verify_offline(&bytes).expect("the real tip id must be accepted");

        // REFUSE: the OTHER (rejected) doc reading — "" because the segment
        // itself is empty, ignoring which cursor it is empty AT.
        let mut wrong = good.clone();
        wrong.chain_head = String::new();
        let bytes = encode_container(&serde_json::to_vec(&wrong).unwrap(), &seg_bytes, &git);
        let err = verify_offline(&bytes).expect_err("the empty-string reading must be refused");
        assert!(err.to_string().contains("chain_head"), "got: {err}");
    }

    /// A manifest at a version this build does not read is refused WHOLE, not
    /// partially believed.
    #[test]
    fn a_future_manifest_version_is_refused_whole() {
        let seg = ChainSegment::between(&[], &StreamCursor::origin())
            .unwrap()
            .encode()
            .into_bytes();
        let git = b"g".to_vec();
        let mut m = manifest();
        m.schema_version = BUNDLE_SCHEMA_VERSION + 1;
        m.segment_sha256 = sha256_hex(&seg);
        m.gitbundle_sha256 = sha256_hex(&git);
        let bytes = encode_container(&serde_json::to_vec(&m).unwrap(), &seg, &git);
        let err = verify_offline(&bytes).expect_err("a future manifest must be refused");
        assert!(
            err.to_string().contains("unsupported manifest"),
            "got: {err}"
        );
    }

    /// Determinism: the same inputs encode to the same bytes.
    #[test]
    fn identical_inputs_encode_identically() {
        let a = encode_container(b"{\"a\":1}", b"s", b"g");
        let b = encode_container(b"{\"a\":1}", b"s", b"g");
        assert_eq!(a, b);
        let m = manifest();
        assert_eq!(
            serde_json::to_vec(&m).unwrap(),
            serde_json::to_vec(&m).unwrap()
        );
    }

    /// The two constant refs a bundle ever carries must themselves be legal —
    /// the checker must never refuse the tree's own data.
    #[test]
    fn the_bundled_ref_constants_are_legal() {
        for name in BUNDLED_REFS {
            assert!(is_legal_ref_name(name), "{name} must be legal");
        }
    }

    /// **From the blind review of this format.** Every shape the review demonstrated
    /// or that `git check-ref-format` itself refuses.
    #[test]
    fn illegal_ref_names_are_rejected() {
        let bad = [
            "not/a/ref",          // not fully-qualified — the review's own PoC
            "refs",               // no component after the namespace
            "HEAD",               // one-level, not under refs/
            "refs/",              // trailing slash, empty final component
            "refs//double",       // empty component
            "refs/a..b",          // two consecutive dots
            "refs/.hidden",       // component starts with `.`
            "refs/foo.lock",      // component ends in `.lock`
            "refs/a@{1}",         // `@{` sequence
            "refs/a~1",           // `~`
            "refs/a^1",           // `^`
            "refs/a:b",           // `:`
            "refs/a?b",           // `?`
            "refs/a*b",           // `*`
            "refs/a[b",           // `[`
            "refs/a\\b",          // backslash
            "refs/a\tb",          // control character
            "refs/a b",           // space
            "refs/trailing/",     // trailing slash
            "refs/trailing/dot.", // trailing dot
        ];
        for name in bad {
            assert!(!is_legal_ref_name(name), "{name:?} must be refused");
        }
    }

    /// Ordinary, real ref names — including ones with several path
    /// components — must pass.
    #[test]
    fn legal_ref_names_are_accepted() {
        for name in [
            "refs/heads/main",
            "refs/rift/chain/v1",
            "refs/heads/rift/checkpoints/v1",
            "refs/tags/v1.2.3",
        ] {
            assert!(is_legal_ref_name(name), "{name} must be accepted");
        }
    }

    /// **The ref-name finding, end to end through `verify_offline`.**
    /// Before this fix, a manifest naming `not/a/ref` in `ref_list` passed
    /// step 6 untouched (only the argv-safety screen ran) and the review
    /// measured `rift bundle import` creating `.git/not/a/ref` from it. Now
    /// the pure offline check refuses it, before any git process runs.
    #[test]
    fn a_ref_list_entry_that_is_not_fully_qualified_is_refused_offline() {
        let seg = ChainSegment::between(&[], &StreamCursor::origin())
            .unwrap()
            .encode()
            .into_bytes();
        let git = b"g".to_vec();
        let mut m = manifest();
        m.segment_sha256 = sha256_hex(&seg);
        m.gitbundle_sha256 = sha256_hex(&git);
        m.ref_list = vec![BundledRef {
            name: "not/a/ref".to_owned(),
            target: "a".repeat(40),
        }];
        let bytes = encode_container(&serde_json::to_vec(&m).unwrap(), &seg, &git);
        let err = verify_offline(&bytes).expect_err("a non-fully-qualified ref must be refused");
        assert!(err.to_string().contains("legal git ref"), "got: {err}");
    }

    /// `git`, run quietly with a pinned identity so a commit never depends on
    /// the machine's global config.
    fn git_test(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("git is on PATH");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// **The ref-list finding, end to end through `cmd_import`.**
    /// Before this fix, an empty `ref_list` alongside a git-bundle part that
    /// actually carries real heads was ACCEPTED — the review's own fixture
    /// (`36_ref_list_empty_but_bundle_has_heads`). This drives a REAL git
    /// bundle carrying two refs through `cmd_import` with a hand-built
    /// manifest whose `ref_list` is empty (everything else — the frame, both
    /// digests, the segment — is well-formed), and asserts the refusal names
    /// the mismatch rather than silently applying nothing.
    #[test]
    fn a_ref_list_that_omits_the_bundles_real_heads_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        git_test(&src, &["init", "-q", "-b", "main"]);
        std::fs::write(src.join("a.txt"), "one\n").unwrap();
        git_test(&src, &["add", "a.txt"]);
        git_test(&src, &["commit", "-q", "-m", "one"]);
        git_test(&src, &["branch", "-q", "second"]);
        std::fs::write(src.join("a.txt"), "two\n").unwrap();
        git_test(&src, &["add", "a.txt"]);
        git_test(&src, &["commit", "-q", "-m", "two"]);

        // A real git bundle carrying TWO distinct heads.
        let git_bundle_path = tmp.path().join("real.gitbundle");
        git_test(
            &src,
            &[
                "bundle",
                "create",
                "--quiet",
                git_bundle_path.to_str().unwrap(),
                "refs/heads/main",
                "refs/heads/second",
            ],
        );
        let gitbundle = std::fs::read(&git_bundle_path).unwrap();

        // The empty segment (legal at the origin) with a manifest whose
        // ref_list names NEITHER real head.
        let seg = ChainSegment::between(&[], &StreamCursor::origin()).unwrap();
        let seg_bytes = seg.encode().into_bytes();
        let mut m = manifest();
        m.ref_list = Vec::new();
        m.from_cursor = seg.from_cursor().to_string();
        m.to_cursor = seg.to_cursor().to_string();
        m.chain_head = seg.to_cursor().checkpoint_id().to_owned();
        m.segment_sha256 = sha256_hex(&seg_bytes);
        m.gitbundle_sha256 = sha256_hex(&gitbundle);
        let container = encode_container(&serde_json::to_vec(&m).unwrap(), &seg_bytes, &gitbundle);
        let bundle_file = tmp.path().join("x.riftbundle");
        std::fs::write(&bundle_file, &container).unwrap();

        let target = tmp.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        git_test(&target, &["init", "-q", "-b", "main"]);

        let err = cmd_import(&target, &bundle_file)
            .expect_err("an empty ref_list against a 2-head bundle must be refused");
        assert!(err.to_string().contains("ref_list"), "got: {err}");
    }
    /// **Step 6b, unit: an incremental bundle cannot bootstrap an empty
    /// repository — and the objects are never written.** The segment resumes
    /// from position 1 of a 2-link chain (a well-formed incremental window)
    /// and the target holds no chain ref. The git-bundle part is deliberately
    /// NOT a git bundle: had the attach check not run first, `git bundle
    /// verify` would have refused it with "damaged or incomplete" — so the
    /// refusal's own wording is the proof of ORDER, and the absent tmp file
    /// is the proof nothing reached the object store.
    #[test]
    fn an_incremental_bundle_does_not_bootstrap_an_empty_repository() {
        use rift_hashchain::ChainLink;
        let links = vec![
            ChainLink {
                checkpoint_id: rift_core::Sha256Id::from_bytes([1u8; 32]),
                merkle_root: rift_core::Sha256Id::from_bytes([9u8; 32]),
                prev: None,
            },
            ChainLink {
                checkpoint_id: rift_core::Sha256Id::from_bytes([2u8; 32]),
                merkle_root: rift_core::Sha256Id::from_bytes([9u8; 32]),
                prev: Some(rift_core::Sha256Id::from_bytes([1u8; 32])),
            },
        ];
        let held = rift_store::cursor::head_of(&links[..1]);
        assert!(!held.is_origin());
        let seg = ChainSegment::between(&links, &held).unwrap();
        assert_eq!(seg.links().len(), 1, "an incremental window of one link");
        let seg_bytes = seg.encode().into_bytes();
        let not_a_bundle = b"PACKish-bytes".to_vec();
        let mut m = manifest();
        m.from_cursor = seg.from_cursor().to_string();
        m.to_cursor = seg.to_cursor().to_string();
        m.chain_head = seg.to_cursor().checkpoint_id().to_owned();
        m.segment_sha256 = sha256_hex(&seg_bytes);
        m.gitbundle_sha256 = sha256_hex(&not_a_bundle);
        let container =
            encode_container(&serde_json::to_vec(&m).unwrap(), &seg_bytes, &not_a_bundle);
        let tmp = tempfile::tempdir().unwrap();
        let bundle_file = tmp.path().join("inc.riftbundle");
        std::fs::write(&bundle_file, &container).unwrap();

        let target = tmp.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        git_test(&target, &["init", "-q", "-b", "main"]);

        let err = cmd_import(&target, &bundle_file)
            .expect_err("an incremental bundle must not bootstrap an empty repository")
            .to_string();
        assert!(err.contains("cannot bootstrap"), "got: {err}");
        assert!(
            !err.contains("damaged or incomplete"),
            "git ran before the attach check: {err}"
        );
        assert!(
            !target.join(".git/rift-import.gitbundle.tmp").exists(),
            "the refusal wrote the bundle part to the repository"
        );
    }
}
