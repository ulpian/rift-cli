//! The git side: what `git blame --line-porcelain` says, and what of it may
//! be believed.
//!
//! Separate from resolution because the parse boundary is where untrusted git
//! output stops being untrusted — the reasoning about quoting, framing and
//! ambiguous names lives beside the parser it constrains, not beside the
//! projection reads that consume its output.

use rift_core::{Result, RiftError};
use rift_shadow::attribution::provenance::normalize_path;
use rift_store::SqliteStore;

use super::render::render;
use super::resolve::{commit_claims, resolve};
use super::safe_text::safe_path;
use crate::cli::{exit, Outcome};

/// An inclusive 1-based line range, as `--line N` or `--line N-M` spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize)]
pub(super) struct LineRange {
    /// First line, 1-based.
    pub(super) start: usize,
    /// Last line, 1-based and inclusive.
    pub(super) end: usize,
}

/// Parse `N` or `N-M` (both 1-based, inclusive; the grammar the legacy backend's `--line`
/// used, `^\d+(-\d+)?$`, kept so operators migrating a script keep their
/// spelling). Anything else — a zero, a reversed range, a stray sign or space —
/// is a validity error, not a guess.
pub(super) fn parse_line_range(spec: &str) -> Result<LineRange> {
    let bad = || RiftError::validity("--line takes N or N-M (1-based, inclusive)");
    let (a, b) = match spec.split_once('-') {
        Some((a, b)) => (a, Some(b)),
        None => (spec, None),
    };
    // `usize::from_str` accepts `+7`; digits-only is the published grammar.
    if a.is_empty()
        || !a.bytes().all(|c| c.is_ascii_digit())
        || b.is_some_and(|b| b.is_empty() || !b.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err(bad());
    }
    let start: usize = a.parse().map_err(|_| bad())?;
    let end: usize = b.map_or(Ok(start), str::parse).map_err(|_| bad())?;
    if start == 0 || end < start {
        return Err(bad());
    }
    Ok(LineRange { start, end })
}

/// Whether an oid is the all-zero one `git blame` reports for a line that is
/// not committed yet.
///
/// A predicate rather than a comparison against a constant because the null oid
/// is 40 zeros in a sha1 repository and 64 in a sha256 one, and this verb
/// answers in both.
pub(super) fn is_uncommitted(oid: &str) -> bool {
    !oid.is_empty() && oid.bytes().all(|b| b == b'0')
}

/// One line as `git blame --line-porcelain` reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BlamedLine {
    /// The commit that last touched the line.
    pub(super) commit: String,
    /// The path the line had **in that commit** — not the path asked for, so a
    /// rename resolves against the name the checkpoint actually recorded.
    pub(super) path: String,
}

/// A run of consecutive lines with the same commit and path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Span {
    /// First line of the run, 1-based.
    pub(super) start: usize,
    /// Last line of the run, 1-based and inclusive.
    pub(super) end: usize,
    /// The commit the run belongs to.
    pub(super) commit: String,
    /// The path that commit knew the file by.
    pub(super) path: String,
}

