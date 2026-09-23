//! What the sweep runs: the arms, and the opted-in corpus each one declares.
//!
//! An arm's declared size is a claim about what capture will read, so the caps
//! the product charges it against are restated here and pinned to the product's
//! own declarations — a corpus the product would decline is an arm that times a
//! walk which captured nothing while its label still names a size.

use serde::{Deserialize, Serialize};

/// Whether a repository has rift's hooks installed, and whether a session is
/// live when the commit lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capture {
    /// No `rift init`: the control. A commit here is a bare commit.
    Off,
    /// Hooks installed, but no session has ever opened — the "installed and
    /// idle" cost every repository pays whether or not an agent is running.
    Idle,
    /// Hooks installed and a session live at commit time: the path that
    /// condenses, archives and writes the trailer.
    Live,
}

/// An opted-in corpus: what `.worktreeinclude` points capture at.
///
/// **Sizes are stated in the unit the ceiling is charged in.**
/// `rift_shadow::inclusion::MAX_TOTAL_BYTES` is 8 KiB and is charged on bytes
/// **read**, decided from the `stat` before the open. But `Walk::file` writes
/// `f <path>` and then prefixes every content line with one space, so a file of
/// N bytes contributes up to **2N + 2** to the document the redactor is handed
/// — worst case a file that is all newlines, where the prefix is the whole
/// line. A full budget therefore hands on a document *just over 16 KiB*, not 8
/// (`the_document_handed_on_is_up_to_twice_the_bytes_read`).
///
/// So every arm publishes both numbers and the factor between them, and no arm
/// is described by one number alone without saying which of the two it means. The
/// factor for a corpus of long lines is near 1.0; the 2× is the pathological
/// end, not the typical one, and conflating them in either direction misstates
/// the ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    /// Files written under the opted-in directory.
    pub files: usize,
    /// Content lines per file.
    pub lines_per_file: usize,
    /// Characters per content line, excluding the newline.
    pub line_width: usize,
}

impl Corpus {
    /// Bytes on disk — what [`MAX_TOTAL_BYTES`-equivalent] budget is charged.
    ///
    /// [`MAX_TOTAL_BYTES`-equivalent]: https://docs.rs/
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.files * self.lines_per_file * (self.line_width + 1)
    }

    /// Bytes of the document handed to the redactor.
    ///
    /// Per file: `f <rel>\n` plus, for each part of `split('\n')`, a space, the
    /// part, and a newline. A file ending in a newline yields one extra empty
    /// part, which is why the count is `lines_per_file + 1`.
    #[must_use]
    pub fn document_bytes(&self, dir: &str) -> usize {
        let per_file = |name_len: usize| {
            2 + dir.len() + 1 + name_len + 1 + (self.lines_per_file + 1) * (self.line_width + 2)
        };
        // Names are `f0000.txt`-shaped, so every file's name is the same length.
        self.files * per_file(9)
    }

    /// Bytes in one file — what `MAX_FILE_BYTES` is charged against, and the
    /// limit that declines a file **before it is opened**.
    #[must_use]
    pub fn bytes_per_file(&self) -> usize {
        self.lines_per_file * (self.line_width + 1)
    }

    /// Whether the product's own caps would let this corpus be read whole.
    #[must_use]
    pub fn is_capturable(&self) -> bool {
        self.bytes_per_file() <= MAX_FILE_BYTES
            && self.bytes() <= MAX_TOTAL_BYTES
            && self.files <= MAX_ENTRIES
    }

    /// The multiple between what is read and what is handed on.
    #[must_use]
    pub fn document_factor(&self, dir: &str) -> f64 {
        let read = self.bytes();
        if read == 0 {
            return 0.0;
        }
        self.document_bytes(dir) as f64 / read as f64
    }
}

/// The directory `.worktreeinclude` opts in, and `.gitignore` keeps out of git.
pub const CORPUS_DIR: &str = "benchdata";

/// `rift_shadow::inclusion::MAX_TOTAL_BYTES`, restated because the harness
/// links no product crate. Pinned against the declaration by
/// `the_ceiling_and_the_control_file_still_match_the_product`.
///
/// **8 KiB since the owner cut it — down from 1 MiB, a 128× cut this
/// harness's own measurements produced.** The guard that pins it reddened on the day the
/// constant landed, which is the guard working; §9–§11 of
/// `benchmarks/hook-latency.md` were taken under the **1 MiB regime** and are
/// labelled there as pre-cap, because no arm in them is capturable now.
pub const MAX_TOTAL_BYTES: usize = 8 * 1024;

