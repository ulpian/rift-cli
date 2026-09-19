//! `gate-intercept` — the read-only guard on the read-interception surface.
//!
//! Every leg below answers one question about the tree AS TRACKED BY GIT, with
//! no build and no cargo subprocess: the gate is meant to run in seconds on a
//! checkout, before anything expensive, and to say plainly which of its legs
//! passed.
//!
//! What each leg holds:
//!
//! * `one-gate-chokepoint` — one file composes an agent's hook response.
//! * `pinned-channels` — the channels Rift answers on are reviewable as a list.
//! * `no-store-on-gate-path` — the decision path opens no store and loads no
//!   tokenizer.
//! * `one-reason-renderer` — one module writes the sentences a model reads.
//! * `closed-rewrite-forms` — every tool-call rewrite is one of three pinned forms.
//! * `family-keyed-caches` — a cache is keyed by the repository family, never
//!   by which checkout happened to be open.
//! * `load-bearing` — not built; see [`load_bearing`].
//!
//! Each leg reports its own violations; the command fails if any leg does.
//!
//! **The honest limits, stated once.** These are text screens over source, not
//! a compiler. They read the production half of a file (everything before its
//! trailing test module, the same horizon the CI meta-checks use), with
//! comments and string bodies masked out where a leg is asking about code
//! rather than prose. They resolve a call by bare function name, which
//! over-matches (a method and a free function of the same name are one node)
//! and never follows a trait object or a function pointer. Every limit is
//! chosen so a screen errs toward reporting: an ambiguous shape is a finding,
//! not an exemption.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use crate::ci_meta::{production_half, tracked_paths};

/// One leg's outcome: its name, whether it was built, and its findings.
#[derive(Debug)]
pub struct Leg {
    /// The leg's name, as the plan and the printed line spell it.
    pub name: &'static str,
    /// What the leg concluded, in one line, whether it passed or failed.
    pub detail: String,
    /// Everything the leg objects to. Empty means the leg holds.
    pub violations: Vec<String>,
}

impl Leg {
    fn new(name: &'static str, detail: String, violations: Vec<String>) -> Self {
        Self {
            name,
            detail,
            violations,
        }
    }
}

/// Every tracked `.rs` file under `crates/` and `bins/`, as `(path, text)`.
///
/// Tracked, not walked: an untracked scratch file is not screened, and a
/// tracked file deleted on disk is a hard error rather than a quiet pass.
///
/// # Errors
/// If `git ls-files` fails or a tracked file cannot be read.
fn rust_corpus(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for rel in tracked_paths(root)? {
        let is_rust = Path::new(&rel)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"));
        if !is_rust || !(rel.starts_with("crates/") || rel.starts_with("bins/")) {
            continue;
        }
        let text = std::fs::read_to_string(root.join(&rel))
            .map_err(|e| format!("read tracked file {rel}: {e}"))?;
        out.push((rel, text));
    }
    if out.is_empty() {
        return Err("gate-intercept: the tracked Rust corpus is empty".to_owned());
    }
    Ok(out)
}

/// True for a path whose contents are test scaffolding rather than shipped
/// code: an integration-test file, a bench, or a pinned fixture.
fn is_test_surface(rel: &str) -> bool {
    rel.contains("/tests/") || rel.contains("/benches/") || rel.contains("/fixtures/")
}

/// Line number (1-based) of the byte offset `at` in `text`.
fn line_of(text: &str, at: usize) -> usize {
    text.get(..at).unwrap_or("").matches('\n').count() + 1
}

// ---------------------------------------------------------------- masking ---
//
// Reading Rust as text, shared by every leg that asks about code rather than
// prose: strip what is not code, find the functions, and see which of them
// call which. Public inside this module's crate so the legs share ONE reading
// -- two readers with two ideas of where a function ends is how a screen goes
// blind over half a file without anyone noticing.

/// `text` with every comment, string literal body and character literal
/// replaced by spaces, and every other byte left exactly where it was.
///
/// Offsets are preserved so a finding can still be reported at its real line.
/// The point is to let a leg ask about CODE: a needle named in a doc comment
/// that explains why the needle is absent must not be the thing that reds the
/// gate, and a needle inside a test's expected-output string is not a call.
///
/// Handles the shapes this workspace actually contains: `//` and `/* */`
/// (nested), `"…"` with escapes, `r"…"` and `r#"…"#` (any hash count, with an
/// optional `b` prefix), and `'c'` — with a lifetime (`'a`, `'static`) left
/// alone, since it is code.
#[must_use]
pub fn mask_code(text: &str) -> String {
    mask(text, false)
}

/// `text` with its comments blanked and its string literals LEFT STANDING.
///
/// The legs that ask "does this file write that sentence?" or "does this path
/// get built here?" are asking about string literals, so those legs cannot use
/// [`mask_code`]. What they still must drop is prose: this workspace explains
/// its guards in comments beside the code they guard, and a screen that reds on
/// the paragraph explaining why a thing is absent teaches everyone to stop
/// writing the paragraph.
#[must_use]
pub fn mask_comments(text: &str) -> String {
    mask(text, true)
}

/// The shared scanner behind both maskings.
fn mask(text: &str, keep_strings: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = vec![b' '; bytes.len()];
    let mut i = 0;
    // Keep a byte as it stands; anything not kept stays a space.
    let keep = |out: &mut Vec<u8>, at: usize, byte: u8| {
        if let Some(slot) = out.get_mut(at) {
            *slot = if byte == b'\n' { b'\n' } else { byte };
        }
    };
    while let Some(&b) = bytes.get(i) {
        let next = bytes.get(i + 1).copied();
        // Line comment: to end of line, newlines preserved by the fill.
        if b == b'/' && next == Some(b'/') {
            while bytes.get(i).is_some_and(|b| *b != b'\n') {
                i += 1;
            }
            continue;
        }
        // Block comment, nested as Rust allows.
        if b == b'/' && next == Some(b'*') {
            let mut depth = 1_usize;
            i += 2;
            while depth > 0 {
                let Some(&here) = bytes.get(i) else { break };
                if here == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if here == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    if here == b'\n' {
                        keep(&mut out, i, b'\n');
                    }
                    i += 1;
                }
            }
            continue;
        }
        // A string literal, raw or ordinary: copied whole when the caller
        // wants literals kept, and otherwise blanked with its newlines left
        // where they were so line numbers still hold.
        let literal = raw_string_len(bytes, i).or_else(|| ordinary_string_len(bytes, i));
        if let Some(len) = literal {
            for j in i..i + len {
                match bytes.get(j) {
                    Some(&byte) if keep_strings || byte == b'\n' => keep(&mut out, j, byte),
                    _ => {}
                }
            }
            i += len;
            continue;
        }
        // A character literal is masked; a lifetime is code and is kept.
        if b == b'\'' {
            if let Some(len) = char_literal_len(bytes, i) {
                i += len;
                continue;
            }
        }
        keep(&mut out, i, b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Byte length of the raw string literal starting at `at`, if one does.
fn raw_string_len(bytes: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    if bytes.get(i) == Some(&b'b') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    // `r` must open a token, not end an identifier such as `_r`.
    if at > 0 && bytes.get(at - 1).is_some_and(|p| is_ident_byte(*p)) {
        return None;
    }
    i += 1;
    let hash_start = i;
    while bytes.get(i) == Some(&b'#') {
        i += 1;
    }
    let hashes = i - hash_start;
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    let closer: Vec<u8> = std::iter::once(b'"')
        .chain(std::iter::repeat_n(b'#', hashes))
        .collect();
    while i < bytes.len() {
        if bytes.get(i..i + closer.len()) == Some(closer.as_slice()) {
            return Some(i + closer.len() - at);
        }
        i += 1;
    }
    Some(bytes.len() - at)
}

/// Byte length of the ordinary string literal starting at `at`, if one does.
fn ordinary_string_len(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'"') {
        return None;
    }
    let mut i = at + 1;
    while let Some(&here) = bytes.get(i) {
        if here == b'\\' {
            i += 2;
            continue;
        }
        i += 1;
        if here == b'"' {
            break;
        }
    }
    Some(i - at)
}

/// Byte length of the character literal starting at `at`, or `None` for a
/// lifetime.
fn char_literal_len(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at + 1) == Some(&b'\\') {
        let mut i = at + 2;
        while bytes.get(i).is_some_and(|b| *b != b'\'') {
            i += 1;
        }
        return Some(i + 1 - at);
    }
    // `'x'` — one character then a closing quote. Anything else is a lifetime.
    let mut i = at + 1;
    while bytes.get(i).is_some_and(|b| !b.is_ascii()) {
        i += 1;
    }
    if i < bytes.len() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'\'') {
        return Some(i + 1 - at);
    }
    None
}

