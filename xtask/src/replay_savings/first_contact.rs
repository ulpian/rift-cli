//! The first-contact rows: the first sights the outline lever would have
//! answered, by class, net of the frame it would have sent, with how many the
//! outline was bound for at the time and how many the window followed with
//! another read of the file.
//!
//! A module of its own because it asks a question of the corpus no bucket
//! does — what a lever that is still observing would be worth — and every
//! figure it adds is on rows of its own, so every row before it reads the
//! same with or without it.

use std::collections::BTreeMap;
use std::sync::Arc;

use rift_agent::window_journal::{
    decide_first_contact, Decision, FirstContactFloor, Journal, Mode, ResolvedRead,
};
use rift_recall_validity::trust::{render_outline_frame, OutlineEntry, OutlinedRead};
use rift_recall_validity::{is_markdown_path, is_outlinable_path, line_span, FileOutline};
use serde_json::{json, Value};

use super::census::{FIRST_CONTACT_BYTES, FIRST_CONTACT_LINES};
use super::render::{commas, row};
use super::report::{Report, Tally};

/// The classes the rows are cut by, in the order the table prints them.
const CLASSES: [(&str, &str); 4] = [
    ("code_whole", "code, whole file"),
    ("code_first_page", "code, first page"),
    ("markdown_whole", "Markdown, whole file"),
    ("markdown_first_page", "Markdown, first page"),
];

/// A large outlinable file's outline, counted as the worker would bind it.
#[derive(Clone, Debug)]
pub(super) struct OutlineFacts {
    /// The SHA-256 of the bytes: the key a binding is found by.
    fp: String,
    /// Whether it is outlined by heading.
    markdown: bool,
    /// The outline.
    outline: FileOutline,
    /// Each definition's token count, in outline order.
    definition_tokens: Vec<u32>,
    /// The whole file's token count.
    whole_tokens: u32,
}

/// One first contact the lever would have answered.
#[derive(Clone, Debug)]
struct Candidate {
    class: usize,
    fp: String,
    stamp: String,
    tokens: u64,
    frame_tokens: u64,
}

/// What the replay saw of first contacts, folded into rows at render time.
#[derive(Clone, Debug, Default)]
pub struct FirstContactPool {
    /// Every candidate, in replay order.
    candidates: Vec<Candidate>,
    /// When each outlined revision was first read, by any window.
    first_read: BTreeMap<String, String>,
    /// The windows' further reads of a file they were outlined, by class.
    followed: [Tally; 4],
}

/// The outline facts of a file large enough for the lever, or `None`.
pub(super) fn outline_facts(key: &str, bytes: &[u8]) -> Option<Arc<OutlineFacts>> {
    let lines = bytes.split_inclusive(|b| *b == b'\n').count();
    let large = u64::try_from(lines).unwrap_or(u64::MAX) >= FIRST_CONTACT_LINES
        || u64::try_from(bytes.len()).unwrap_or(u64::MAX) >= FIRST_CONTACT_BYTES;
    if !large || !is_outlinable_path(key) {
        return None;
    }
    let outline = FileOutline::of(key, bytes)?;
    let text = std::str::from_utf8(bytes).ok()?;
    Some(Arc::new(OutlineFacts {
        fp: rift_agent::content_sha256(bytes).to_hex(),
        markdown: is_markdown_path(key),
        definition_tokens: outline
            .definitions()
            .iter()
            .map(|d| rift_tokens::count(line_span(text, d.first_line(), d.last_line())))
            .collect(),
        whole_tokens: rift_tokens::count(text),
        outline,
    }))
}

impl FirstContactPool {
    /// A read of an outlined revision: from this moment its outline is bound.
    pub(super) fn note_read(&mut self, facts: &OutlineFacts, stamp: &str) {
        let first = self
            .first_read
            .entry(facts.fp.clone())
            .or_insert_with(|| stamp.to_owned());
        if stamp < first.as_str() {
            stamp.clone_into(first);
        }
    }