/// `rift_shadow::inclusion::MAX_FILE_BYTES` — half the total budget.
///
/// It is restated because it **silently decides whether an arm measures
/// anything at all**: a file over this size is declined before it is opened, so
/// a corpus declared in the old regime would have every file refused and the
/// arm would time a walk that captured nothing while its label still said
/// 1,048,576 B. `every_declared_corpus_is_capturable_under_the_products_caps`
/// is the guard that makes that impossible to declare.
pub const MAX_FILE_BYTES: usize = MAX_TOTAL_BYTES / 2;

/// `rift_shadow::inclusion::MAX_ENTRIES` — records the walk may write.
pub const MAX_ENTRIES: usize = 256;

/// One arm of the sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arm {
    /// Stable identifier, used in the report and by `--arms`.
    pub id: String,
    /// One line describing what this arm exercises.
    pub title: String,
    /// Capture configuration.
    pub capture: Capture,
    /// The `.worktreeinclude` corpus this arm opts in, when it opts anything in.
    #[serde(default)]
    pub include: Option<Corpus>,
}

impl Arm {
    /// Whether this arm is the control every other arm is quoted against.
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.capture == Capture::Off
    }
}

/// The default sweep: the control, the idle arm, the live arm.
#[must_use]
pub fn arms() -> Vec<Arm> {
    vec![
        Arm {
            id: "A_bare".to_owned(),
            title: "bare commit, no hooks installed (control)".to_owned(),
            capture: Capture::Off,
            include: None,
        },
        Arm {
            id: "B0_idle".to_owned(),
            title: "hooks installed, nothing opted in, no live session".to_owned(),
            capture: Capture::Idle,
            include: None,
        },
        Arm {
            id: "B1_live".to_owned(),
            title: "hooks installed, a session live at commit time".to_owned(),
            capture: Capture::Live,
            include: None,
        },
        // **Re-sized for the 8 KiB cap.** The previous arm set
        // — 51 KB to 1 MiB, cheap and worst twins, plus two ladders — was
        // declared against a 1 MiB budget with a **flat 256 KiB** per-file
        // limit (`MAX_TOTAL_BYTES / 2` is the rule that arrived *with* the cut).
        // Under the new caps only `F1_newline_8k` — 8 files x 1,024 B = 8,192 B
        // — remains capturable. The rest fail for **two different reasons** and
        // the distinction matters: `C1/C2/C3`, `D1/D2/D3`, `E1-E5` and `F5`
        // exceed the 4 KiB **per-file** limit and are declined file-by-file,
        // while `F2/F3/F4` are 2,048/3,072/4,096 B per file — all within it —
        // and are stopped by the **total** budget partway through the walk. An
        // arm in either state times a walk that captured little or nothing
        // while its label still says 1,048,576 B.
        // That is the false-label class this harness exists to remove, so they are
        // gone rather than kept as decoration. Their measurements are not: §9
        // through §11 of `benchmarks/hook-latency.md` and the committed run
        // artifacts hold them, labelled as the pre-cap evidence that
        // **produced** the shipped caps.
        //
        // Pairs again, same bytes read on both sides, so the only independent
        // variable is what the redactor is handed. `G3`/`H3` sit on the new
        // ceiling exactly.
        include_arm("G1_line_2k", 4, 8, LINE_WIDTH),
        include_arm("G2_line_4k", 4, 16, LINE_WIDTH),
        include_arm("G3_line_8k", 8, 16, LINE_WIDTH),
        include_arm("H1_newline_2k", 4, 512, 0),
        include_arm("H2_newline_4k", 4, 1_024, 0),
        include_arm("H3_newline_8k", 8, 1_024, 0),
    ]
}

/// A live arm that opts a corpus in through `.worktreeinclude`.
fn include_arm(id: &str, files: usize, lines_per_file: usize, line_width: usize) -> Arm {
    let corpus = Corpus {
        files,
        lines_per_file,
        line_width,
    };
    Arm {
        id: id.to_owned(),
        title: format!(
            "live session + `.worktreeinclude` over {files} files ({} B read, {} B handed on, {:.2}x)",
            corpus.bytes(),
            corpus.document_bytes(CORPUS_DIR),
            corpus.document_factor(CORPUS_DIR),
        ),
        capture: Capture::Live,
        include: Some(corpus),
    }
}

