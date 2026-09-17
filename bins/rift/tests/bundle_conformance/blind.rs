//! **The blind reader**: a second implementation of the offline bundle
//! verification, written from `docs/sync-format.md` alone — its Transfer
//! bundle, Stream cursor and Chain segment entries — and from nothing in this
//! repository's own crates. It uses `std`, `sha2` and `serde_json` only; the
//! self-pin `the_blind_reader_uses_nothing_of_rifts_own` keeps it that way.
//!
//! Steps 1–6 of the bundle entry's verification order are PURE (no git, no
//! store, no clock); steps 7–9 are asked of git itself, in a scratch copy of
//! the importing repository so the reader mutates nothing it is judging.
//! Every refusal names the reason and never the input.
//!
//! Where the document leaves a WRITER rule (manifest compactness and field
//! order), this reader records it as an INFO property, never a refusal.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

/// A refusal, by reason. Never carries input bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal(pub(crate) &'static str);

/// The one container version this reader understands.
const CONTAINER_VERSION: u64 = 1;
/// The one manifest version this reader understands.
const MANIFEST_VERSION: u64 = 1;
/// The one cursor / segment version this reader understands.
const CURSOR_VERSION: u64 = 1;
/// The fixed sanity ceiling on any one part's declared length, checked
/// BEFORE the length is believed (bundle entry, failure posture).
const PART_CEILING: u64 = 512 * 1024 * 1024;
/// The three part names, in the one order they may appear.
const PART_ORDER: [&str; 3] = ["manifest", "segment", "gitbundle"];
/// The manifest's closed member set, in writer order.
const MANIFEST_FIELDS: [&str; 8] = [
    "schema_version",
    "repo_id",
    "from_cursor",
    "to_cursor",
    "ref_list",
    "chain_head",
    "segment_sha256",
    "gitbundle_sha256",
];

/// A canonical base-10 integer: ASCII digits, no leading zero unless the
/// value is exactly `0`, no sign, no padding, no radix prefix.
pub(crate) fn canonical_u64(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

/// Lowercase hex SHA-256 of `bytes`, plain, no domain prefix.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

// ---------------------------------------------------------------------------
// Step 1 — the frame
// ---------------------------------------------------------------------------

/// The three parts of a parsed container.
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub(crate) manifest: Vec<u8>,
    pub(crate) segment: Vec<u8>,
    pub(crate) gitbundle: Vec<u8>,
}

fn take_line(bytes: &[u8], at: usize) -> Result<(&[u8], usize), Refusal> {
    let rest = bytes.get(at..).ok_or(Refusal("frame: truncated"))?;
    let nl = rest
        .iter()
        .position(|b| *b == b'\n')
        .ok_or(Refusal("frame: header line is not newline-terminated"))?;
    Ok((&rest[..nl], at + nl + 1))
}

/// Parse the container: header, three parts in fixed order, each
/// `<name> <len>\n<len bytes>\n`, nothing after the last terminator.
pub(crate) fn parse_frame(bytes: &[u8]) -> Result<Frame, Refusal> {
    let (header, mut at) = take_line(bytes, 0)?;
    let header = std::str::from_utf8(header).map_err(|_| Refusal("frame: header is not ASCII"))?;
    let version = header
        .strip_prefix("rift-bundle/")
        .ok_or(Refusal("frame: header is not a rift-bundle header"))?;
    match canonical_u64(version) {
        Some(v) if v == CONTAINER_VERSION => {}
        Some(0) => return Err(Refusal("frame: container version 0 never existed")),
        Some(_) => return Err(Refusal("frame: unknown container version")),
        None => {
            return Err(Refusal(
                "frame: container version is not a canonical integer",
            ))
        }
    }
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for expected in PART_ORDER {
        let (line, after) = take_line(bytes, at)?;
        let line =
            std::str::from_utf8(line).map_err(|_| Refusal("frame: part header is not ASCII"))?;
        let (name, len) = line
            .split_once(' ')
            .ok_or(Refusal("frame: part header has no length"))?;
        if name != expected {
            return Err(Refusal("frame: part name unknown or out of order"));
        }
        let len =
            canonical_u64(len).ok_or(Refusal("frame: part length is not a canonical integer"))?;
        if len > PART_CEILING {
            return Err(Refusal("frame: part length exceeds the sanity ceiling"));
        }
        let len = usize::try_from(len)
            .map_err(|_| Refusal("frame: part length exceeds the sanity ceiling"))?;
        let end = after
            .checked_add(len)
            .ok_or(Refusal("frame: part length runs past the end of the input"))?;
        let body = bytes
            .get(after..end)
            .ok_or(Refusal("frame: part length runs past the end of the input"))?;
        if bytes.get(end) != Some(&b'\n') {
            return Err(Refusal(
                "frame: part is not followed by exactly one newline",
            ));
        }
        parts.push(body.to_vec());
        at = end + 1;
    }
    if at != bytes.len() {
        return Err(Refusal("frame: trailing bytes after the last part"));
    }
    let gitbundle = parts.pop().unwrap_or_default();
    let segment = parts.pop().unwrap_or_default();
    let manifest = parts.pop().unwrap_or_default();
    Ok(Frame {
        manifest,
        segment,
        gitbundle,
    })
}