    /// A first sight the shipped decision would outline, if it is one; says
    /// which class it was counted in.
    pub(super) fn note_first_sight(
        &mut self,
        read: &ResolvedRead,
        facts: &OutlineFacts,
        file_bytes: u64,
        stamp: &str,
        tokens: u64,
    ) -> Option<usize> {
        let floor = FirstContactFloor {
            min_lines: u32::try_from(FIRST_CONTACT_LINES).unwrap_or(u32::MAX),
            min_bytes: FIRST_CONTACT_BYTES,
            min_page_lines: rift_recall_validity::trust::OUTLINE_FRAME_MIN_PAGE_LINES,
        };
        let decision = decide_first_contact(
            read,
            &Journal::default(),
            Mode::Observe,
            None,
            floor,
            file_bytes,
        );
        if decision == Decision::Allow {
            return None;
        }
        let frame = frame_of(read, facts)?;
        let whole = read.range.hi >= read.total_lines;
        let class = usize::from(facts.markdown) * 2 + usize::from(!whole);
        self.candidates.push(Candidate {
            class,
            fp: facts.fp.clone(),
            stamp: stamp.to_owned(),
            tokens,
            frame_tokens: u64::from(rift_tokens::count(&frame)),
        });
        Some(class)
    }

    /// A window's further read of a file it was outlined.
    pub(super) fn note_follow_up(&mut self, class: usize, tokens: u64) {
        if let Some(tally) = self.followed.get_mut(class) {
            tally.add(tokens);
        }
    }

    /// The candidates of one class.
    fn of_class(&self, class: usize) -> impl Iterator<Item = &Candidate> {
        self.candidates.iter().filter(move |c| c.class == class)
    }

    /// Whether a candidate's outline was bound when it was read: an earlier
    /// read of the same bytes, in any window.
    fn bound(&self, candidate: &Candidate) -> bool {
        self.first_read
            .get(&candidate.fp)
            .is_some_and(|first| first.as_str() < candidate.stamp.as_str())
    }
}

/// The frame the sealed renderer would have sent for this read.
fn frame_of(read: &ResolvedRead, facts: &OutlineFacts) -> Option<String> {
    let entries: Vec<OutlineEntry<'_>> = facts
        .outline
        .definitions()
        .iter()
        .zip(&facts.definition_tokens)
        .map(|(d, tokens)| OutlineEntry {
            kind: d.kind(),
            name: d.name(),
            first_line: d.first_line(),
            last_line: d.last_line(),
            tokens: Some(*tokens),
        })
        .collect();
    let hash = read.hash.to_hex();
    render_outline_frame(&OutlinedRead {
        path: &read.path,
        hash: &hash,
        lines: facts.outline.lines(),
        bytes: facts.outline.bytes(),
        whole_tokens: Some(facts.whole_tokens),
        definitions: u32::try_from(facts.outline.definitions().len()).unwrap_or(u32::MAX),
        markdown: facts.markdown,
        entries: &entries,
        page_end: (read.range.hi < read.total_lines).then_some(read.range.hi),
    })
}

/// One class's figures: the pool, its frames, what is left net of them, the
/// bound part, and the further reads.
struct ClassFigures {
    pool: Tally,
    frames: u64,
    frame_p50: u64,
    bound: Tally,
    bound_frames: u64,
    followed: Tally,
}

impl ClassFigures {
    fn of(pool: &FirstContactPool, class: usize) -> Self {
        let mut figures = Self {
            pool: Tally::default(),
            frames: 0,
            frame_p50: 0,
            bound: Tally::default(),
            bound_frames: 0,
            followed: pool.followed.get(class).copied().unwrap_or_default(),
        };
        let mut frames: Vec<u64> = Vec::new();
        for candidate in pool.of_class(class) {
            figures.pool.add(candidate.tokens);
            figures.frames = figures.frames.saturating_add(candidate.frame_tokens);
            frames.push(candidate.frame_tokens);
            if pool.bound(candidate) {
                figures.bound.add(candidate.tokens);
                figures.bound_frames = figures.bound_frames.saturating_add(candidate.frame_tokens);
            }
        }
        frames.sort_unstable();
        figures.frame_p50 = frames.get(frames.len() / 2).copied().unwrap_or(0);
        figures
    }
}

/// The table's first-contact rows.
pub(super) fn first_contact_rows(report: &Report, denominator: u64) -> String {
    let mut out = String::from(
        "\n  first contact: first sights the outline would answer (observed; net of the frame)\n",
    );
    for (class, (_, label)) in CLASSES.iter().enumerate() {
        let f = ClassFigures::of(&report.first_contact, class);
        out.push_str(&row(label, f.pool, denominator));
        let net = Tally {
            calls: f.pool.calls,
            tokens: f.pool.tokens.saturating_sub(f.frames),
        };
        let net_label = format!("  net of frames (p50 {} tok)", commas(f.frame_p50));
        out.push_str(&row(&net_label, net, denominator));
        let bound = Tally {
            calls: f.bound.calls,
            tokens: f.bound.tokens.saturating_sub(f.bound_frames),
        };
        out.push_str(&row("  bound by an earlier read, net", bound, denominator));
        out.push_str(&row("  followed by another read", f.followed, denominator));
    }
    out
}