/// Content-line width. 63 characters plus the newline is 64 bytes a line, so a
/// corpus's on-disk size is an exact multiple and an arm can sit *on* the read
/// ceiling rather than near it.
const LINE_WIDTH: usize = 63;

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::hooklat::thousands;

    #[test]
    fn the_control_is_the_only_arm_with_capture_off_and_it_is_first() {
        let arms = arms();
        assert!(arms[0].is_control(), "the control leads the sweep");
        assert_eq!(
            arms.iter().filter(|a| a.is_control()).count(),
            1,
            "exactly one control: an arm quoted against two is quoted against neither"
        );
    }

    /// **The guard that would have caught the whole B1 ripple by itself.**
    /// The old arm set survived a 128× cap cut as *declarations* — the numbers
    /// still parsed, the tests still named them, and only three assertions
    /// about one constant reddened. What nothing checked was whether the
    /// product would actually read the corpora those arms declare. Under the
    /// new caps all but `F1_newline_8k` would have been declined — most by the
    /// per-file limit, `F2`/`F3`/`F4` by the total budget instead — while their
    /// labels still said 1,048,576 B.
    #[test]
    fn every_declared_corpus_is_capturable_under_the_products_caps() {
        for arm in arms() {
            let Some(c) = arm.include else { continue };
            assert!(
                c.bytes_per_file() <= MAX_FILE_BYTES,
                "{}: {} B per file is over MAX_FILE_BYTES ({MAX_FILE_BYTES}); every file \
                 would be DECLINED BEFORE IT IS OPENED and the arm would time a walk that \
                 captured nothing while its label claimed {} B",
                arm.id,
                c.bytes_per_file(),
                c.bytes(),
            );
            assert!(
                c.bytes() <= MAX_TOTAL_BYTES,
                "{}: {} B is over MAX_TOTAL_BYTES ({MAX_TOTAL_BYTES}); the walk would stop \
                 partway and the arm's declared size would be fiction",
                arm.id,
                c.bytes(),
            );
            assert!(c.files <= MAX_ENTRIES, "{}: over MAX_ENTRIES", arm.id);
            assert!(c.is_capturable(), "{}", arm.id);
        }
    }

    /// Pairs under the new cap: same bytes read, 1.0× against 2.0× handed on,
    /// so the only independent variable is what the redactor is given.
    ///
    /// **Corrected wording, per the verifier:** the twins also differ in line
    /// *count* by 64×, which is inseparable from the mechanism — the 2× **is**
    /// the per-line prefix. "Exactly one variable" was wrong; "one
    /// *independent* variable" is right.
    #[test]
    fn the_cap_sized_pairs_read_the_same_bytes_and_hand_on_twice_as_much() {
        let all = arms();
        let rung = |id: &str| all.iter().find(|a| a.id == id).unwrap().include.unwrap();
        for (cheap, worst, bytes) in [
            ("G1_line_2k", "H1_newline_2k", 2_048),
            ("G2_line_4k", "H2_newline_4k", 4_096),
            ("G3_line_8k", "H3_newline_8k", MAX_TOTAL_BYTES),
        ] {
            let (c, w) = (rung(cheap), rung(worst));
            assert_eq!(c.bytes(), bytes, "{cheap}");
            assert_eq!(w.bytes(), bytes, "{worst} must read its twin's bytes");
            assert!(
                c.document_factor(CORPUS_DIR) < 1.35,
                "{cheap} is the cheap side"
            );
            assert!(
                w.document_factor(CORPUS_DIR) > 1.9,
                "{worst} is the 2N + 2 end"
            );
            assert!(c.is_capturable() && w.is_capturable());
        }
        // The top pair sits on the new ceiling exactly, not near it.
        assert_eq!(rung("G3_line_8k").bytes(), MAX_TOTAL_BYTES);
        assert_eq!(rung("H3_newline_8k").bytes(), MAX_TOTAL_BYTES);
    }

    /// **§1 of the tracked artifact describes this instrument, so it is pinned
    /// to it — line by line, arm to size.**
    ///
    /// The first version of this pin searched the **whole 750-line document**
    /// and asserted the arm id and the byte count *independently*, so nothing
    /// bound an arm to its size. A verifier rewrote §1's ceiling row to a
    /// fabricated `9,999` and **the pin passed**, because `8,192` still
    /// occurred elsewhere in the document. It also fell through a broken
    /// thousands formatter that rendered 512 as `"0,512"`.
    ///
    /// So: §1 is sliced out first, the arm's **own line** must carry its size,
    /// and there is no whole-document escape hatch. Calibrated by planting a
    /// fabricated size in §1 rather than by reading this code — which is the
    /// discipline the sentence describing a guard needs and did not get twice.
    ///
    /// # Declared gaps — what this guard does NOT catch
    ///
    /// It is **line-scoped string presence**, nothing stronger, and two plants
    /// pass it. Both were demonstrated, not reasoned to:
    ///
    /// 1. **Collapsing the three table rows into one line** carrying all six
    ///    arm ids and all three sizes. Every arm then finds "its" line and that
    ///    line contains "its" size, so the binding this guard was rebuilt to
    ///    enforce is satisfied by a line that binds nothing.
    /// 2. **An additive contradiction** — `| 8,192 superseded, now 9,999 (the
    ///    ceiling exactly) |`. The required substring is still present, so the
    ///    guard stays green while a reader takes away 9,999.
    ///
    /// Closing either needs a parser over the table rather than a substring
    /// search, and that is not written. The docstring above is literally true
    /// of what the code does; this section exists because true-but-narrow is
    /// still a gap, and a gap is declared by enumeration or it is a hole
    /// nobody knows about.
    #[test]
    fn section_one_states_the_arms_the_instrument_actually_runs() {
        let doc = crate::leg::default_repo_root().join("benchmarks/hook-latency.md");
        let text = match fs::read_to_string(&doc) {
            Ok(text) => text,
            Err(e) => {
                assert!(
                    !crate::leg::default_repo_root().join("benchmarks").is_dir(),
                    "{} is unreadable ({e}) but benchmarks/ exists — the guard would skip \
                     itself",
                    doc.display()
                );
                return;
            }
        };
        // §1 only. A claim about §1 checked against the whole document is the
        // vacuity this test was rebuilt to close.
        let from = text
            .find("## 1. Protocol")
            .expect("the artifact has a §1 Protocol");
        let to = text[from..]
            .find("\n## 2.")
            .map_or(text.len(), |o| from + o);
        let section = &text[from..to];
        assert!(
            section.len() < text.len(),
            "the slice must not be the document"
        );

        for arm in arms() {
            let Some(c) = arm.include else { continue };
            let needle = format!("`{}`", arm.id);
            let line = section
                .lines()
                .find(|l| l.contains(&needle))
                .unwrap_or_else(|| panic!("§1 does not name `{}`", arm.id));
            let size = thousands(c.bytes());
            assert!(
                line.contains(&size),
                "§1's line for `{}` states no size of {size} B — an arm and a number that are \
                 merely both present somewhere bind nothing:\n{line}",
                arm.id
            );
        }
        for stale in ["51,200", "256,000", "1,048,576"] {
            assert!(
                !section.contains(stale),
                "§1 still advertises a deleted arm's size ({stale})"
            );
        }
    }

    #[test]
    fn the_ceiling_and_the_control_file_still_match_the_product() {
        let root = crate::leg::default_repo_root();
        let inclusion = root.join("crates/rift-shadow/src/inclusion/mod.rs");
        // A checkout without the product tree cannot check this — but the
        // skip is bounded, not free: if `crates/` is there the file must be
        // readable, so the guard cannot be silently disarmed by a move.
        let text = match fs::read_to_string(&inclusion) {
            Ok(text) => text,
            Err(e) => {
                assert!(
                    !root.join("crates").is_dir(),
                    "the product tree is here but {} is not readable ({e}) — the guard \
                     would have skipped itself",
                    inclusion.display()
                );
                return;
            }
        };
        assert!(
            text.contains("pub const MAX_TOTAL_BYTES: usize = 8 * 1024;"),
            "the read ceiling moved in the product; this harness still says {MAX_TOTAL_BYTES}"
        );
        assert!(
            text.contains("pub const MAX_FILE_BYTES: u64 = MAX_TOTAL_BYTES as u64 / 2;"),
            "the per-file limit moved; arms sized against {MAX_FILE_BYTES} would be declined"
        );
        // Restated in this harness and, until now, pinned by nothing — the same
        // unpinned-restatement class the per-file limit was just fixed for.
        assert!(
            text.contains(&format!("pub const MAX_ENTRIES: usize = {MAX_ENTRIES};")),
            "the record cap moved in the product; this harness still says {MAX_ENTRIES}"
        );
        assert!(
            text.contains(&format!(
                "pub const INCLUDE_FILE: &str = \"{}\";",
                crate::scenario::INCLUDE_FILE
            )),
            "the opt-in control file was renamed; the arms would opt nothing in"
        );
    }
}