// ---------------------------------------------------------------------------
// Step 2 — the manifest
// ---------------------------------------------------------------------------

/// One `ref_list` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefEntry {
    pub(crate) name: String,
    pub(crate) target: String,
}

/// The manifest's eight members, typed.
#[derive(Debug, Clone)]
pub(crate) struct Manifest {
    pub(crate) schema_version: u64,
    pub(crate) repo_id: String,
    pub(crate) from_cursor: String,
    pub(crate) to_cursor: String,
    pub(crate) ref_list: Vec<RefEntry>,
    pub(crate) chain_head: String,
    pub(crate) segment_sha256: String,
    pub(crate) gitbundle_sha256: String,
    /// INFO, never a refusal: the part's bytes are exactly the compact,
    /// field-ordered serialization a conforming writer emits.
    pub(crate) writer_conforming: bool,
}

fn string_member(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, Refusal> {
    obj.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or(Refusal(
            "manifest: a string member is missing or not a string",
        ))
}

/// The compact, field-ordered bytes a conforming writer emits for `m`.
pub(crate) fn conforming_manifest_bytes(m: &Manifest) -> Vec<u8> {
    let refs: Vec<serde_json::Value> = m
        .ref_list
        .iter()
        .map(|r| serde_json::json!({ "name": r.name, "target": r.target }))
        .collect();
    // Assembled member by member in MANIFEST_FIELDS order so the order is
    // this function's, not the serializer's map order.
    let mut out = String::from("{");
    let values: [serde_json::Value; 8] = [
        serde_json::Value::from(m.schema_version),
        serde_json::Value::from(m.repo_id.as_str()),
        serde_json::Value::from(m.from_cursor.as_str()),
        serde_json::Value::from(m.to_cursor.as_str()),
        serde_json::Value::Array(refs),
        serde_json::Value::from(m.chain_head.as_str()),
        serde_json::Value::from(m.segment_sha256.as_str()),
        serde_json::Value::from(m.gitbundle_sha256.as_str()),
    ];
    for (i, (key, value)) in MANIFEST_FIELDS.iter().zip(values.iter()).enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(key).unwrap_or_default());
        out.push(':');
        out.push_str(&serde_json::to_string(value).unwrap_or_default());
    }
    out.push('}');
    out.into_bytes()
}

/// Parse the manifest: one JSON object, the closed eight-member set (an
/// unknown member is a refusal, a missing one too), a readable version.
pub(crate) fn parse_manifest(bytes: &[u8]) -> Result<Manifest, Refusal> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| Refusal("manifest: not a JSON document"))?;
    let obj = value
        .as_object()
        .ok_or(Refusal("manifest: not a JSON object"))?;
    for key in obj.keys() {
        if !MANIFEST_FIELDS.contains(&key.as_str()) {
            return Err(Refusal("manifest: unknown member"));
        }
    }
    for key in MANIFEST_FIELDS {
        if !obj.contains_key(key) {
            return Err(Refusal("manifest: a member is missing"));
        }
    }
    let schema_version = obj
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or(Refusal("manifest: schema_version is not an integer"))?;
    match schema_version {
        0 => return Err(Refusal("manifest: schema_version 0 never existed")),
        v if v == MANIFEST_VERSION => {}
        _ => return Err(Refusal("manifest: unknown schema_version")),
    }
    let refs = obj
        .get("ref_list")
        .and_then(serde_json::Value::as_array)
        .ok_or(Refusal("manifest: ref_list is not an array"))?;
    let mut ref_list = Vec::new();
    for entry in refs {
        let entry = entry
            .as_object()
            .ok_or(Refusal("manifest: a ref_list entry is not an object"))?;
        if entry.len() != 2 {
            return Err(Refusal(
                "manifest: a ref_list entry has members other than name/target",
            ));
        }
        ref_list.push(RefEntry {
            name: string_member(entry, "name")?,
            target: string_member(entry, "target")?,
        });
    }
    let mut m = Manifest {
        schema_version,
        repo_id: string_member(obj, "repo_id")?,
        from_cursor: string_member(obj, "from_cursor")?,
        to_cursor: string_member(obj, "to_cursor")?,
        ref_list,
        chain_head: string_member(obj, "chain_head")?,
        segment_sha256: string_member(obj, "segment_sha256")?,
        gitbundle_sha256: string_member(obj, "gitbundle_sha256")?,
        writer_conforming: false,
    };
    m.writer_conforming = conforming_manifest_bytes(&m) == bytes;
    Ok(m)
}