/// Whether `b` can appear inside a Rust identifier.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether the occurrence of `needle` at `at` in `hay` stands as its own token
/// rather than inside a longer identifier.
fn is_whole_token(hay: &str, at: usize, needle: &str) -> bool {
    let bytes = hay.as_bytes();
    let before = at.checked_sub(1).and_then(|i| bytes.get(i));
    let after = bytes.get(at + needle.len());
    let opens = needle.as_bytes().first().is_some_and(|b| is_ident_byte(*b));
    let closes = needle.as_bytes().last().is_some_and(|b| is_ident_byte(*b));
    (!opens || !before.is_some_and(|b| is_ident_byte(*b)))
        && (!closes || !after.is_some_and(|b| is_ident_byte(*b)))
}

/// Byte offsets of every whole-token occurrence of `needle` in `hay`.
fn token_offsets(hay: &str, needle: &str) -> Vec<usize> {
    hay.match_indices(needle)
        .filter(|(at, _)| is_whole_token(hay, *at, needle))
        .map(|(at, _)| at)
        .collect()
}

/// A function as this screen sees it: a name and the masked text of its body.
#[derive(Debug)]
pub struct FnBody {
    /// The function's own name, with no module or type qualification.
    pub name: String,
    /// `path:line` of the definition, for a finding that names its site.
    pub site: String,
    /// The masked body, braces included.
    pub body: String,
}

/// Skip a balanced bracket run starting at `at` (which must be the opener),
/// returning the offset just past its closer.
fn skip_balanced(bytes: &[u8], at: usize, open: u8, close: u8) -> usize {
    let mut depth = 0_usize;
    let mut i = at;
    while let Some(&b) = bytes.get(i) {
        // `->` inside a generic bound is an arrow, not a closing angle.
        if open == b'<' && b == b'-' && bytes.get(i + 1) == Some(&b'>') {
            i += 2;
            continue;
        }
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    bytes.len()
}

/// Every function defined in `masked`, with its body.
///
/// `masked` must already have comments and literals removed, so every brace it
/// contains is a real one. A declaration with no body (a trait method) is
/// skipped rather than guessed at.
#[must_use]
pub fn fn_bodies(rel: &str, masked: &str) -> Vec<FnBody> {
    let bytes = masked.as_bytes();
    let mut out = Vec::new();
    for at in token_offsets(masked, "fn ") {
        let mut i = at + 3;
        while bytes.get(i) == Some(&b' ') {
            i += 1;
        }
        let name_start = i;
        while bytes.get(i).is_some_and(|b| is_ident_byte(*b)) {
            i += 1;
        }
        if i == name_start {
            continue;
        }
        let name = masked.get(name_start..i).unwrap_or("").to_owned();
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes.get(i) == Some(&b'<') {
            i = skip_balanced(bytes, i, b'<', b'>');
        }
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes.get(i) != Some(&b'(') {
            continue;
        }
        i = skip_balanced(bytes, i, b'(', b')');
        // Return type and where-clause, up to the body or the semicolon that
        // says there is none.
        while bytes.get(i).is_some_and(|b| *b != b'{' && *b != b';') {
            i += 1;
        }
        if bytes.get(i) != Some(&b'{') {
            continue;
        }
        let end = skip_balanced(bytes, i, b'{', b'}');
        out.push(FnBody {
            name,
            site: format!("{rel}:{}", line_of(masked, at).saturating_sub(1)),
            body: masked.get(i..end).unwrap_or("").to_owned(),
        });
    }
    out
}

/// Whether the identifier at `start` is qualified by a preceding path segment
/// that names a type rather than a module.
fn is_associated_on_a_type(bytes: &[u8], start: usize) -> bool {
    if start < 2 || bytes.get(start - 1) != Some(&b':') || bytes.get(start - 2) != Some(&b':') {
        return false;
    }
    let mut at = start - 2;
    while at > 0 && bytes.get(at - 1).is_some_and(|b| is_ident_byte(*b)) {
        at -= 1;
    }
    bytes.get(at).is_some_and(u8::is_ascii_uppercase)
}

/// Every function in `defined` that `body` CALLS.
///
/// A call, not a mention: the name is followed by `(` (or a turbofish and then
/// `(`) and is not preceded by `.`. Dropping method position is what keeps the
/// walk usable — this codebase has free functions named `open`, `serve` and
/// `gather`, and counting every `.open(` on every unrelated type as an edge
/// reached nine tenths of the binary from one hook and told a reader nothing.
/// A path-qualified call (`crate::thing::open(`) still counts, so the
/// tightening loses no free call.
#[must_use]
pub fn calls_within(body: &str, defined: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes.get(i).copied().is_some_and(is_ident_byte) {
            i += 1;
            continue;
        }
        let start = i;
        while bytes.get(i).copied().is_some_and(is_ident_byte) {
            i += 1;
        }
        let Some(word) = body.get(start..i) else {
            continue;
        };
        if !defined.contains(word) {
            continue;
        }
        if start > 0 && bytes.get(start - 1) == Some(&b'.') {
            continue;
        }
        // An associated function on a TYPE (`File::open`, `SqliteStore::open`)
        // is not the free function of the same name, and treating it as one is
        // what turned this walk into most of the binary. A module-qualified
        // call (`crate::family::recall_db`) still counts: module segments are
        // lower case, type names are not.
        if is_associated_on_a_type(bytes, start) {
            continue;
        }
        let mut at = i;
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        // A turbofish stands between the name and its argument list.
        if bytes.get(at) == Some(&b':') && bytes.get(at + 1) == Some(&b':') {
            at += 2;
            while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            if bytes.get(at) == Some(&b'<') {
                at = skip_balanced(bytes, at, b'<', b'>');
            }
        }
        if bytes.get(at) == Some(&b'(') {
            out.insert(word.to_owned());
        }
    }
    out
}

// -------------------------------------------------- leg: one chokepoint ---

/// The literals a hook response is made of. A file that spells one of these is
/// either the chokepoint, a test, a pinned artifact, or a violation.
const RESPONSE_LITERALS: [&str; 5] = [
    "permissionDecision",
    "\"decision\"",
    "updatedInput",
    "injectSteps",
    "hookSpecificOutput",
];

/// The one file entitled to compose an agent's hook response.
const CHOKEPOINT: &str = "crates/rift-agent/src/respond/mod.rs";

/// Where the exemptions live.
const CHOKEPOINT_ALLOW: &str = "ci/intercept-chokepoint-allow.txt";

/// One allow-list entry: a site that spells a response literal for an
/// unrelated reason.
#[derive(Debug)]
struct Allowed {
    path: String,
    literal: String,
    count: usize,
    seen: usize,
}

/// Parse the allow-list's text.
fn read_allow_list(text: &str) -> Result<Vec<Allowed>, String> {
    let mut out = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
        let [path, literal, count, reason] = fields.as_slice() else {
            return Err(format!(
                "{CHOKEPOINT_ALLOW}:{}: expected `path | literal | count | reason`",
                index + 1
            ));
        };
        let count: usize = count
            .parse()
            .map_err(|_| format!("{CHOKEPOINT_ALLOW}:{}: `{count}` is not a count", index + 1))?;
        if reason.len() < 20 {
            return Err(format!(
                "{CHOKEPOINT_ALLOW}:{}: the reason must say why, in a sentence",
                index + 1
            ));
        }
        out.push(Allowed {
            path: (*path).to_owned(),
            literal: (*literal).to_owned(),
            count,
            seen: 0,
        });
    }
    Ok(out)
}