/// Parse `git blame --line-porcelain` output into one entry per line.
///
/// The format repeats every header for every line: a `<oid> <orig> <final>`
/// header, then key/value headers, then the content prefixed by a tab. Only the
/// oid and `filename` are read — author, mail and summary are untrusted free
/// text and are deliberately dropped here rather than filtered downstream.
///
/// # What actually stops a filename from spelling a second header
///
/// **git's quoting — not the check below, and the two are easy to confuse.** To
/// choose the path that file evidence is matched against, a name has to spell
/// `filename hijacked.rs` on a line of its own, which needs a newline; and a
/// newline can never appear *inside* a value this function inspects, because it
/// splits the document into lines first (`str::lines` splits on `\n` alone —
/// `\r`, `U+0085` and `U+2028` all stay inside a line). So if git ever stopped
/// quoting, the porcelain would already carry two ordinary-looking `filename`
/// headers and no per-value check could tell them apart. What stops it is that
/// git c-quotes the whole pathname — `"b.rs\nfilename hijacked.rs"`, one line,
/// one header — and `tests/cli_provenance_e2e.rs` asserts *that*, against a name whose
/// post-newline segment is deliberately free of control characters so the
/// assertion depends on the quoting rather than on a fail-safe firing.
///
/// # What the check below is for, and the class it leaves alone
///
/// Measured on git 2.50.1: every control byte **below 0x80** is c-quoted
/// unconditionally (`ESC` → `"s\033.rs"`, `DEL` → `"d\177.rs"`), and
/// `core.quotePath` governs only bytes ≥ 0x80. So an ASCII control character in
/// a value is evidence that the framing promise did not hold at all — and a
/// value framing does not vouch for cannot ground file evidence. It keeps its
/// line, since dropping the entry would renumber every line after it, and loses
/// its path; an empty path matches no recorded file set, so it can never become
/// evidence.
///
/// The C1 controls, `U+0080..=U+009F`, are the opposite case: they are ≥ 0x80,
/// so arriving raw is exactly what `core.quotePath=false` asked for and says
/// nothing about framing. Testing `char::is_control` folded the two classes
/// together and threw away the file evidence of every C1-named path for a
/// reason that did not apply to it — the same silent degradation
/// `core.quotePath=false` was added to end for `café.rs`.
///
/// # The second thing a value has to be, to ground evidence
///
/// Distinguishable from a *different* name. `paths_contain` compares
/// `normalize_path`ed strings, and that normalization **trims**, so a file named
/// `a.rs ` (or `a.rs\u{85}`, or with any of the 25 code points Rust calls
/// whitespace at either end — measured) matches a checkpoint that recorded
/// `a.rs`. That is `file-touched` — the basis reserved for recorded evidence —
/// asserted for a path the attacker chose and a checkpoint that never saw it.
/// So a value that is not its own trimmed self loses its path here, at the
/// boundary where the value arrives, rather than by changing what matching
/// means for every caller of `normalize_path`.
///
/// **Who creates the reachable collisions — measured, because the first
/// version of this note asserted the opposite.** The 25 are exactly
/// `char::is_whitespace`, over a sweep of every scalar value to `0x10FFFF`
/// rather than the BMP. Of them, **20 reach this parser raw, and 19 of those 20
/// only because `blame` passes `-c core.quotePath=false`**: under git's default
/// (`core.quotePath=true`, git 2.50.1) exactly one arrives raw, `U+0020`, and
/// the other 19 arrive c-quoted, matching nothing. The remaining five are the
/// C0 whitespace (`TAB LF VT FF CR`), c-quoted under both settings. So this verb
/// does not inherit those 19 — **it creates them**, as the price of the
/// correctness fix that stopped `café.rs` from losing its evidence — and this
/// guard is what pays it. `tests/cli_provenance_e2e.rs` asserts the whole table against
/// real git under both settings, so the price stays measured rather than
/// narrated.
///
/// **What the guard itself costs, since "nothing" would be the convenient
/// answer and is not the true one.** A file whose name really *is* its trimmed
/// self plus whitespace — recorded that way by the projection, delivered that
/// way by git — loses its file evidence here and degrades to a labelled
/// `path-excluded (guess)`. That is unavoidable rather than free: `paths_contain`
/// cannot tell that name from the different, real one, so `file-touched` for it
/// would be a coin flip printed as a fact, and this module's whole rule is that
/// a guess is never printed as evidence. Every name that is its own normal form
/// — which is every path git can produce for an ordinary repository — is
/// untouched, and that is the claim being made, not "nothing changed".
///
/// Two adjacent facts, so they are not discovered as surprises. **Leading**
/// position collides for 27 rather than 25, `/` and `\` joining through the
/// prefix-strip and the fold; neither is reachable from git, which emits
/// repo-relative `/`-separated paths. And the *opposite* class is open and is
/// not closed here: `café.rs` in NFC and in NFD do **not** collide, so a
/// filesystem storing one form while the projection recorded the other silently
/// loses real evidence for a real file. That one costs evidence rather than
/// forging it, and it belongs to the matcher rather than to this boundary.
///
/// **This guard closes one of two entrances, and the other is named rather than
/// fixed here.** `paths_contain` normalizes *both* sides and nothing inspects
/// `files_touched`, so a checkpoint that **recorded** `src/a.rs\u{3000}` — or
/// `./src/a.rs`, or `a\b.rs` — still grounds `file-touched` for the different,
/// real `src/a.rs`, and can outrank a genuine candidate in `rank`. Smaller blast
/// radius, since the attacker must also own the commit that claims the
/// checkpoint, but the same class and the same basis. Closing it means deciding
/// what a *recorded* path that is not its own normal form means, which is the
/// attribution crate's call and not this parser's.
///
/// **The question is asked in `normalize_path`'s own vocabulary.** An earlier
/// cut asked `name.trim() != name`, which is half of what the matcher does: it
/// left the `\` → `/` fold open — so `src/a\b.rs` would still have grounded
/// evidence for the different, real `src/a/b.rs` — along with the leading `/`
/// and `./` strips, and it would have under-covered again the next time
/// `normalize_path` grew a step. `normalize_path(name) != name` is the same
/// question put to the function that actually decides, so every collision the
/// sweep finds falls out of it, including the ones nobody enumerated.
///
/// **The backslash asymmetry, decided.** `"` and `\` are c-quoted whatever
/// `core.quotePath` says (measured, git 2.50.1, both settings, asserted in
/// `tests/cli_provenance_e2e.rs`) — exactly the standing this file gives the C0 class. So
/// either of them in a value git delivered *as itself* is the same
/// broken-framing evidence, and now fires the same fail-safe. The rule cannot be
/// "no backslashes", because a c-quoted value is *made* of them; it is **an
/// unquoted value holds none of the bytes git escapes**, which is git's framing
/// contract read as one bit rather than as a model of its quoting. Deciding it
/// the other way — leaving `\` alone because the fold is unreachable from the
/// git side — would have been true and would have left the parser's own rule
/// saying two different things about two classes git treats identically.
///
/// A c-quoted value therefore keeps its spelling, and since this parser does not
/// c-unquote it can only match a projection that stores encodings too. Two
/// different names can still collide *there* (`"a\\b.rs"` and `"a\\/b.rs"` both
/// normalize to `"a//b.rs"`), which is the same routed item as the other
/// entrance: `paths_contain` normalizes **both** sides and nothing inspects
/// `files_touched`.
///
/// One residual limit, stated rather than left to be found: a filename that
/// really does hold a C0 control arrives c-quoted, and the quoted spelling
/// matches nothing the projection recorded, so that class degrades to a
/// labelled `path-excluded (guess)`. This parser does not c-unquote — the
/// evidence gained would be for names that cannot occur without an attacker,
/// and it would mean re-creating, inside a value, the newline that splitting
/// the document into lines is what makes harmless.
pub(super) fn parse_line_porcelain(out: &[u8]) -> Vec<BlamedLine> {
    let text = String::from_utf8_lossy(out);
    let mut lines = Vec::new();
    let mut commit: Option<String> = None;
    let mut path: Option<String> = None;
    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix('\t') {
            let _ = rest;
            if let (Some(c), Some(p)) = (commit.take(), path.clone()) {
                lines.push(BlamedLine { commit: c, path: p });
            }
            continue;
        }
        if let Some(name) = raw.strip_prefix("filename ") {
            // git c-quotes a name it had to escape, and only such a name, so a
            // value is either wrapped in `"` — an *encoding* of the name, which
            // this parser does not decode — or it is the name itself.
            let quoted = name.len() >= 2 && name.starts_with('"') && name.ends_with('"');
            // `"` and `\` are escaped whatever `core.quotePath` says, so either
            // of them in a value git delivered as itself is the same
            // broken-framing evidence an ASCII control is.
            let unvouched = name.chars().any(|c| c.is_ascii_control())
                || (!quoted && (name.contains('\\') || name.contains('"')));
            // A value the matcher cannot tell from a *different* name is not
            // evidence either, and the test is the matcher's own: anything else
            // re-derives `normalize_path` and drifts from it.
            let ambiguous = !quoted && normalize_path(name) != name;
            path = Some(if unvouched || ambiguous {
                String::new()
            } else {
                name.to_owned()
            });
            continue;
        }
        let head = raw.split(' ').next().unwrap_or("");
        // 40 or 64: `rift_git::validate_oid` accepts both, and accepting only
        // the sha1 width made a sha256 repository parse to nothing at all —
        // "0 lines across 0 commits" for a file that has both.
        if matches!(head.len(), 40 | 64) && head.bytes().all(|b| b.is_ascii_hexdigit()) {
            commit = Some(head.to_ascii_lowercase());
        }
    }
    lines
}