/// The same rows for the machine-readable document.
pub(super) fn first_contact_json(report: &Report) -> Value {
    let mut map = serde_json::Map::new();
    for (class, (key, _)) in CLASSES.iter().enumerate() {
        let f = ClassFigures::of(&report.first_contact, class);
        map.insert(
            (*key).to_owned(),
            json!({
                "calls": f.pool.calls,
                "tokens": f.pool.tokens,
                "frame_tokens": f.frames,
                "frame_tokens_p50": f.frame_p50,
                "net_tokens": f.pool.tokens.saturating_sub(f.frames),
                "bound_calls": f.bound.calls,
                "bound_net_tokens": f.bound.tokens.saturating_sub(f.bound_frames),
                "followed_calls": f.followed.calls,
                "followed_tokens": f.followed.tokens,
            }),
        );
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::{HiKind, ReadChannel, ReadRequest, ReadSpan};

    /// A code file of five hundred-line definitions.
    fn big_file() -> String {
        (0..5).fold(String::new(), |mut out, part| {
            out.push_str(&format!("fn part_{part}() {{\n"));
            for line in 0..98 {
                out.push_str(&format!("    let value_{line} = {line};\n"));
            }
            out.push_str("}\n");
            out
        })
    }

    fn read(lo: u32, hi: HiKind) -> ResolvedRead {
        let request = ReadRequest {
            path: "src/big.rs".to_owned(),
            span: ReadSpan::lines(lo, hi),
            channel: ReadChannel::Tool,
        };
        ResolvedRead::new(&request, rift_agent::content_sha256(b"rev"), 500).unwrap()
    }

    /// ★ A first whole read and a first page are counted in their classes,
    /// net of the real frame; one whose bytes another window read earlier is
    /// the bound part; a mid-file read is no first contact; and a further read
    /// of the file is counted against its class.
    #[test]
    fn first_contacts_are_counted_by_class_bound_and_followed() {
        let body = big_file();
        let facts = outline_facts("src/big.rs", body.as_bytes()).expect("a large code file");
        assert!(outline_facts("src/big.toml", body.as_bytes()).is_none());
        assert!(outline_facts("src/small.rs", b"fn a() {}\n").is_none());
        let mut pool = FirstContactPool::default();
        pool.note_read(&facts, "2026-01-01T00:00:05Z");
        let whole = read(1, HiKind::DefaultWindow(HiKind::END_OF_FILE));
        let page = read(1, HiKind::Explicit(200));
        let mid = read(300, HiKind::Explicit(400));
        assert_eq!(
            pool.note_first_sight(&whole, &facts, 9_000, "2026-01-01T00:00:09Z", 4_000),
            Some(0)
        );
        assert_eq!(
            pool.note_first_sight(&page, &facts, 9_000, "2026-01-01T00:00:01Z", 1_500),
            Some(1)
        );
        assert_eq!(
            pool.note_first_sight(&mid, &facts, 9_000, "2026-01-01T00:00:10Z", 900),
            None
        );
        pool.note_follow_up(1, 700);
        let report = Report {
            first_contact: pool,
            ..Report::default()
        };
        let json = first_contact_json(&report);
        assert_eq!(json["code_whole"]["calls"], 1);
        assert_eq!(
            json["code_whole"]["bound_calls"], 1,
            "read at :05, outlined at :09"
        );
        assert_eq!(
            json["code_first_page"]["bound_calls"], 0,
            "nothing read it before :01"
        );
        assert_eq!(json["code_first_page"]["followed_calls"], 1);
        let frame = json["code_whole"]["frame_tokens"].as_u64().unwrap();
        assert!(frame > 0 && frame < 400, "{frame}");
        assert_eq!(
            json["code_whole"]["net_tokens"].as_u64().unwrap(),
            4_000 - frame
        );
        let table = first_contact_rows(&report, 100_000);
        assert!(table.contains("code, whole file"), "{table}");
        assert!(!table.contains("src/big.rs"), "no path reaches the table");
    }
}