/// **One file composes an agent's hook response.**
///
/// The screen: in the production half of every tracked `.rs` file under
/// `crates/` and `bins/` other than the chokepoint, no literal from
/// `RESPONSE_LITERALS` may appear. Integration tests, benches and pinned
/// fixtures are outside the corpus — a test asserting the bytes an agent
/// receives has to name them, and a fixture is the artifact those bytes were
/// pinned from.
///
/// Comments and string bodies are NOT masked here, deliberately and unlike the
/// other legs: what this leg is looking for IS a string literal, and a response
/// field named in a comment beside the code that writes it is exactly the shape
/// worth reading before it becomes code.
///
/// An entry in `CHOKEPOINT_ALLOW` exempts an exact count of one literal in
/// one file. A count that no longer matches — in either direction — is a
/// violation, so the list shrinks with the sites it covers.
///
/// # Errors
/// If the corpus or the allow-list cannot be read.
pub fn one_gate_chokepoint(root: &Path) -> Result<Leg, String> {
    let allow = std::fs::read_to_string(root.join(CHOKEPOINT_ALLOW))
        .map_err(|e| format!("read {CHOKEPOINT_ALLOW}: {e}"))?;
    screen_chokepoint(&rust_corpus(root)?, &allow)
}

/// The screen over an explicit corpus and allow-list text — the seam a shape
/// test plants its violation through, so a test needs no repository of its own
/// (the same seam, for the same reason, as the retired-vocabulary screen's).
fn screen_chokepoint(corpus: &[(String, String)], allow_text: &str) -> Result<Leg, String> {
    let mut allow = read_allow_list(allow_text)?;
    let mut v = Vec::new();
    let mut allowed_sites = 0_usize;
    for (rel, text) in corpus {
        if rel == CHOKEPOINT || is_test_surface(rel) {
            continue;
        }
        let production = production_half(text);
        for literal in RESPONSE_LITERALS {
            let hits: Vec<usize> = production
                .match_indices(literal)
                .map(|(at, _)| at)
                .collect();
            if hits.is_empty() {
                continue;
            }
            if let Some(entry) = allow
                .iter_mut()
                .find(|a| a.path == *rel && a.literal == literal)
            {
                entry.seen = hits.len();
                allowed_sites += hits.len();
                if entry.count == hits.len() {
                    continue;
                }
                v.push(format!(
                    "{rel}: `{literal}` occurs {} time(s), but {CHOKEPOINT_ALLOW} allows {} — \
                     re-read the site and correct the count, or move the code into {CHOKEPOINT}",
                    hits.len(),
                    entry.count
                ));
                continue;
            }
            for at in hits {
                v.push(format!(
                    "{rel}:{}: `{literal}` outside {CHOKEPOINT} — compose the response there, \
                     or, if this is not a hook response, say so in {CHOKEPOINT_ALLOW}",
                    // The production half is prefixed with one newline by the
                    // horizon split, so its offsets are one line ahead.
                    line_of(&production, at).saturating_sub(1)
                ));
            }
        }
    }
    for entry in &allow {
        if entry.seen == 0 {
            v.push(format!(
                "{CHOKEPOINT_ALLOW}: `{}` in {} no longer occurs — delete the entry",
                entry.literal, entry.path
            ));
        }
    }
    Ok(Leg::new(
        "one-gate-chokepoint",
        format!(
            "{} files screened; {CHOKEPOINT} is the only composer; {allowed_sites} allow-listed \
             site(s) spell a response literal for an unrelated reason",
            corpus.len()
        ),
        v,
    ))
}

// ----------------------------------------------------- leg: pinned channels ---

/// Where the reviewable channel list lives.
const CHANNELS_FILE: &str = "ci/intercept-channels.txt";

/// The fence a pinned output-surface artifact keeps its capability rows in.
const CAPABILITY_FENCE: &str = "```rift-capability-rows";