/// Collapse consecutive lines with the same commit and path into spans.
pub(super) fn spans(lines: &[BlamedLine]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let n = i + 1;
        match out.last_mut() {
            Some(s) if s.commit == l.commit && s.path == l.path => s.end = n,
            _ => out.push(Span {
                start: n,
                end: n,
                commit: l.commit.clone(),
                path: l.path.clone(),
            }),
        }
    }
    out
}

/// Run `git blame --line-porcelain` (bounded to `range` when given, via git's
/// own `-L`) and return spans carrying TRUE file line numbers. `Ok(None)` is
/// git's refusal — an untracked path, or a range past the end of the file —
/// which each verb words for itself.
pub(super) fn blamed_spans(
    repo: &rift_git::RepoPaths,
    file: &str,
    range: Option<LineRange>,
) -> Result<Option<Vec<Span>>> {
    let mut args = vec![
        "-c".to_owned(),
        "core.quotePath=false".to_owned(),
        "blame".to_owned(),
        "--line-porcelain".to_owned(),
    ];
    if let Some(r) = range {
        // git does the trimming: `-L N,M` blames only the asked lines, so a
        // one-line question on a ten-thousand-line file costs one line's
        // porcelain, not a post-hoc filter over all of it.
        args.push("-L".to_owned());
        args.push(format!("{},{}", r.start, r.end));
    }
    args.push("--".to_owned());
    args.push(file.to_owned());
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[])?;
    if !ok {
        return Ok(None);
    }
    let mut out_spans = spans(&parse_line_porcelain(&out));
    if let Some(r) = range {
        // The porcelain numbers what it was given from 1; shift every span so
        // the rendered numbers are true file lines again.
        let off = r.start - 1;
        for s in &mut out_spans {
            s.start += off;
            s.end += off;
        }
    }
    Ok(Some(out_spans))
}