// ---------------------------------------------------------------------------
// The stream cursor
// ---------------------------------------------------------------------------

/// A parsed cursor: version, 1-based chain position, checkpoint id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) chain_seq: u64,
    pub(crate) checkpoint_id: String,
}

impl Cursor {
    pub(crate) fn is_origin(&self) -> bool {
        self.chain_seq == 0
    }
}

/// `rift-cursor/<v>:<seq>:<id>` — exactly three fields, canonical ints,
/// the id `[0-9A-Za-z]{0,64}`, and the origin's two halves one fact.
pub(crate) fn parse_cursor(text: &str) -> Result<Cursor, Refusal> {
    let rest = text
        .strip_prefix("rift-cursor/")
        .ok_or(Refusal("cursor: not a rift-cursor"))?;
    let fields: Vec<&str> = rest.split(':').collect();
    if fields.len() != 3 {
        return Err(Refusal("cursor: not exactly three fields"));
    }
    let version =
        canonical_u64(fields[0]).ok_or(Refusal("cursor: version is not a canonical integer"))?;
    match version {
        0 => return Err(Refusal("cursor: version 0 never existed")),
        v if v == CURSOR_VERSION => {}
        _ => return Err(Refusal("cursor: unknown version")),
    }
    let chain_seq =
        canonical_u64(fields[1]).ok_or(Refusal("cursor: chain_seq is not a canonical integer"))?;
    let id = fields[2];
    if id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Refusal(
            "cursor: checkpoint_id is outside its charset or length",
        ));
    }
    if (chain_seq == 0) != id.is_empty() {
        return Err(Refusal("cursor: origin halves disagree"));
    }
    Ok(Cursor {
        chain_seq,
        checkpoint_id: id.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Step 4 — the chain segment
// ---------------------------------------------------------------------------

/// One `link` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) checkpoint_id: String,
    pub(crate) merkle_root: String,
    pub(crate) prev: Option<String>,
}

/// A parsed segment.
#[derive(Debug, Clone)]
pub(crate) struct Segment {
    pub(crate) from: Cursor,
    pub(crate) to: Cursor,
    pub(crate) links: Vec<Link>,
}

fn lower_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parse the segment: UTF-8, every line `\n`-terminated (no `\r\n`, the last
/// line included), no blank lines, at least three lines, a canonical header
/// version, `from`/`to` cursors, then `link` records only.
pub(crate) fn parse_segment(bytes: &[u8]) -> Result<Segment, Refusal> {
    let text = std::str::from_utf8(bytes).map_err(|_| Refusal("segment: not UTF-8"))?;
    if !text.ends_with('\n') {
        return Err(Refusal("segment: final line is not newline-terminated"));
    }
    let body = &text[..text.len() - 1];
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.iter().any(|l| l.is_empty()) {
        return Err(Refusal("segment: blank line"));
    }
    if lines.iter().any(|l| l.ends_with('\r')) {
        return Err(Refusal("segment: CRLF line termination"));
    }
    if lines.len() < 3 {
        return Err(Refusal("segment: fewer than three lines"));
    }
    let version = lines[0]
        .strip_prefix("rift-chain-segment/")
        .ok_or(Refusal("segment: header is not a chain-segment header"))?;
    match canonical_u64(version) {
        Some(v) if v == CURSOR_VERSION => {}
        Some(0) => return Err(Refusal("segment: version 0 never existed")),
        Some(_) => return Err(Refusal("segment: unknown version")),
        None => return Err(Refusal("segment: version is not a canonical integer")),
    }
    let from = parse_cursor(
        lines[1]
            .strip_prefix("from ")
            .ok_or(Refusal("segment: line 2 is not a from cursor"))?,
    )?;
    let to = parse_cursor(
        lines[2]
            .strip_prefix("to ")
            .ok_or(Refusal("segment: line 3 is not a to cursor"))?,
    )?;
    let mut records = Vec::new();
    for line in &lines[3..] {
        let rest = line
            .strip_prefix("link ")
            .ok_or(Refusal("segment: a line is not a link record"))?;
        let fields: Vec<&str> = rest.split(' ').collect();
        if fields.len() != 3 {
            return Err(Refusal(
                "segment: a link record does not have exactly three fields",
            ));
        }
        if !lower_hex64(fields[0]) || !lower_hex64(fields[1]) {
            return Err(Refusal(
                "segment: a hex field is not 64 lowercase hex characters",
            ));
        }
        let prev = if fields[2] == "-" {
            None
        } else if lower_hex64(fields[2]) {
            Some(fields[2].to_owned())
        } else {
            return Err(Refusal("segment: prev is neither 64 lowercase hex nor `-`"));
        };
        records.push(Link {
            checkpoint_id: fields[0].to_owned(),
            merkle_root: fields[1].to_owned(),
            prev,
        });
    }
    Ok(Segment {
        from,
        to,
        links: records,
    })
}

/// The four things segment verification establishes, plus the empty-segment
/// rule. Roots are NOT authenticated here (the entry says so).
pub(crate) fn verify_segment(seg: &Segment) -> Result<(), Refusal> {
    let n = seg.links.len() as u64;
    if seg.to.chain_seq != seg.from.chain_seq + n {
        return Err(Refusal(
            "segment: to.chain_seq != from.chain_seq + links (rule 1)",
        ));
    }
    if seg.links.is_empty() {
        if seg.from != seg.to {
            return Err(Refusal("segment: empty segment with from != to"));
        }
        return Ok(());
    }
    let first = &seg.links[0];
    match (&first.prev, seg.from.is_origin()) {
        (None, true) => {}
        (Some(prev), false) if *prev == seg.from.checkpoint_id => {}
        (Some(_), true) => {
            return Err(Refusal(
                "segment: first link has a prev at the origin (rule 2)",
            ))
        }
        (None, false) => {
            return Err(Refusal(
                "segment: first link is genesis but from is not the origin (rule 2)",
            ))
        }
        (Some(_), false) => {
            return Err(Refusal(
                "segment: first link's prev is not from's checkpoint id (rule 2)",
            ))
        }
    }
    for pair in seg.links.windows(2) {
        if pair[1].prev.as_deref() != Some(pair[0].checkpoint_id.as_str()) {
            return Err(Refusal(
                "segment: a link's prev is not the previous link's id (rule 3)",
            ));
        }
    }
    if seg.links[n as usize - 1].checkpoint_id != seg.to.checkpoint_id {
        return Err(Refusal(
            "segment: to's checkpoint id is not the last link's id (rule 4)",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Step 6 — legal refs and oids
// ---------------------------------------------------------------------------

/// A legal, FULLY-QUALIFIED ref name: the argv-safety screen plus
/// `check-ref-format`'s rule set, as the bundle entry enumerates them.
pub(crate) fn legal_ref_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('-') {
        return false;
    }
    if name.bytes().any(|b| b < 0x20 || b == 0x7f || b == b' ') {
        return false;
    }
    if name.contains(|c: char| "~^:?*[\\;&|$`'\"<>(){}".contains(c)) {
        return false;
    }
    if !name.starts_with("refs/") {
        return false;
    }
    if name.ends_with('/') || name.ends_with('.') || name.contains("..") || name.contains("@{") {
        return false;
    }
    name.split('/').all(|component| {
        !component.is_empty()
            && !component.starts_with('.')
            && !component.as_bytes().ends_with(b".lock")
            && component != "@"
    })
}

/// A legal oid: 40 or 64 hex characters, either case, never normalised.
pub(crate) fn legal_oid(oid: &str) -> bool {
    (oid.len() == 40 || oid.len() == 64) && oid.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// Steps 1–6 together — the pure verdict
// ---------------------------------------------------------------------------

/// What the pure steps established, for the git-side steps to continue from.
#[derive(Debug, Clone)]
pub(crate) struct PureAccept {
    pub(crate) manifest: Manifest,
    pub(crate) segment: Segment,
    pub(crate) gitbundle: Vec<u8>,
}

/// Steps 1–6, in the document's order, which is the security property.
pub(crate) fn verify_pure(bytes: &[u8]) -> Result<PureAccept, Refusal> {
    // (1) the frame parses
    let frame = parse_frame(bytes)?;
    // (2) the manifest parses at a readable version
    let manifest = parse_manifest(&frame.manifest)?;
    // (3) both digests match the parts as read
    if sha256_hex(&frame.segment) != manifest.segment_sha256 {
        return Err(Refusal(
            "digest: segment_sha256 does not match the segment part",
        ));
    }
    if sha256_hex(&frame.gitbundle) != manifest.gitbundle_sha256 {
        return Err(Refusal(
            "digest: gitbundle_sha256 does not match the gitbundle part",
        ));
    }
    // (4) the segment verifies on its own bytes
    let segment = parse_segment(&frame.segment)?;
    verify_segment(&segment)?;
    // (5) the manifest agrees with the segment
    let from = parse_cursor(&manifest.from_cursor)?;
    let to = parse_cursor(&manifest.to_cursor)?;
    if from != segment.from || to != segment.to {
        return Err(Refusal("agree: manifest cursors differ from the segment's"));
    }
    if manifest.chain_head != to.checkpoint_id {
        return Err(Refusal(
            "agree: chain_head is not the checkpoint id at to_cursor",
        ));
    }
    if manifest.chain_head.is_empty() != to.is_origin() {
        return Err(Refusal("agree: chain_head empty only at the origin"));
    }
    // (6) every ref name legal and fully qualified, every target a legal oid
    for r in &manifest.ref_list {
        if !legal_ref_name(&r.name) {
            return Err(Refusal("refs: illegal or unqualified ref name"));
        }
        if !legal_oid(&r.target) {
            return Err(Refusal("refs: illegal oid"));
        }
    }
    Ok(PureAccept {
        manifest,
        segment,
        gitbundle: frame.gitbundle,
    })
}

// ---------------------------------------------------------------------------
// Steps 7–9 — git itself, in a scratch copy of the importing repository
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git is on PATH")
}

/// The importing repository's canonical order — the first-parent walk of
/// `refs/rift/chain/v1`, each entry's id riding the `Rift-Chain-Checkpoint`
/// trailer (Chain ref entry: "a consumer recovers the whole order from commit
/// objects alone"). Entry N is that checkpoint's `chain_seq`. An absent ref is
/// an empty order — a repository with no chain.
fn local_chain_order(repo: &Path) -> Vec<String> {
    let out = git(
        repo,
        &[
            "log",
            "--first-parent",
            "--reverse",
            "--format=%(trailers:key=Rift-Chain-Checkpoint,valueonly)",
            "refs/rift/chain/v1",
        ],
    );
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Step 6b (chain-segment entry, IMPORT IS ALL-OR-NOTHING): the segment must
/// ATTACH to the importing repository — its `from` cursor names a position the
/// repository holds, AND the two chains agree at every position where they
/// overlap. The origin cursor carries no identity, so against a repository
/// with NO chain only an origin segment attaches: an incremental one would
/// write a second genesis.
pub(crate) fn verify_attach(segment: &Segment, order: &[String]) -> Result<(), Refusal> {
    let from = &segment.from;
    if order.is_empty() {
        return if from.is_origin() {
            Ok(())
        } else {
            Err(Refusal(
                "attach: an incremental segment cannot bootstrap a repository with no chain",
            ))
        };
    }
    let held = usize::try_from(from.chain_seq)
        .map_err(|_| Refusal("attach: the from cursor's position is not addressable"))?;
    if held > order.len() {
        return Err(Refusal(
            "attach: the from cursor is ahead of this repository's chain",
        ));
    }
    if let Some(at) = held.checked_sub(1) {
        let names_held = order
            .get(at)
            .is_some_and(|id| id.eq_ignore_ascii_case(&from.checkpoint_id));
        if !names_held {
            return Err(Refusal(
                "attach: the from cursor names a checkpoint this repository does not hold there",
            ));
        }
    }
    for (i, link) in segment.links.iter().enumerate() {
        if let Some(mine) = order.get(held + i) {
            if !mine.eq_ignore_ascii_case(&link.checkpoint_id) {
                return Err(Refusal(
                    "attach: the chains disagree at a position they both hold",
                ));
            }
        }
    }
    Ok(())
}

/// Steps 7–9: git accepts the bundle and its heads set-equal `ref_list`;
/// every target's objects are present; every ref is absent, equal, or an
/// ancestor of its target. `scratch` is a COPY of the importing repository
/// that this reader may write objects into.
pub(crate) fn verify_git_side(accepted: &PureAccept, scratch: &Path) -> Result<(), Refusal> {
    // (6b) the segment attaches to the importing repository's own chain —
    // decided before a single object is written.
    verify_attach(&accepted.segment, &local_chain_order(scratch))?;
    let file = scratch.join("blind-reader.gitbundle");
    std::fs::write(&file, &accepted.gitbundle).map_err(|_| Refusal("git: scratch write failed"))?;
    let file_s = file
        .to_str()
        .ok_or(Refusal("git: scratch path is not UTF-8"))?;
    // (7) git accepts the bundle as intact …
    if !git(scratch, &["bundle", "verify", "--quiet", file_s])
        .status
        .success()
    {
        return Err(Refusal("git: bundle verify refused the bundle"));
    }
    // … and ref_list set-equals the heads the bundle's own header carries.
    let heads = git(scratch, &["bundle", "list-heads", file_s]);
    if !heads.status.success() {
        return Err(Refusal("git: bundle list-heads failed"));
    }
    let actual: BTreeSet<(String, String)> = String::from_utf8_lossy(&heads.stdout)
        .lines()
        .filter_map(|l| {
            l.split_once(' ')
                .map(|(oid, name)| (name.to_owned(), oid.to_owned()))
        })
        .collect();
    let claimed: BTreeSet<(String, String)> = accepted
        .manifest
        .ref_list
        .iter()
        .map(|r| (r.name.clone(), r.target.clone()))
        .collect();
    if actual != claimed {
        return Err(Refusal(
            "git: ref_list does not set-equal the bundle's heads",
        ));
    }
    // (8) the objects for every target are present once read in (objects
    // are inert until a ref names them).
    if !git(scratch, &["bundle", "unbundle", file_s])
        .status
        .success()
    {
        return Err(Refusal("git: unbundle failed"));
    }
    for r in &accepted.manifest.ref_list {
        if !git(scratch, &["cat-file", "-e", &r.target])
            .status
            .success()
        {
            return Err(Refusal("git: a target's objects are absent"));
        }
        // (9) absent, equal, or fast-forward.
        let local = git(scratch, &["rev-parse", "--verify", "--quiet", &r.name]);
        if !local.status.success() {
            continue;
        }
        let cur = String::from_utf8_lossy(&local.stdout).trim().to_owned();
        if cur.eq_ignore_ascii_case(&r.target) {
            continue;
        }
        if !git(scratch, &["merge-base", "--is-ancestor", &cur, &r.target])
            .status
            .success()
        {
            return Err(Refusal("git: a ref would not fast-forward"));
        }
    }
    Ok(())
}

/// The whole verdict, steps 1–9.
pub(crate) fn verify(bytes: &[u8], scratch: &Path) -> Result<PureAccept, Refusal> {
    let accepted = verify_pure(bytes)?;
    verify_git_side(&accepted, scratch)?;
    Ok(accepted)
}

// ---------------------------------------------------------------------------
// A writer, for the mutation matrix: assemble a container from parts
// ---------------------------------------------------------------------------

/// Frame three parts exactly as the container entry spells it.
pub(crate) fn assemble(manifest: &[u8], segment: &[u8], gitbundle: &[u8]) -> Vec<u8> {
    let mut out = format!("rift-bundle/{CONTAINER_VERSION}\n").into_bytes();
    for (name, part) in PART_ORDER.iter().zip([manifest, segment, gitbundle]) {
        out.extend_from_slice(format!("{name} {}\n", part.len()).as_bytes());
        out.extend_from_slice(part);
        out.push(b'\n');
    }
    out
}