/// The rows `ci/intercept-channels.txt` states, comments and blanks dropped.
fn stated_channel_rows(root: &Path) -> Result<BTreeSet<String>, String> {
    let text = std::fs::read_to_string(root.join(CHANNELS_FILE))
        .map_err(|e| format!("read {CHANNELS_FILE}: {e}"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

/// The rows every pinned output-surface artifact carries, each prefixed with
/// the family its adapter crate is named for.
fn pinned_channel_rows(root: &Path) -> Result<(BTreeSet<String>, usize), String> {
    let mut rows = BTreeSet::new();
    let mut fixtures = 0_usize;
    for rel in tracked_paths(root)? {
        let Some(name) = rel.rsplit('/').next() else {
            continue;
        };
        if !name.ends_with("-hook-output-surface.md") {
            continue;
        }
        let family = rel
            .split('/')
            .nth(1)
            .and_then(|c| c.strip_prefix("rift-agent-"))
            .ok_or_else(|| format!("{rel}: not inside an adapter crate"))?
            .to_owned();
        let text = std::fs::read_to_string(root.join(&rel))
            .map_err(|e| format!("read tracked file {rel}: {e}"))?;
        fixtures += 1;
        let mut inside = false;
        for line in text.lines() {
            let line = line.trim_end();
            if inside {
                if line == "```" {
                    inside = false;
                } else if !line.trim().is_empty() {
                    rows.insert(format!("family={family} {}", line.trim()));
                }
            } else if line == CAPABILITY_FENCE {
                inside = true;
            }
        }
    }
    if fixtures == 0 {
        return Err("gate-intercept: no pinned output-surface artifact is tracked".to_owned());
    }
    Ok((rows, fixtures))
}

/// **The channels Rift answers on are a reviewable list.**
///
/// `ci/intercept-channels.txt` must equal, as a set, the capability rows fenced
/// in every tracked `*-hook-output-surface.md`.
///
/// The chain this closes, and where each link is proven: the code table
/// (`rift_agent::respond::CAPABILITIES`) equals its family's fixture rows —
/// proven by each adapter's own `pinned_output_surface` test, which renders the
/// table and compares both directions — and the fixture rows equal this file,
/// proven here. So a widened channel cannot reach the tree without touching the
/// artifact AND this list, and the list is one place a reviewer can read.
///
/// The limit, stated: this leg does not parse `CAPABILITIES` itself. It reads
/// the artifacts, which are the authority the table is checked against. If the
/// adapter tests were deleted the chain would lose its middle link, which is
/// why those tests are named in the program record rather than left implicit.
///
/// # Errors
/// If the list or an artifact cannot be read, or no artifact is tracked.
pub fn pinned_channels(root: &Path) -> Result<Leg, String> {
    let (pinned, fixtures) = pinned_channel_rows(root)?;
    Ok(screen_channels(
        &stated_channel_rows(root)?,
        &pinned,
        fixtures,
    ))
}

/// The comparison itself, over two row sets — the seam a shape test uses.
fn screen_channels(stated: &BTreeSet<String>, pinned: &BTreeSet<String>, fixtures: usize) -> Leg {
    let mut v = Vec::new();
    for row in stated.difference(pinned) {
        v.push(format!(
            "{CHANNELS_FILE}: `{row}` is stated here but no pinned artifact carries it"
        ));
    }
    for row in pinned.difference(stated) {
        v.push(format!(
            "{CHANNELS_FILE}: a pinned artifact carries `{row}` and this list does not — \
             a widened channel is reviewed here or it is not reviewed"
        ));
    }
    Leg::new(
        "pinned-channels",
        format!(
            "{} row(s) across {fixtures} pinned artifact(s) match {CHANNELS_FILE}",
            pinned.len()
        ),
        v,
    )
}

// --------------------------------------------------- leg: no store on path ---

/// The entry points a decision about a tool call is made from, end to end:
/// the guarded step the hook runs first, the gate inside it, the record kept
/// of what the agent itself wrote, the policy and step questions each asks,
/// and the composition of the answer.
///
/// Named rather than inferred, and deliberately NOT the whole hook: the hook
/// that runs these also runs the capture ingest afterwards, which opens a
/// store because that is its job. The decision is the part that answers while
/// the agent waits, and it is the part this leg walks.
///
/// A root that no longer exists is a hard error, not a smaller walk.
const GATE_ROOTS: [&str; 11] = [
    "window_journal_step",
    "read_gate",
    "doc_gate",
    "doc_gate_in",
    "document_verify",
    "edit_record",
    "gate_policy",
    "gate_step",
    "edit_step",
    "gate",
    "respond_with_decision",
];

/// What must not be reachable from a decision, and why each one matters.
///
/// The first two open a projection and the fourth is the store type itself;
/// the third names the recall projection, which nothing on this path has any
/// business reaching for; the fifth reads a revision's bytes back out of a
/// store; the sixth opens the git object database; the seventh is the
/// tokenizer. A decision has a hook's latency budget and the agent waiting on
/// it, so every one of these belongs to the worker that runs afterwards.
///
/// The eighth and ninth are the in-process document parsers. They are linked
/// into the binary, and a decision may still only reach them through the
/// child the binary re-spawns (`doc_helper`): a parser wedged on a hostile
/// file inside the hook process could not be killed, and a container that
/// decompresses past memory would take the hook with it.
const FORBIDDEN_ON_GATE_PATH: [&str; 9] = [
    "open_migrated",
    "open_capture_projection",
    "recall_db",
    "SqliteStore::",
    "basis_bytes",
    "GixSubstrate::open",
    "rift_tokens::",
    "anydoc::",
    "AnydocConverter",
];

/// The crates a decision's call graph is walked through.
const GATE_CRATES: [&str; 2] = ["bins/rift/src/", "crates/rift-agent/src/"];

/// **Nothing on the decision path opens a store or loads the tokenizer.**
///
/// The walk: every function defined in the production half of `bins/rift/src/`
/// and `crates/rift-agent/src/`, resolved by bare name; breadth-first from
/// `GATE_ROOTS`; every reachable body screened for `FORBIDDEN_ON_GATE_PATH`.
///
/// **What this catches and what it cannot.** Names are resolved without types
/// or modules, so two functions sharing a name are one node and the walk is
/// WIDER than the real call graph — the conservative direction for a gate, and
/// the reason a finding here deserves reading before it is dismissed. A call
/// made through a trait object or a stored function pointer is not followed;
/// what covers that gap is the second half of the screen, which looks for the
/// forbidden names as MENTIONS rather than as edges, so a call into another
/// crate (`basis_bytes` lives in one) is caught at the site that writes it even
/// though the walk does not enter that crate. What neither half sees is a
/// forbidden operation reached under a name none of these seven spell; that is
/// a gate on the vocabulary, and the vocabulary is pinned by the list above.
///
/// # Errors
/// If the corpus cannot be read, or a named root no longer exists.
pub fn no_store_on_gate_path(root: &Path) -> Result<Leg, String> {
    let corpus: Vec<(String, String)> = rust_corpus(root)?
        .into_iter()
        .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
        .collect();
    if corpus.is_empty() {
        return Err("gate-intercept: no source under the crates a decision is made in".to_owned());
    }
    Ok(screen_gate_path(&corpus)?.0)
}

/// The walk over an explicit corpus, returning the leg AND the names it
/// reached — the seam a planted call is proven through, and the one a test
/// uses to check the walk still arrives where it should. A tightening that
/// quietly empties the walk is a green gate that guards nothing, so the
/// waypoints are asserted rather than assumed.
fn screen_gate_path(corpus: &[(String, String)]) -> Result<(Leg, BTreeSet<String>), String> {
    let mut fns: Vec<FnBody> = Vec::new();
    for (rel, text) in corpus {
        fns.extend(fn_bodies(rel, &mask_code(&production_half(text))));
    }
    let defined: BTreeSet<String> = fns.iter().map(|f| f.name.clone()).collect();
    for root_fn in GATE_ROOTS {
        if !defined.contains(root_fn) {
            return Err(format!(
                "gate-intercept: `{root_fn}` is named as a decision entry point and no longer \
                 exists — re-read the hook and correct the list, never shrink the walk silently"
            ));
        }
    }
    // Breadth-first over bare names. One name can have several definitions;
    // every one of them is walked, which is the wide direction.
    let mut by_name: BTreeMap<&str, Vec<&FnBody>> = BTreeMap::new();
    for f in &fns {
        by_name.entry(f.name.as_str()).or_default().push(f);
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for root_fn in GATE_ROOTS {
        if seen.insert(root_fn) {
            queue.push_back(root_fn);
        }
    }
    let mut v = Vec::new();
    let mut reached = 0_usize;
    while let Some(name) = queue.pop_front() {
        for f in by_name.get(name).into_iter().flatten() {
            reached += 1;
            for needle in FORBIDDEN_ON_GATE_PATH {
                if !token_offsets(&f.body, needle).is_empty() {
                    v.push(format!(
                        "{}: `{}` reaches `{needle}` — a decision runs inside a hook, with the \
                         agent waiting on it; leave this to the worker that runs afterwards",
                        f.site, f.name
                    ));
                }
            }
            for callee in calls_within(&f.body, &defined) {
                if let Some(next) = by_name.keys().find(|k| ***k == *callee) {
                    if seen.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
        }
    }
    Ok((
        Leg::new(
            "no-store-on-gate-path",
            format!(
                "{} function(s) reachable from {} decision entry point(s), out of {} defined; \
                 none opens a store or loads the tokenizer",
                reached,
                GATE_ROOTS.len(),
                fns.len()
            ),
            v,
        ),
        seen.iter().map(|n| (*n).to_owned()).collect(),
    ))
}

// -------------------------------------------------- leg: one reason renderer ---

/// Where every sentence a model reads from Rift is written.
const RENDERER: &str = "crates/rift-recall-validity/src/trust.rs";

/// The phrases the renderer opens its lines with, read out of the renderer
/// itself rather than retyped here.
///
/// Retyping them would make this gate a second copy to keep in step, and a
/// stale second copy guards the wording it remembers instead of the wording
/// that ships. The declarations are `const FRAME_<something>_OPENER: &str =
/// "…";` and the parse is deliberately dumb.
fn frame_openers(root: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(root.join(RENDERER))
        .map_err(|e| format!("read {RENDERER}: {e}"))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("const FRAME_") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(": &str = ") else {
            continue;
        };
        if !name.ends_with("_OPENER") {
            continue;
        }
        let phrase = value.trim().trim_end_matches(';').trim_matches('"');
        if !phrase.is_empty() {
            out.push(phrase.to_owned());
        }
    }
    if out.len() < 5 {
        return Err(format!(
            "gate-intercept: only {} frame opener(s) parsed out of {RENDERER} — the declarations \
             changed shape and this leg would guard almost nothing",
            out.len()
        ));
    }
    Ok(out)
}

/// **One module writes the sentences a model reads.**
///
/// Every frame Rift pushes into a model's context is refused if it can be
/// forged, counted honestly, and held to its own bytes by goldens — all of that
/// inside one module. A second place that opens a line with one of the same
/// phrases inherits none of it, and it is indistinguishable to a reader (and to
/// a model) from the real thing.
///
/// So: no production code outside the renderer may spell one of its openers.
/// Comments are masked and string literals are not, because a phrase written
/// into an output IS a string literal, while a comment quoting one is how this
/// codebase explains itself.
///
/// # Errors
/// If the corpus or the renderer cannot be read, or the openers stop parsing.
pub fn one_reason_renderer(root: &Path) -> Result<Leg, String> {
    Ok(screen_renderer(&rust_corpus(root)?, &frame_openers(root)?))
}

/// The screen over an explicit corpus and opener list — the seam a planted
/// sentence is proven through.
fn screen_renderer(corpus: &[(String, String)], openers: &[String]) -> Leg {
    let mut v = Vec::new();
    let mut screened = 0_usize;
    for (rel, text) in corpus {
        if rel == RENDERER || is_test_surface(rel) {
            continue;
        }
        screened += 1;
        let production = mask_comments(&production_half(text));
        for opener in openers {
            for at in production.match_indices(opener.as_str()).map(|(at, _)| at) {
                v.push(format!(
                    "{rel}:{}: writes `{opener}`, a line only {RENDERER} may open — a frame \
                     written anywhere else carries none of the refusals that make it safe",
                    line_of(&production, at).saturating_sub(1)
                ));
            }
        }
    }
    Leg::new(
        "one-reason-renderer",
        format!(
            "{} opening phrase(s) declared in {RENDERER}; {screened} other file(s) write none",
            openers.len()
        ),
        v,
    )
}

// ---------------------------------------------------- leg: closed rewrite forms ---

/// The two amendments Rift may make to a tool call, and the only fields either
/// one names.
///
/// `tool_input` is the call as the agent wrote it, carried so the amendment can
/// be built from it; the other two are the amendments themselves. A third field
/// name here would be a third thing Rift can change about a call it did not
/// make, which is the change this leg exists to make impossible to land
/// quietly.
const REWRITE_FIELDS: [&str; 3] = ["tool_input", "file_path", "head_limit"];

/// The two additive forms, spelled as the enum variants that carry them.
const REWRITE_VARIANTS: [&str; 2] = ["DocumentSidecar", "GrepHeadLimit"];

/// Every form the response capability table may name: two additive field
/// amendments and one bounded replacement of a strict redundant shell read.
const RESPONSE_REWRITE_FORMS: [&str; 3] = [
    "DocumentSidecarPath",
    "GrepHeadLimit",
    "ShellReadReplacement",
];

/// The field names a struct-literal or pattern names between `{` and `}`: an
/// identifier followed by `:` (but not `::`) at the top level of the braces.
fn named_fields(masked: &str, open_brace: usize) -> Vec<String> {
    let bytes = masked.as_bytes();
    let end = skip_balanced(bytes, open_brace, b'{', b'}');
    let mut out = Vec::new();
    let mut depth = 0_usize;
    let mut i = open_brace;
    while i < end {
        let Some(&b) = bytes.get(i) else { break };
        match b {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth != 1
            || !is_ident_byte(b)
            || bytes
                .get(i.wrapping_sub(1))
                .is_some_and(|p| is_ident_byte(*p))
        {
            i += 1;
            continue;
        }
        let start = i;
        while bytes.get(i).copied().is_some_and(is_ident_byte) {
            i += 1;
        }
        let mut at = i;
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if bytes.get(at) == Some(&b':') && bytes.get(at + 1) != Some(&b':') {
            if let Some(word) = masked.get(start..i) {
                out.push(word.to_owned());
            }
        }
    }
    out
}

/// The variants and payload fields the amendment enum DECLARES, if `masked`
/// declares it.
///
/// A construction site is written `Rewrite::DocumentSidecar { … }` and a
/// declaration is not, so the two need separate readings. Without this one, an
/// unreachable third variant could sit in the enum indefinitely, waiting for a
/// caller — declared capability is capability.
fn declared_rewrite_forms(rel: &str, masked: &str) -> Vec<String> {
    let bytes = masked.as_bytes();
    let mut v = Vec::new();
    for at in token_offsets(masked, "enum Rewrite") {
        let Some(open) = masked
            .get(at..)
            .and_then(|rest| rest.find('{'))
            .map(|i| at + i)
        else {
            continue;
        };
        let end = skip_balanced(bytes, open, b'{', b'}');
        let mut depth = 0_usize;
        let mut i = open;
        while i < end {
            let Some(&b) = bytes.get(i) else { break };
            match b {
                b'{' | b'(' | b'[' => depth += 1,
                b'}' | b')' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth != 1
                || !is_ident_byte(b)
                || bytes
                    .get(i.wrapping_sub(1))
                    .is_some_and(|p| is_ident_byte(*p))
            {
                i += 1;
                continue;
            }
            let start = i;
            while bytes.get(i).copied().is_some_and(is_ident_byte) {
                i += 1;
            }
            let Some(variant) = masked.get(start..i) else {
                continue;
            };
            let line = line_of(masked, start);
            if !REWRITE_VARIANTS.contains(&variant) {
                v.push(format!(
                    "{rel}:{line}: the amendment enum declares `{variant}` — a declared third \
                     form is a third form, caller or no caller"
                ));
                continue;
            }
            let mut payload = i;
            while bytes.get(payload).is_some_and(u8::is_ascii_whitespace) {
                payload += 1;
            }
            if bytes.get(payload) != Some(&b'{') {
                continue;
            }
            for field in named_fields(masked, payload) {
                if !REWRITE_FIELDS.contains(&field.as_str()) {
                    v.push(format!(
                        "{rel}:{line}: `{variant}` declares `{field}` — an amendment carries the \
                         call and the one field its form is named for"
                    ));
                }
            }
        }
    }
    v
}

/// **A tool-call rewrite is one of three pinned forms, never an open-ended edit.**
///
/// Two screens over the masked corpus, tests included — a test that builds a
/// third form is a third form that exists:
///
/// 1. every `Rewrite::<Variant> { … }` names a variant from
///    `REWRITE_VARIANTS` and fields from `REWRITE_FIELDS`;
/// 2. every `RewriteForm::<Variant>` names one of the three pinned response forms;
/// 3. the additive `Rewrite` enum's own declaration names its two variants and
///    no others, with those field names and no others — read separately,
///    because a declaration is not written in the shape a construction site.
///
/// # Errors
/// If the corpus cannot be read, or neither form is found anywhere (a screen
/// over a vocabulary that has vanished is a screen that guards nothing).
pub fn closed_rewrite_forms(root: &Path) -> Result<Leg, String> {
    screen_rewrites(&rust_corpus(root)?)
}

/// The screen over an explicit corpus — the seam a planted third form is
/// proven through.
fn screen_rewrites(corpus: &[(String, String)]) -> Result<Leg, String> {
    let mut v = Vec::new();
    let mut sites = 0_usize;
    for (rel, text) in corpus {
        let masked = mask_code(text);
        // The amendment type lives in `rift-agent`, so only a file inside that
        // crate or one that names it can build one. Without this, an unrelated
        // `Rewrite` in another crate's test support answers to the same two
        // words and the leg reports on a type it has no business judging.
        if !rel.starts_with("crates/rift-agent") && !masked.contains("rift_agent") {
            continue;
        }
        v.extend(declared_rewrite_forms(rel, &masked));
        let bytes = masked.as_bytes();
        for at in token_offsets(&masked, "Rewrite::") {
            let mut i = at + "Rewrite::".len();
            let start = i;
            while bytes.get(i).copied().is_some_and(is_ident_byte) {
                i += 1;
            }
            let Some(variant) = masked.get(start..i) else {
                continue;
            };
            sites += 1;
            let line = line_of(&masked, at);
            if !REWRITE_VARIANTS.contains(&variant) {
                v.push(format!(
                    "{rel}:{line}: `Rewrite::{variant}` is a third amendment — Rift changes a \
                     document read's path or adds a search's missing cap, and nothing else"
                ));
                continue;
            }
            while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            if bytes.get(i) != Some(&b'{') {
                continue;
            }
            for field in named_fields(&masked, i) {
                if !REWRITE_FIELDS.contains(&field.as_str()) {
                    v.push(format!(
                        "{rel}:{line}: `Rewrite::{variant}` names `{field}` — an amendment \
                         touches the one field its form is named for"
                    ));
                }
            }
        }
        for at in token_offsets(&masked, "RewriteForm::") {
            let mut i = at + "RewriteForm::".len();
            let start = i;
            while bytes.get(i).copied().is_some_and(is_ident_byte) {
                i += 1;
            }
            let Some(form) = masked.get(start..i) else {
                continue;
            };
            sites += 1;
            if !RESPONSE_REWRITE_FORMS.contains(&form) {
                v.push(format!(
                    "{rel}:{}: `RewriteForm::{form}` is not a pinned rewrite form",
                    line_of(&masked, at)
                ));
            }
        }
    }
    if sites == 0 {
        return Err("gate-intercept: no amendment site found anywhere in the tree".to_owned());
    }
    Ok(Leg::new(
        "closed-rewrite-forms",
        format!("{sites} rewrite site(s), all of them one of the three pinned forms"),
        v,
    ))
}

// ----------------------------------------------------- leg: family-keyed caches ---

/// The caches this program adds, as they appear in a path expression.
const MEMBER_CACHES: [&str; 2] = [".rift/state/doc", ".rift/state/search"];

/// The family's own directory in the git common dir, as a path literal and as
/// the constant the code that joins it usually names instead.
const FAMILY_STORE: &str = "rift-family";

/// **A cache is keyed by the family, never by which checkout is open.**
///
/// A linked worktree is a member of its repository's family, and every durable
/// root — the projection, the recall store, the binary of record, the registry
/// row — is keyed by the family's minted identity in the common directory. A
/// cache keyed by a checkout path instead does not merely miss: it silently
/// splits, so a temporary worktree's work never compounds with anything and the
/// saving looks like a smaller number rather than a bug.
///
/// Two rules, both over function bodies in production halves, with comments
/// masked and path literals kept:
///
/// 1. a body that builds one of `MEMBER_CACHES` does not also derive an
///    identity from a checkout (`repo_id_for`) — the per-member caches live
///    under the member's own `.rift/state/`, keyed by nothing;
/// 2. a body that builds the `FAMILY_STORE` directory names a `common_dir` —
///    the family store is reached from the common directory or it is not the
///    family store.
///
/// The limits: this reads names, so a body that hands a checkout path to a
/// helper which then joins the cache is split across two bodies and seen by
/// neither rule. What it does catch is the shape the mistake actually takes,
/// which is both halves written together.
///
/// # Errors
/// If the corpus cannot be read.
pub fn family_keyed_caches(root: &Path) -> Result<Leg, String> {
    Ok(screen_family_caches(&rust_corpus(root)?))
}

/// The screen over an explicit corpus — the seam both plantings go through.
fn screen_family_caches(corpus: &[(String, String)]) -> Leg {
    let mut v = Vec::new();
    let mut cache_sites = 0_usize;
    let mut family_sites = 0_usize;
    for (rel, text) in corpus {
        if is_test_surface(rel) {
            continue;
        }
        for f in fn_bodies(rel, &mask_comments(&production_half(text))) {
            for cache in MEMBER_CACHES {
                if !f.body.contains(cache) {
                    continue;
                }
                cache_sites += 1;
                if !token_offsets(&f.body, "repo_id_for").is_empty() {
                    v.push(format!(
                        "{}: `{}` builds `{cache}` and derives an id from a checkout — a member \
                         cache lives under that member's own state directory, and an id comes \
                         from the family",
                        f.site, f.name
                    ));
                }
            }
            let builds_family_store =
                f.body.contains(FAMILY_STORE) || !token_offsets(&f.body, "FAMILY_DIR").is_empty();
            if !builds_family_store {
                continue;
            }
            family_sites += 1;
            if !f.body.contains("common_dir") {
                v.push(format!(
                    "{}: `{}` builds the family store without a common directory — a family's \
                     store is one directory shared by every member, reached from the common dir",
                    f.site, f.name
                ));
            }
        }
    }
    Leg::new(
        "family-keyed-caches",
        format!(
            "{cache_sites} member-cache site(s) derive no id from a checkout; {family_sites} \
             family-store site(s) are reached from a common directory{}",
            if cache_sites == 0 {
                " (no member cache is built yet, so that half is a standing guard on work \
                 that has not landed)"
            } else {
                ""
            }
        ),
        v,
    )
}

// --------------------------------------------------------- leg: not built ---

/// **Not built, and this is the honest report of why.**
///
/// The plan for this gate carried a seventh leg that would have checked a
/// registry file listing each load-bearing test by name. The owner ruled that
/// the program's registers are its own chronology and its decision records, and
/// that no such file is to be created; the tests are registered by name where
/// the work that added them is recorded.
///
/// The leg is kept, printing that, rather than deleted: a leg that vanishes
/// reads as a leg that passed.
#[must_use]
pub fn load_bearing() -> Leg {
    Leg::new(
        "load-bearing",
        "not built (registers live in the program chronology, by owner decision)".to_owned(),
        Vec::new(),
    )
}

// ------------------------------------------------------------- the command ---

/// Run every leg over the tracked tree.
///
/// # Errors
/// If a leg cannot read what it screens.
pub fn run_all(root: &Path) -> Result<Vec<Leg>, String> {
    Ok(vec![
        one_gate_chokepoint(root)?,
        pinned_channels(root)?,
        no_store_on_gate_path(root)?,
        one_reason_renderer(root)?,
        closed_rewrite_forms(root)?,
        family_keyed_caches(root)?,
        load_bearing(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The gate as it stands on this tree is green.** A gate whose only
    /// evidence is synthetic is a gate nobody has pointed at the real thing.
    #[test]
    fn every_built_leg_holds_on_this_workspace() {
        let legs = run_all(&crate::workspace_root()).expect("every leg runs");
        for leg in &legs {
            assert!(
                leg.violations.is_empty(),
                "{}: {:#?}",
                leg.name,
                leg.violations
            );
        }
        assert_eq!(legs.len(), 7, "every leg reports, built or not");
    }

    /// A corpus entry, spelled the way a caller would.
    fn file(rel: &str, text: &str) -> (String, String) {
        (rel.to_owned(), text.to_owned())
    }

    /// The allow-list text every shape test starts from: valid, and empty of
    /// entries, so a test that expects a finding is not accidentally exempted.
    const NO_ALLOWANCES: &str = "# nothing is allowed here\n";

    /// ★ A hook response composed anywhere but the one file is a finding.
    #[test]
    fn a_second_place_that_composes_a_hook_response_is_caught() {
        let corpus = vec![file(
            "bins/rift/src/sneaky.rs",
            "fn answer() -> String {\n    \"hookSpecificOutput\".to_owned()\n}\n",
        )];
        let leg = screen_chokepoint(&corpus, NO_ALLOWANCES).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("bins/rift/src/sneaky.rs:2"),
            "the finding names the line: {:#?}",
            leg.violations
        );
    }

    /// The chokepoint itself, and the test surfaces that have to name the bytes
    /// an agent receives, are outside the corpus.
    #[test]
    fn the_chokepoint_and_the_test_surfaces_are_not_findings() {
        let body = "let _ = \"permissionDecision\";\n";
        let corpus = vec![
            file(CHOKEPOINT, body),
            file("bins/rift/tests/hooks_e2e.rs", body),
            file("crates/rift-agent-codex/fixtures/pinned.rs", body),
        ];
        let leg = screen_chokepoint(&corpus, NO_ALLOWANCES).expect("the screen runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
    }

    /// A file's trailing test module is not production code, and the horizon
    /// this leg uses is the one the other meta-checks use.
    #[test]
    fn a_literal_below_the_test_horizon_is_not_a_finding() {
        let corpus = vec![file(
            "bins/rift/src/thing.rs",
            "fn real() {}\n#[cfg(test)]\nmod tests {\n    const X: &str = \"updatedInput\";\n}\n",
        )];
        let leg = screen_chokepoint(&corpus, NO_ALLOWANCES).expect("the screen runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
    }

    /// ★ An allow-list entry covers an exact count, so a real occurrence added
    /// beside an exempt one is still reported.
    #[test]
    fn an_allow_list_entry_covers_a_count_and_not_a_file() {
        let allow = "bins/rift/src/find.rs | \"decision\" | 1 | \
                     the serialized name of a finding kind, nothing to do with a hook\n";
        let one = vec![file(
            "bins/rift/src/find.rs",
            "fn kind(s: &str) {\n    let _ = \"decision\";\n}\n",
        )];
        assert!(screen_chokepoint(&one, allow)
            .expect("the screen runs")
            .violations
            .is_empty());
        let two = vec![file(
            "bins/rift/src/find.rs",
            "fn kind(s: &str) {\n    let _ = \"decision\";\n    let _ = \"decision\";\n}\n",
        )];
        let leg = screen_chokepoint(&two, allow).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("occurs 2 time(s)"),
            "{:#?}",
            leg.violations
        );
    }

    /// The list shrinks: an entry whose site is gone is itself a finding.
    #[test]
    fn an_allowance_for_a_site_that_no_longer_exists_is_a_finding() {
        let allow = "bins/rift/src/gone.rs | \"decision\" | 1 | \
                     a site that has since been deleted from the tree\n";
        let leg = screen_chokepoint(&[file("bins/rift/src/kept.rs", "fn f() {}\n")], allow)
            .expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(leg.violations[0].contains("no longer occurs"));
    }

    /// An allowance with no reason is not an allowance.
    #[test]
    fn an_allowance_must_say_why() {
        let err = read_allow_list("a.rs | \"decision\" | 1 | because\n")
            .expect_err("a bare word is not a reason");
        assert!(err.contains("say why"), "{err}");
        let err = read_allow_list("a.rs | \"decision\" | 1\n").expect_err("four fields or none");
        assert!(err.contains("expected"), "{err}");
    }

    /// ★ A channel widened in a pinned artifact but not in the reviewable list
    /// is a finding, and so is the reverse.
    #[test]
    fn a_channel_the_reviewable_list_does_not_state_is_caught() {
        let stated: BTreeSet<String> = ["family=codex verb=PreCompact channel=Neutral"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut pinned = stated.clone();
        pinned.insert("family=codex verb=PreToolUse channel=DenyWithReason".to_owned());
        let leg = screen_channels(&stated, &pinned, 1);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(leg.violations[0].contains("and this list does not"));

        let leg = screen_channels(&pinned, &stated, 1);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(leg.violations[0].contains("no pinned artifact carries it"));
    }

    /// The reviewable list is the union of the artifacts, family by family —
    /// checked against the tree rather than against a second copy of itself.
    #[test]
    fn the_reviewable_list_names_every_family_with_a_pinned_artifact() {
        let root = crate::workspace_root();
        let (pinned, fixtures) = pinned_channel_rows(&root).expect("the artifacts parse");
        assert!(fixtures >= 3, "three alpha families are pinned: {fixtures}");
        for family in ["claude-code", "codex", "antigravity"] {
            assert!(
                pinned
                    .iter()
                    .any(|r| r.starts_with(&format!("family={family} "))),
                "{family} has rows"
            );
        }
        assert_eq!(stated_channel_rows(&root).expect("the list parses"), pinned);
    }

    /// Masking removes prose and literal text and moves nothing.
    #[test]
    fn masking_keeps_code_and_drops_everything_else() {
        let src = "let a = \"open_migrated\"; // open_migrated\n/* open_migrated */ f();\n";
        let masked = mask_code(src);
        assert_eq!(masked.len(), src.len(), "offsets survive masking");
        assert_eq!(masked.matches("open_migrated").count(), 0, "{masked}");
        assert!(masked.contains("let a ="), "{masked}");
        assert!(masked.contains("f();"), "{masked}");
        assert_eq!(masked.lines().count(), src.lines().count());
    }

    /// A raw string, a nested block comment and a lifetime are each read as
    /// what they are.
    #[test]
    fn masking_reads_raw_strings_comments_and_lifetimes() {
        let masked = mask_code("let r = r#\"open_migrated\"#; /* a /* b */ c */ let x: &'a u8;\n");
        assert_eq!(masked.matches("open_migrated").count(), 0, "{masked}");
        assert!(
            masked.contains("let x: &'a u8;"),
            "a lifetime is code: {masked}"
        );
        assert!(
            !masked.contains('c'),
            "the nested comment closes once: {masked}"
        );
    }

    /// A body is the braces that belong to it, generics and all.
    #[test]
    fn a_function_body_is_found_through_generics_and_where_clauses() {
        let src =
            "fn outer<F: Fn(u8) -> u8>(f: F) -> u8\nwhere\n    F: Copy,\n{\n    inner(1)\n}\n\
                   fn inner(x: u8) -> u8 { x }\n";
        let found = fn_bodies("a.rs", &mask_code(src));
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["outer", "inner"], "{names:?}");
        assert!(found[0].body.contains("inner(1)"), "{}", found[0].body);
    }

    /// ★ A store opened on the decision path is caught, wherever on it.
    #[test]
    fn a_planted_store_call_on_the_decision_path_is_caught() {
        let clean = vec![(
            "bins/rift/src/hooks/gate.rs".to_owned(),
            "fn window_journal_step() { read_gate(); }\n\
             fn read_gate() { helper(); }\n\
             fn edit_record() {}\nfn gate_policy() {}\nfn gate_step() {}\nfn edit_step() {}\n\
             fn gate() {}\nfn respond_with_decision() {}\n\
             fn doc_gate() {}\nfn doc_gate_in() {}\nfn document_verify() {}\n\
             fn helper() { let _ = 1; }\n"
                .to_owned(),
        )];
        let (leg, reached) = screen_gate_path(&clean).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert!(reached.contains("helper"), "the walk arrives: {reached:?}");

        let planted = vec![(
            clean[0].0.clone(),
            clean[0].1.replace(
                "fn helper() { let _ = 1; }",
                "fn helper() { open_migrated(); }",
            ),
        )];
        let (leg, _) = screen_gate_path(&planted).expect("the walk runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("open_migrated"),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ The in-process parsers are linked into the binary and a decision may
    /// still not call them: only the child the binary re-spawns may. A
    /// planted engine call on the decision path is a finding, by either of
    /// the two spellings a caller could reach it under.
    #[test]
    fn a_planted_in_process_parser_on_the_decision_path_is_caught() {
        let clean = vec![(
            "bins/rift/src/hooks/gate.rs".to_owned(),
            "fn window_journal_step() {}\nfn read_gate() {}\nfn edit_record() {}\n\
             fn gate_policy() {}\nfn gate_step() {}\nfn edit_step() {}\nfn gate() {}\n\
             fn respond_with_decision() {}\n\
             fn doc_gate() { doc_gate_in(); }\nfn doc_gate_in() { sidecar(); }\n\
             fn document_verify() {}\n\
             fn sidecar() { let _ = 1; }\n"
                .to_owned(),
        )];
        let (leg, reached) = screen_gate_path(&clean).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert!(reached.contains("sidecar"), "the walk arrives: {reached:?}");

        for plant in [
            "fn sidecar() { let _ = AnydocConverter.convert(b, e, c); }",
            "fn sidecar() { let _ = anydoc::to_markdown_bytes(b, f); }",
        ] {
            let planted = vec![(
                clean[0].0.clone(),
                clean[0].1.replace("fn sidecar() { let _ = 1; }", plant),
            )];
            let (leg, _) = screen_gate_path(&planted).expect("the walk runs");
            assert_eq!(leg.violations.len(), 1, "{plant}: {:#?}", leg.violations);
            assert!(
                leg.violations[0].contains("sidecar"),
                "{plant}: {:#?}",
                leg.violations
            );
        }
    }

    /// ★ The same planting, in the real hook this gate is written for: the
    /// synthetic proof above says the algorithm works, this one says it is
    /// pointed at the code that matters.
    #[test]
    fn a_planted_store_call_in_the_real_hook_is_caught() {
        let root = crate::workspace_root();
        let corpus: Vec<(String, String)> = rust_corpus(&root)
            .expect("the corpus reads")
            .into_iter()
            .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
            .collect();
        assert!(
            screen_gate_path(&corpus)
                .expect("the walk runs")
                .0
                .violations
                .is_empty(),
            "the tree is clean before anything is planted"
        );
        let planted: Vec<(String, String)> = corpus
            .into_iter()
            .map(|(rel, text)| {
                if rel != "bins/rift/src/hooks/gate.rs" {
                    return (rel, text);
                }
                let at = text
                    .find("fn read_gate(")
                    .and_then(|i| text[i..].find('{').map(|j| i + j + 1))
                    .expect("the gate has a body");
                let mut planted = text.clone();
                planted.insert_str(at, "\n    let _ = open_migrated();");
                (rel, planted)
            })
            .collect();
        let (leg, _) = screen_gate_path(&planted).expect("the walk runs");
        assert!(
            leg.violations
                .iter()
                .any(|f| f.contains("read_gate") && f.contains("open_migrated")),
            "{:#?}",
            leg.violations
        );
    }

    /// The walk over the real tree still arrives at the decision's own
    /// vocabulary. Without this, a tightening that shrank the graph to the
    /// roots would pass and guard nothing.
    #[test]
    fn the_walk_reaches_the_functions_a_decision_is_actually_made_of() {
        let root = crate::workspace_root();
        let corpus: Vec<(String, String)> = rust_corpus(&root)
            .expect("the corpus reads")
            .into_iter()
            .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
            .collect();
        let (_, reached) = screen_gate_path(&corpus).expect("the walk runs");
        for waypoint in [
            "decide",
            "deny_channel",
            "gate_limits",
            "gate_frame",
            "journal_decision",
            "read_decision_audit",
        ] {
            assert!(reached.contains(waypoint), "the walk reaches {waypoint}");
        }
    }

    /// A root that has been renamed away stops the gate rather than shrinking
    /// the walk in silence.
    #[test]
    fn a_decision_entry_point_that_no_longer_exists_is_a_hard_error() {
        let err = screen_gate_path(&[(
            "bins/rift/src/x.rs".to_owned(),
            "fn other() {}\n".to_owned(),
        )])
        .expect_err("a missing root stops the gate");
        assert!(err.contains("window_journal_step"), "{err}");
    }

    /// A store opened by a function nothing on the decision path calls is not
    /// this leg's business.
    #[test]
    fn a_store_opened_off_the_decision_path_is_not_a_finding() {
        let corpus = vec![(
            "bins/rift/src/hooks/gate.rs".to_owned(),
            "fn window_journal_step() {}\nfn read_gate() {}\nfn edit_record() {}\n\
             fn gate_policy() {}\nfn gate_step() {}\nfn edit_step() {}\nfn gate() {}\n\
             fn respond_with_decision() {}\n\
             fn doc_gate() {}\nfn doc_gate_in() {}\nfn document_verify() {}\n\
             fn ingest() { open_capture_projection(); }\n"
                .to_owned(),
        )];
        let (leg, _) = screen_gate_path(&corpus).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
    }

    /// Strings survive the comment-only masking; prose does not.
    #[test]
    fn comment_masking_keeps_the_strings_a_path_is_built_from() {
        let masked = mask_comments("let p = \".rift/state/doc\"; // .rift/state/doc\n");
        assert_eq!(masked.matches(".rift/state/doc").count(), 1, "{masked}");
        assert!(masked.contains("let p ="), "{masked}");
    }

    /// The openers come out of the renderer itself, so this gate cannot guard
    /// wording the renderer no longer has.
    #[test]
    fn the_frame_openers_are_read_out_of_the_renderer() {
        let openers = frame_openers(&crate::workspace_root()).expect("the openers parse");
        assert!(openers.len() >= 9, "{openers:?}");
        assert!(
            openers.iter().any(|o| o.contains("Read skipped:")),
            "{openers:?}"
        );
    }

    /// ★ A second place that opens a line with one of Rift's own frame phrases
    /// is caught; the same phrase in a comment is not.
    #[test]
    fn a_frame_phrase_written_outside_the_renderer_is_caught() {
        let openers = vec!["[rift] Read skipped:".to_owned()];
        let planted = vec![(
            "bins/rift/src/elsewhere.rs".to_owned(),
            "fn say() -> &'static str {\n    \"[rift] Read skipped: trust me\"\n}\n".to_owned(),
        )];
        let leg = screen_renderer(&planted, &openers);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("elsewhere.rs:2"),
            "{:#?}",
            leg.violations
        );

        let explained = vec![(
            "bins/rift/src/elsewhere.rs".to_owned(),
            "/// Never writes `[rift] Read skipped:` itself.\nfn say() {}\n".to_owned(),
        )];
        assert!(
            screen_renderer(&explained, &openers).violations.is_empty(),
            "a comment explaining the guard is not a breach of it"
        );
    }

    /// ★ A third amendment, or a third field on one of the two, is caught.
    #[test]
    fn a_third_way_to_amend_a_tool_call_is_caught() {
        let third_form = vec![(
            "crates/rift-agent/src/respond/mod.rs".to_owned(),
            "fn f() { let _ = Rewrite::ShellCommand { command: \"x\" }; }\n".to_owned(),
        )];
        let leg = screen_rewrites(&third_form).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("third amendment"),
            "{:#?}",
            leg.violations
        );

        let third_field = vec![(
            "crates/rift-agent/src/respond/mod.rs".to_owned(),
            "fn f() { let _ = Rewrite::GrepHeadLimit { tool_input: t, head_limit: 4, path: p }; }\n"
                .to_owned(),
        )];
        let leg = screen_rewrites(&third_field).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("names `path`"),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ A third variant DECLARED on the amendment enum is caught even with no
    /// caller anywhere.
    #[test]
    fn a_third_amendment_form_declared_but_never_built_is_caught() {
        let declared = vec![(
            "crates/rift-agent/src/respond/mod.rs".to_owned(),
            "pub enum Rewrite<'a> {\n    DocumentSidecar { tool_input: &'a Value, file_path: &'a str },\n             \x20   ShellPrefix { tool_input: &'a Value, prefix: &'a str },\n}\n             fn f() { let _ = Rewrite::DocumentSidecar { tool_input: t, file_path: p }; }\n"
                .to_owned(),
        )];
        let leg = screen_rewrites(&declared).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("declares `ShellPrefix`"),
            "{:#?}",
            leg.violations
        );
    }

    /// An amendment type belonging to some other crate is not this leg's
    /// business, and a tree with no amendment at all is a screen guarding
    /// nothing.
    #[test]
    fn an_unrelated_type_of_the_same_name_is_left_alone() {
        let unrelated = vec![(
            "crates/rift-shadow-attribution/tests/support/mod.rs".to_owned(),
            "fn f() -> Rewrite { Rewrite::from(1) }\n".to_owned(),
        )];
        let err = screen_rewrites(&unrelated).expect_err("nothing amendable was found");
        assert!(err.contains("no amendment site"), "{err}");
    }

    /// ★ A cache keyed by which checkout is open, and a family store reached
    /// without the common directory, are both caught.
    #[test]
    fn a_cache_keyed_by_a_checkout_is_caught() {
        let by_checkout = vec![(
            "crates/rift-recall-doc/src/lib.rs".to_owned(),
            "fn sidecar(w: &Path) -> PathBuf {\n    \
             let id = repo_id_for(w);\n    w.join(\".rift/state/doc\").join(id)\n}\n"
                .to_owned(),
        )];
        let leg = screen_family_caches(&by_checkout);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("derives an id from a checkout"),
            "{:#?}",
            leg.violations
        );

        let by_path = vec![(
            "crates/rift-git/src/family.rs".to_owned(),
            "fn store(w: &Path) -> PathBuf {\n    w.join(\"rift-family\")\n}\n".to_owned(),
        )];
        let leg = screen_family_caches(&by_path);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("without a common directory"),
            "{:#?}",
            leg.violations
        );

        let correct = vec![(
            "crates/rift-git/src/family.rs".to_owned(),
            "fn store(common_dir: &Path) -> PathBuf {\n    common_dir.join(\"rift-family\")\n}\n"
                .to_owned(),
        )];
        assert!(screen_family_caches(&correct).violations.is_empty());
    }

    /// The leg that is deliberately absent says so, and passes.
    #[test]
    fn the_unbuilt_leg_reports_itself_rather_than_disappearing() {
        let leg = load_bearing();
        assert!(leg.violations.is_empty());
        assert!(leg.detail.contains("not built"), "{}", leg.detail);
    }
}