/// `rift blame <file> [--line N|N-M]`: per-line provenance for one file.
pub(crate) async fn blame(
    store: &SqliteStore,
    root: &std::path::Path,
    file: &str,
    line: Option<&str>,
) -> Result<Outcome> {
    if file.trim().is_empty() {
        return Err(RiftError::validity("blame needs a path"));
    }
    let range = line.map(parse_line_range).transpose()?;
    let repo = rift_git::discover(root)?;
    // `--` before the path: `run_git_status` takes an arbitrary argv with no
    // screening, so without it a filename beginning with `-` is parsed as a
    // flag. Load-bearing only for a path that is really tracked *and* really
    // spelled like one, which is why it is pinned against tracked files named
    // `-w` and `--evil.rs` (`tests/cli_provenance_e2e.rs`): git exits 129 for both without
    // it — the first for want of a path after consuming the option, the second
    // as `error: unknown option` — and 0 with it.
    //
    // `core.quotePath=false` is a **correctness** setting, not a safety one.
    // git's default c-quotes every byte ≥ 0x80, so `café.rs` arrives as
    // `"caf\303\251.rs"` and matches nothing the projection recorded — every
    // non-ASCII path silently loses its file evidence and degrades to a guess.
    // Turning it off does not weaken the parse boundary: control characters are
    // c-quoted unconditionally, which `core.quotePath` does not govern
    // (measured; asserted in `tests/cli_provenance_e2e.rs`).
    let Some(spans) = blamed_spans(&repo, file, range)? else {
        let (what, line_json) = match range {
            // `-L` past the end of the file fails the same way an untracked
            // path does; the message carries both candidate causes.
            Some(r) => (
                format!(
                    "cannot blame {} at lines {}-{}: path untracked or range past end of file",
                    safe_path(file),
                    r.start,
                    r.end
                ),
                serde_json::json!(format!("{}-{}", r.start, r.end)),
            ),
            None => (
                format!(
                    "cannot blame {}: not a tracked path in this worktree",
                    safe_path(file)
                ),
                serde_json::Value::Null,
            ),
        };
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            what,
            serde_json::json!({ "error": "not_blameable", "file": file, "line": line_json }),
        ));
    };
    let res = resolve(store, spans, |oid| commit_claims(&repo, oid)).await?;
    Ok(render(
        file,
        range,
        &res.rows,
        &res.claims,
        &res.facts,
        res.reads,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rift_shadow::attribution::provenance::{select_for_file, CheckpointCandidate};
    use rift_shadow::attribution::SelectionBasis;

    use super::*;
    use crate::provenance::resolve::{Reads, Resolved};
    use crate::provenance::tests::{oid, render_whole};

    /// `--line` speaks the legacy backend's grammar exactly — `N` or `N-M`, digits only,
    /// 1-based, inclusive — and everything outside it is a refusal, not a
    /// guess. `+7`, `7 `, `0`, and a reversed range are the shapes a lenient
    /// parser would have quietly accepted.
    #[test]
    fn the_line_grammar_admits_n_and_n_dash_m_and_nothing_else() {
        assert_eq!(
            parse_line_range("12").unwrap(),
            LineRange { start: 12, end: 12 }
        );
        assert_eq!(
            parse_line_range("12-20").unwrap(),
            LineRange { start: 12, end: 20 }
        );
        assert_eq!(parse_line_range("7-7").unwrap().end, 7);
        for bad in [
            "", "0", "0-3", "5-4", "+7", "7 ", " 7", "a", "3-", "-3", "3-b", "1-2-3",
        ] {
            assert!(parse_line_range(bad).is_err(), "{bad:?} must be refused");
        }
    }

    fn porcelain(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut s = String::new();
        for (i, (oid, path, content)) in entries.iter().enumerate() {
            let n = i + 1;
            s.push_str(&format!("{oid} {n} {n} 1\n"));
            s.push_str("author Someone <hostile@example.invalid>\n");
            s.push_str("summary feat: \u{1b}[31mnot rendered\u{1b}[0m\n");
            s.push_str(&format!("filename {path}\n"));
            s.push_str(&format!("\t{content}\n"));
        }
        s.into_bytes()
    }

    #[test]
    fn porcelain_yields_one_entry_per_line_and_drops_the_untrusted_headers() {
        let out = porcelain(&[
            (&oid('a'), "src/lib.rs", "fn a() {}"),
            (&oid('a'), "src/lib.rs", "fn b() {}"),
            (&oid('b'), "src/old.rs", "fn c() {}"),
        ]);
        let lines = parse_line_porcelain(&out);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].commit, oid('a'));
        assert_eq!(lines[2].path, "src/old.rs");
        // Author and summary were read past, not captured: nothing downstream
        // can render them by accident.
        assert!(!format!("{lines:?}").contains("hostile"));
        assert!(!format!("{lines:?}").contains("not rendered"));
    }

    #[test]
    fn a_line_whose_content_looks_like_a_header_does_not_become_one() {
        // The content of a blamed line is tab-prefixed, so a file containing
        // what looks like porcelain must not add entries.
        let mut out = porcelain(&[(&oid('a'), "src/lib.rs", "x")]);
        out.extend_from_slice(
            format!(
                "{} 2 2 1\nfilename src/lib.rs\n\t{} 9 9 1\n",
                oid('b'),
                oid('c')
            )
            .as_bytes(),
        );
        let lines = parse_line_porcelain(&out);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].commit, oid('b'));
    }

    /// A provenance tool that silently answers "nothing" is worse than one that
    /// errors. Accepting only 40-character oids did exactly that on a real
    /// sha256 repository: `rift blame f.rs` printed "0 lines across 0 commits"
    /// for a three-line, one-commit file, because every porcelain header was
    /// 64 characters wide and none of them matched. `rift_git::validate_oid`
    /// has accepted `40 | 64` all along, so the parser was the only disagreeing
    /// half. Asserted against a real `--object-format=sha256` repository in
    /// `tests/cli_provenance_e2e.rs`; this is the parser's half.
    #[test]
    fn a_sha256_repository_is_answered_rather_than_reported_empty() {
        let long = |c: char| -> String { std::iter::repeat_n(c, 64).collect() };
        let lines = parse_line_porcelain(&porcelain(&[
            (&long('a'), "src/lib.rs", "1"),
            (&long('a'), "src/lib.rs", "2"),
            (&long('b'), "src/lib.rs", "3"),
        ]));
        assert_eq!(lines.len(), 3, "sha256 oids are oids");
        assert_eq!(lines[2].commit, long('b'));
        assert_eq!(spans(&lines).len(), 2);
        // The null oid is as wide as the repository's hash, so "not committed"
        // cannot be a comparison against one fixed constant.
        for width in [40, 64] {
            let zeros: String = std::iter::repeat_n('0', width).collect();
            assert!(is_uncommitted(&zeros), "{width}-wide null oid");
            let rows = [Resolved {
                span: Span {
                    start: 1,
                    end: 2,
                    commit: zeros,
                    path: "src/lib.rs".to_owned(),
                },
                selection: None,
                counts: None,
                sessions: Vec::new(),
            }];
            let out = render_whole(
                "src/lib.rs",
                &rows,
                &HashMap::new(),
                &HashMap::new(),
                Reads::default(),
            );
            assert!(out.human.contains("not committed"), "{}", out.human);
            // …and an uncommitted span is not counted as a commit.
            assert_eq!(out.json["summary"]["commits"], 0);
        }
        assert!(!is_uncommitted(&long('a')));
        assert!(
            !is_uncommitted(""),
            "an absent oid is not an uncommitted one"
        );
    }

    /// The parse boundary's fail-safe, and the class it deliberately leaves
    /// alone — which is the half a `char::is_control` check got wrong.
    ///
    /// An **ASCII** control character in a `filename` value means git's
    /// unconditional quoting did not happen, so the value is not vouched for as
    /// a whole path: it keeps its line and loses its name, and an empty path
    /// matches no recorded file set, so it can never read as `file-touched`.
    ///
    /// A **C1** control (`U+0080..=U+009F`) is the opposite: it is ≥ 0x80, so
    /// arriving raw is precisely what `core.quotePath=false` asked git for
    /// (measured on git 2.50.1, asserted end to end in `tests/cli_provenance_e2e.rs`). The
    /// earlier check folded the two together, so `g\u{85}.rs` — a path git
    /// spelled in full and correctly — lost its file evidence and degraded to a
    /// guess, silently, for a reason that was never about it.
    #[test]
    fn the_fail_safe_fires_on_the_class_git_promised_to_quote_and_not_on_c1() {
        let c1 = "src/g\u{85}.rs";
        let mut out = String::new();
        out.push_str(&format!("{} 1 1 1\n", oid('a')));
        out.push_str("filename src/ok.rs\n\tone\n");
        out.push_str(&format!("{} 2 2 1\n", oid('a')));
        out.push_str("filename src/e\u{1b}[31mvil.rs\n\ttwo\n");
        out.push_str(&format!("{} 3 3 1\n", oid('a')));
        out.push_str(&format!("filename {c1}\n\tthree\n"));
        let lines = parse_line_porcelain(out.as_bytes());
        assert_eq!(lines.len(), 3, "line numbering is preserved");
        assert_eq!(lines[0].path, "src/ok.rs");
        assert_eq!(lines[1].path, "", "an unvouched-for path is no path");
        assert_eq!(lines[2].path, c1, "C1 is delivered raw on purpose");

        // No path is no evidence: with a file set recorded, the answer is a
        // labelled guess rather than the `file-touched` it could have forged.
        let excluded = [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec!["src/e\u{1b}[31mvil.rs".to_owned()],
        }];
        let sel = select_for_file(&excluded, &lines[1].path).expect("a candidate");
        assert_eq!(sel.basis, SelectionBasis::PathExcluded);

        // …and a path git *did* vouch for gets the evidence it is entitled to.
        let recorded = [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec![c1.to_owned()],
        }];
        let sel = select_for_file(&recorded, &lines[2].path).expect("a candidate");
        assert_eq!(sel.basis, SelectionBasis::FileTouched);

        // The escaping the renderer would apply if any of this were ever
        // printed: `safe_path` covers both classes and the two Unicode line
        // separators that are not `Cc` at all, so nothing here depends on the
        // parser having filtered first.
        for c in [
            '\u{7f}', '\u{85}', '\u{9b}', '\u{2028}', '\u{2029}', '\u{1b}',
        ] {
            let rendered = safe_path(&format!("a{c}.rs"));
            assert!(!rendered.contains(c), "U+{:04X}: {rendered}", c as u32);
        }
    }

    /// The code points `paths_contain` cannot tell from nothing, written down
    /// once so the test below can iterate them — and pinned by the sweep in
    /// [`the_collision_class_is_whatever_the_sweep_finds`], which is what stops
    /// this list from being a list someone believed.
    const COLLIDING: [char; 25] = [
        '\u{9}', '\u{a}', '\u{b}', '\u{c}', '\u{d}', '\u{20}', '\u{85}', '\u{a0}', '\u{1680}',
        '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}',
        '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}',
        '\u{205f}', '\u{3000}',
    ];

    /// The class is closed by measurement, not by enumeration.
    ///
    /// `paths_contain` compares `normalize_path`ed strings and that
    /// normalization trims, folds `\` to `/` and strips a leading `/` or `./`,
    /// so a name differing from another only in those respects is
    /// indistinguishable from a *different* file. Every scalar value to
    /// `0x10FFFF` is tried at both ends of a name: the trailing collisions come
    /// to exactly 25 — exactly `char::is_whitespace` and exactly [`COLLIDING`] —
    /// and the leading ones to 27, `/` and `\` joining through the prefix-strip
    /// and the fold. A code point that starts colliding tomorrow turns this red
    /// without anyone having edited a list.
    #[test]
    fn the_collision_class_is_whatever_the_sweep_finds() {
        let base = ["src/a.rs".to_owned()];
        let collides =
            |name: &str| rift_shadow::attribution::provenance::paths_contain(&base, name);
        let (mut trailing, mut leading) = (Vec::new(), Vec::new());
        let mut name = String::new();
        for cp in 0..=0x0010_FFFF_u32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            name.clear();
            name.push_str("src/a.rs");
            name.push(c);
            if collides(&name) {
                trailing.push(c);
            }
            name.clear();
            name.push(c);
            name.push_str("src/a.rs");
            if collides(&name) {
                leading.push(c);
            }
        }
        assert_eq!(trailing, COLLIDING, "trailing collisions");
        assert!(
            trailing
                .iter()
                .copied()
                .eq(('\0'..=char::MAX).filter(|c| c.is_whitespace())),
            "the trailing class is exactly `char::is_whitespace`: {trailing:?}"
        );
        let mut expected_leading = trailing.clone();
        expected_leading.extend(['/', '\\']);
        expected_leading.sort_unstable();
        assert_eq!(leading.len(), 27, "leading collisions: {leading:?}");
        assert_eq!(leading, expected_leading, "leading collisions");
        // A multi-character run collides too, so the guard cannot be a
        // single-character check.
        assert!(collides("src/a.rs\u{20}\u{9}\u{3000}\u{85}"));
        // The opposite class, recorded because it is open and costs evidence
        // rather than forging it: two normal forms of the same name do *not*
        // collide, so a projection holding one and a filesystem the other
        // silently loses a real file's evidence. Not this boundary's to fix.
        assert!(!rift_shadow::attribution::provenance::paths_contain(
            &["caf\u{e9}.rs".to_owned()],
            "cafe\u{301}.rs"
        ));
    }

    /// One blamed line per `filename` value, each under its own commit header —
    /// the shape the guard is asked about, with the framing that carries it.
    fn blamed(filenames: &[&str]) -> Vec<BlamedLine> {
        let mut out = String::new();
        for (i, name) in filenames.iter().enumerate() {
            let n = i + 1;
            out.push_str(&format!("{} {n} {n} 1\n", oid('a')));
            out.push_str(&format!("filename {name}\n\t{n}\n"));
        }
        parse_line_porcelain(out.as_bytes())
    }

    /// A checkpoint that recorded exactly `src/a.rs`.
    fn recorded_a_rs() -> [CheckpointCandidate; 1] {
        [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec!["src/a.rs".to_owned()],
        }]
    }

    /// Every member of the class, through the parser — with the collision
    /// asserted alongside, so the test cannot go green because the collision
    /// quietly stopped existing.
    ///
    /// Without the guard each of these resolves to `file-touched`, the basis
    /// reserved for recorded evidence, from a checkpoint that never saw that
    /// path. *Which* of them git can deliver, and who made them deliverable, is
    /// the subject of
    /// `the_quote_path_flag_this_verb_adds_is_what_makes_nineteen_collisions_reachable`
    /// in `tests/cli_provenance_e2e.rs`: 20 arrive raw under the `core.quotePath=false`
    /// this verb passes, exactly one under git's default.
    #[test]
    fn a_name_the_matcher_cannot_tell_from_another_grounds_no_evidence() {
        let recorded = recorded_a_rs();
        for suffix in COLLIDING {
            // Two of the 25 cannot reach the guard as a raw suffix at all,
            // because the line framing consumes them first: `LF` ends the
            // porcelain line, and `str::lines` strips a trailing `CR`. Both are
            // c-quoted by git unconditionally under either `core.quotePath`
            // (measured in `tests/cli_provenance_e2e.rs`), so the class is empty in
            // practice — and the `CR` residual is pinned below rather than left
            // to be discovered.
            if suffix == '\n' || suffix == '\r' {
                continue;
            }
            // The ambiguity itself, asserted where it actually lives, so this
            // test cannot pass because the collision stopped existing: the
            // matcher really does read these two names as one file.
            assert!(
                rift_shadow::attribution::provenance::paths_contain(
                    &["src/a.rs".to_owned()],
                    &format!("src/a.rs{suffix}")
                ),
                "U+{:04X} no longer collides — reconsider the guard",
                suffix as u32
            );
            let lines = blamed(&[&format!("src/a.rs{suffix}")]);
            assert_eq!(lines.len(), 1, "the line survives");
            assert_eq!(
                lines[0].path, "",
                "U+{:04X} is trimmed away by the matcher",
                suffix as u32
            );
            let sel = select_for_file(&recorded, &lines[0].path).expect("a candidate");
            assert_eq!(
                sel.basis,
                SelectionBasis::PathExcluded,
                "U+{:04X} became evidence",
                suffix as u32
            );
        }
    }

    /// THE RESIDUAL, stated as a test so that changing how this parser splits
    /// lines has to face it: a `CR`-suffixed name reaches the guard already
    /// stripped, so it grounds evidence for the *different* file `src/a.rs`.
    /// Unreachable only because git c-quotes `CR` unconditionally — the same
    /// kind of dependency as the newline one, and asserted against real git for
    /// the same reason.
    #[test]
    fn a_carriage_return_is_stripped_before_the_guard_can_ever_see_it() {
        let lines = blamed(&["src/a.rs\r"]);
        assert_eq!(
            lines[0].path, "src/a.rs",
            "`str::lines` strips the CR before the guard can see it"
        );
    }

    /// WHAT THE GUARD COSTS, asserted rather than asserted away. This
    /// checkpoint really did record the whitespace-suffixed path, and git
    /// really did deliver it — no attacker anywhere — and the answer is a
    /// labelled guess rather than the `file-touched` it looks entitled to.
    /// Unavoidable, because `paths_contain` cannot tell that name from the
    /// different, real `src/a.rs`, so the evidence would be a coin flip printed
    /// as a fact. Not free, and not left for a reader to find.
    #[test]
    fn a_real_file_with_a_real_whitespace_name_loses_real_evidence_here() {
        let genuinely = [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec!["src/a.rs\u{3000}".to_owned()],
        }];
        let lines = blamed(&["src/a.rs\u{3000}"]);
        assert_eq!(lines[0].path, "");
        assert_eq!(
            select_for_file(&genuinely, &lines[0].path)
                .expect("a candidate")
                .basis,
            SelectionBasis::PathExcluded,
            "a real file with a real whitespace name loses real evidence here"
        );
    }

    /// A leading collision is the same ambiguity, and an ordinary name — every
    /// path git can produce for an ordinary repository — is untouched, which is
    /// the claim the cost above is narrow about.
    #[test]
    fn a_leading_space_collides_and_an_ordinary_name_still_resolves() {
        let recorded = recorded_a_rs();
        let lines = blamed(&[" src/a.rs", "src/a.rs"]);
        assert_eq!(lines[0].path, "", "a leading space is the same ambiguity");
        assert_eq!(lines[1].path, "src/a.rs");
        assert_eq!(
            select_for_file(&recorded, &lines[1].path)
                .expect("a candidate")
                .basis,
            SelectionBasis::FileTouched,
            "the real name still resolves"
        );
    }

    /// The asymmetry the C0 fail-safe left open, closed — and the one thing the
    /// closure must not cost.
    ///
    /// `"` and `\` are c-quoted whatever `core.quotePath` says, exactly like the
    /// C0 class, so either of them in a value git delivered *as itself* is the
    /// same broken framing. The backslash is the one that also forges: the
    /// matcher folds it to `/`, so `src/a\b.rs` reads as the different, real
    /// file `src/a/b.rs` — asserted below at the matcher, so this test cannot
    /// pass because the fold quietly went away.
    ///
    /// And the cost that would have made the closure wrong: a **c-quoted** value
    /// keeps its spelling. Refusing every backslash would refuse every quoted
    /// value, which is what `tests/cli_provenance_e2e.rs` needs in order to assert that git
    /// quoted the whole name onto one line at all.
    #[test]
    fn an_unquoted_value_holding_a_byte_git_always_escapes_grounds_no_evidence() {
        let recorded = [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec!["src/a/b.rs".to_owned()],
        }];
        // The collisions themselves, at the matcher.
        for name in ["src/a\\b.rs", "./src/a/b.rs", "/src/a/b.rs"] {
            assert!(
                rift_shadow::attribution::provenance::paths_contain(
                    &["src/a/b.rs".to_owned()],
                    name
                ),
                "{name} no longer collides — reconsider the guard"
            );
        }
        // Each refused, and refused as a *path* rather than as a line.
        for name in ["src/a\\b.rs", "src/a\"b.rs", "./src/a/b.rs", "/src/a/b.rs"] {
            let mut out = String::new();
            out.push_str(&format!("{} 1 1 1\n", oid('a')));
            out.push_str(&format!("filename {name}\n\tone\n"));
            let lines = parse_line_porcelain(out.as_bytes());
            assert_eq!(lines.len(), 1, "{name}: the line survives");
            assert_eq!(lines[0].path, "", "{name} kept its path");
            assert_eq!(
                select_for_file(&recorded, &lines[0].path)
                    .expect("a candidate")
                    .basis,
                SelectionBasis::PathExcluded,
                "{name} became evidence"
            );
        }
        // A c-quoted value is not refused for the backslashes it is made of.
        let mut out = String::new();
        out.push_str(&format!("{} 1 1 1\n", oid('a')));
        out.push_str("filename \"b.rs\\nfilename hijacked.rs\"\n\tone\n");
        out.push_str(&format!("{} 2 2 1\n", oid('a')));
        out.push_str("filename src/a/b.rs\n\ttwo\n");
        let lines = parse_line_porcelain(out.as_bytes());
        assert_eq!(
            lines[0].path, "\"b.rs\\nfilename hijacked.rs\"",
            "a quoted value keeps the spelling git gave it"
        );
        // …and an ordinary name still resolves, so the guard costs nothing.
        assert_eq!(lines[1].path, "src/a/b.rs");
        assert_eq!(
            select_for_file(&recorded, &lines[1].path)
                .expect("a candidate")
                .basis,
            SelectionBasis::FileTouched
        );
    }

    /// What a git that stopped c-quoting would actually cost, so the e2e
    /// assertion on the quoted spelling is not merely one of several answers
    /// that would pass.
    ///
    /// These are the bytes such a git would emit for a file named
    /// `b.rs\nfilename hijacked.rs` — and nothing in this parser can tell the
    /// second header from a real one, because by then it *is* a real header.
    /// That is the whole reason `tests/cli_provenance_e2e.rs` asserts git's quoting
    /// positively instead of asserting that `hijacked.rs` is absent.
    #[test]
    fn a_raw_newline_in_a_filename_value_would_choose_the_attackers_path() {
        let mut out = String::new();
        out.push_str(&format!("{} 1 1 1\n", oid('a')));
        out.push_str("filename b.rs\nfilename hijacked.rs\n\tone\n");
        let lines = parse_line_porcelain(out.as_bytes());
        assert_eq!(lines.len(), 1, "one blamed line either way");
        assert_eq!(lines[0].path, "hijacked.rs", "the later header wins");
    }

    #[test]
    fn adjacent_lines_with_one_answer_collapse_into_one_span() {
        let lines = parse_line_porcelain(&porcelain(&[
            (&oid('a'), "src/lib.rs", "1"),
            (&oid('a'), "src/lib.rs", "2"),
            (&oid('b'), "src/lib.rs", "3"),
            (&oid('a'), "src/lib.rs", "4"),
        ]));
        let got = spans(&lines);
        assert_eq!(got.len(), 3);
        assert_eq!((got[0].start, got[0].end), (1, 2));
        assert_eq!((got[1].start, got[1].end), (3, 3));
        assert_eq!((got[2].start, got[2].end), (4, 4));
        // A rename splits a span even when the commit is the same.
        let renamed = spans(&parse_line_porcelain(&porcelain(&[
            (&oid('a'), "new.rs", "1"),
            (&oid('a'), "old.rs", "2"),
        ])));
        assert_eq!(renamed.len(), 2);
    }
}
