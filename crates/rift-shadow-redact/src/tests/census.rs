//! The recorded false-positive baseline, `benchmarks/redaction-fp-baseline.md`:
//! the reader of its two fences, the census that measures this repository's own
//! text, the checker that judges one against the other, and the calibration
//! arms that prove the instrument reads what it claims to read.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::SecretBytes;

use super::fixtures::{visit_text_files, workspace_root};
use super::reference::builtin_detector_ids;
use crate::redact;

/// The tracked false-positive baseline, the artifact both fences live in.
const FP_ARTIFACT: &str = "benchmarks/redaction-fp-baseline.md";

/// The artifact's text, or a panic naming the path it is missing from.
fn fp_artifact_text() -> String {
    let path = workspace_root().join(FP_ARTIFACT);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the recorded false-positive baseline is missing at {}: {e}",
            path.display()
        )
    })
}

/// One row of the ` ```fp-classified ` fence: hits that are irreducible,
/// classified under a closed two-class vocabulary, and therefore
/// subtracted from the corpus count before it meets its ceiling.
#[derive(Debug, PartialEq, Eq)]
struct FpClassified {
    /// The corpus path the row is grouped under, as [`corpus_key`] writes it.
    path: String,
    detector: String,
    /// Hits subtracted from the detector's corpus-wide count.
    classified: u32,
    /// What the whole file measures for that detector, asserted **exactly**.
    file_total: u32,
}

/// The two classes, and there are exactly two. Adding a third is an owner
/// escalation, exactly as raising a ceiling is.
const FP_CLASSES: [&str; 2] = ["pinned-evidence", "frozen-bytes"];

/// Read the ` ```fp-classified ` fence.
///
/// Grammar using the earlier grouped-census form — a path is ONE
/// high-entropy token, and
/// this fence lives inside the corpus it measures, so stating each path
/// once is what keeps the table from walking the ceiling upwards:
///
/// - `--- <class>` opens a section; the class must be one of [`FP_CLASSES`];
/// - a bare line opens a path group;
/// - `TAB <detector> TAB <classified> TAB <file_total> TAB <class>: <reason>`
///   is a row under the open group.
///
/// **A row states its own class and must match its section.** That is not
/// redundancy: a sibling baseline reader carries a declared, open gap
/// where a section header injected mid-group silently refiles the rows below
/// it under the wrong reason. Here a misfiled row disagrees with its section
/// and is refused, so that class of error dies by construction rather than
/// by attention.
///
/// **What is refused, and why each one is a hazard rather than a taste:**
/// an unknown class (the vocabulary is closed); **a repeated
/// `(path, detector)` ANYWHERE in the fence** (see below — this is the one
/// that matters); a class section opened twice, or a second group for one
/// path inside one section (both are a second place for one path to be
/// listed, and a per-section refusal cannot see across either); a row before
/// any path line or before any section (its group or its class cannot be
/// known); a row that classifies nothing, or claims more than the file total
/// (a subtraction table is not a per-file parking lot, and no row may buy
/// headroom the file does not have); an entry under
/// `crates/rift-shadow-redact/**` or any `fuzz` component, which the walk
/// never reaches, so the row could only ever be an orphan; and **a reason
/// the entropy layer itself fires on**, so a quoted high-entropy token is
/// refused rather than billed to the ceiling this fence exists to relieve.
///
/// **The duplicate-key refusal is WHOLE-FENCE, and that is a repair.** It
/// was scoped to a section, and a verifier planted the hole both ways: the
/// same path listed under both class sections, and one class section
/// reopened. Two rows for one `(path, detector)` each satisfy the
/// `file_total` arm **individually** — each says truthfully what the file
/// measures — while their `classified` counts are **summed** into the
/// subtraction. A file carrying four hits therefore passed a ceiling of
/// **zero**, measured, with no code weakened and no arm disabled. That is
/// the whitelist the classification rules explicitly refuse to claim, reachable with
/// two rows of ordinary-looking table. A file whose hits would split across
/// both classes files the whole count under the one class it can be
/// defended as, and says so in the reason.
///
/// **What the reason guard does NOT do**, stated because the first version
/// of this comment claimed it: it does not refuse a spelled-out path.
/// Measured — ordinary repository paths sit under the entropy layer's
/// threshold and no detector fires on them, so the basename rule the
/// artifact states is an ARGUMENT this parser does not enforce. What the
/// guard catches is a reason carrying a token the pipeline itself masks,
/// which is the quoted-token half of the discipline.
///
/// **An ABSENT fence is refused; an EMPTY one is not.** `raw − 0` is the
/// arithmetic that ran before classification existed, so a table with no rows is the
/// identity and is the correct starting state. A fence deleted by an edit,
/// though, would take its rows with it and leave a reader that silently
/// returns nothing — a check that cannot fire.
fn fp_parse_classified(text: &str) -> Result<Vec<FpClassified>, String> {
    let mut reader = FpFenceReader::default();
    for (i, raw) in text.lines().enumerate() {
        let at = i + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.starts_with("```") {
            reader.fence_marker(line);
            continue;
        }
        if !reader.inside || line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(class) = line.strip_prefix("--- ") {
            reader.open_section(class.trim(), at)?;
            continue;
        }
        match line.strip_prefix('\t') {
            Some(row) => reader.row(row, at)?,
            None => reader.open_group(line, at)?,
        }
    }
    if !reader.saw_fence {
        return Err(format!(
            "{FP_ARTIFACT}: no fp-classified fence — an empty table is the identity and is \
             fine, but a fence that has been deleted takes its rows with it and leaves a \
             reader that cannot fire"
        ));
    }
    Ok(reader.out)
}

/// The reader's state between one line of the fence and the next: which
/// section and group are open, what has been opened before, and what has been
/// listed. One method per line kind, so each refusal sits beside the line
/// that earns it.
#[derive(Default)]
struct FpFenceReader {
    out: Vec<FpClassified>,
    section: Option<&'static str>,
    /// Path groups opened in the current section, by line.
    opened: BTreeMap<String, usize>,
    opened_sections: BTreeMap<&'static str, usize>,
    /// WHOLE-FENCE, never cleared. See `fp_parse_classified`: a key listed
    /// twice is the whitelist this arithmetic refuses to be, and clearing this
    /// at a section boundary was a real hole a verifier planted both ways.
    listed: BTreeMap<(String, String), usize>,
    group: Option<String>,
    inside: bool,
    saw_fence: bool,
}

impl FpFenceReader {
    /// A ` ``` ` line: enter the classified fence, or leave whatever fence
    /// this was.
    fn fence_marker(&mut self, line: &str) {
        self.inside = line.trim() == "```fp-classified";
        self.saw_fence |= self.inside;
        self.section = None;
        self.group = None;
        self.opened.clear();
    }

    /// A `--- <class>` line.
    fn open_section(&mut self, class: &str, at: usize) -> Result<(), String> {
        let Some(known) = FP_CLASSES.iter().find(|c| **c == class) else {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: `{class}` is not one of the two classes — the \
                 vocabulary is closed, and adding a class is an owner escalation \
                 exactly as raising the ceiling is"
            ));
        };
        if let Some(first) = self.opened_sections.get(known) {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: the `{class}` section was already opened at line \
                 {first} — a class reopened is a second place for one path to be \
                 listed, and the per-section refusals below cannot see across it"
            ));
        }
        self.opened_sections.insert(known, at);
        self.section = Some(known);
        self.group = None;
        self.opened.clear();
        Ok(())
    }

    /// A bare path line.
    fn open_group(&mut self, line: &str, at: usize) -> Result<(), String> {
        if let Some(first) = self.opened.get(line) {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: `{line}` already opened a group at line {first} in \
                 this section — merging them would let a duplicated entry pass unseen"
            ));
        }
        self.opened.insert(line.to_owned(), at);
        self.group = Some(line.to_owned());
        Ok(())
    }

    /// A tab-led entry row under the open group.
    fn row(&mut self, row: &str, at: usize) -> Result<(), String> {
        let Some(class) = self.section else {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: an entry row before any `--- <class>` section — the \
                 class it is claimed under cannot be known"
            ));
        };
        let Some(path) = self.group.clone() else {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: an entry row before any path line — the group it \
                 belongs to cannot be known"
            ));
        };
        let entry = fp_parse_row(&path, class, row, at)?;
        let key = (path.clone(), entry.detector.clone());
        if let Some(first) = self.listed.get(&key) {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: `{}` is already listed for `{path}` at line {first} — \
                 anywhere in this fence, not merely in this section. Two rows for one key \
                 each satisfy the file-total arm on their own while their classified counts \
                 SUM, which subtracts a file twice and is exactly the whitelist this \
                 arithmetic refuses to be. File the whole count under the one class it can \
                 be defended as.",
                entry.detector
            ));
        }
        self.listed.insert(key, at);
        self.out.push(entry);
        Ok(())
    }
}

/// One row's fields, with every refusal the row itself can carry.
fn fp_parse_row(path: &str, section: &str, row: &str, at: usize) -> Result<FpClassified, String> {
    let mut fields = row.splitn(4, '\t');
    let (Some(detector), Some(classified), Some(file_total), Some(tail)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(format!(
            "{FP_ARTIFACT}:{at}: expected \
             `TAB <detector> TAB <classified> TAB <file_total> TAB <class>: <reason>` \
             under `{path}`"
        ));
    };
    let number = |field: &str, what: &str| -> Result<u32, String> {
        field
            .trim()
            .parse::<u32>()
            .map_err(|_| format!("{FP_ARTIFACT}:{at}: `{field}` is not a {what}"))
    };
    let classified = number(classified, "classified count")?;
    let file_total = number(file_total, "file total")?;
    let Some((class, reason)) = tail.split_once(':') else {
        return Err(format!(
            "{FP_ARTIFACT}:{at}: the last field is `<class>: <reason>`, and this one \
             carries no class"
        ));
    };
    let (class, reason) = (class.trim(), reason.trim());
    if class != section {
        return Err(format!(
            "{FP_ARTIFACT}:{at}: this row claims `{class}` inside the `{section}` section \
             — a row filed under the wrong reason is the miss a section header cannot see"
        ));
    }
    if reason.is_empty() {
        return Err(format!(
            "{FP_ARTIFACT}:{at}: a classified entry carries the reason it is irreducible"
        ));
    }
    if !pipeline_hits(reason).is_empty() {
        return Err(format!(
            "{FP_ARTIFACT}:{at}: this reason fires the pipeline itself, so writing it \
             costs the very ceiling this row exists to relieve — name the file by \
             basename or by where it sits, and never quote the token"
        ));
    }
    if classified == 0 {
        return Err(format!(
            "{FP_ARTIFACT}:{at}: a row that classifies nothing subtracts nothing — this \
             is a subtraction table, not a per-file census to park totals in"
        ));
    }
    if classified > file_total {
        return Err(format!(
            "{FP_ARTIFACT}:{at}: {classified} classified of a {file_total} file total — no \
             row may buy headroom the file does not have"
        ));
    }
    if path.starts_with("crates/rift-shadow-redact/") || path.split('/').any(|part| part == "fuzz")
    {
        return Err(format!(
            "{FP_ARTIFACT}:{at}: `{path}` is outside the corpus — the walk excludes the \
             seeded-secret regions, so this row could only ever be an orphan"
        ));
    }
    Ok(FpClassified {
        path: path.to_owned(),
        detector: detector.trim().to_owned(),
        classified,
        file_total,
    })
}

/// The artifact's classified entries, or a panic carrying the refusal.
fn fp_classified_entries() -> Vec<FpClassified> {
    fp_parse_classified(&fp_artifact_text()).unwrap_or_else(|e| panic!("{e}"))
}

/// The tracked false-positive baseline, `benchmarks/redaction-fp-baseline.md`.
///
/// The security bar asks for a false-positive rate measured against this
/// repository's own text **with a recorded absolute baseline**. The
/// measurement above is one layer of that (five numeric detectors, asserted
/// at zero); this is the other, and the difference is where the number
/// lives. A boolean assertion inside a test is not a baseline anybody can
/// compare across commits, so the numbers live in a tracked artifact and are
/// read back **out of it** here: the ceilings below are the file's, not the
/// test's, and editing one is a reviewable diff in `benchmarks/`.
///
/// Parsing is deliberately dumb — `id = ceiling` inside the file's
/// ` ```fp-baseline ` fence — so the artifact stays a document a person
/// reads rather than a serialization format.
fn fp_baseline_ceilings() -> BTreeMap<String, u32> {
    let text = fp_artifact_text();
    let mut ceilings = BTreeMap::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("```") {
            inside = line.trim() == "```fp-baseline";
            continue;
        }
        if !inside {
            continue;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((id, ceiling)) = line.split_once('=') {
            let ceiling = ceiling
                .trim()
                .parse::<u32>()
                .unwrap_or_else(|e| panic!("baseline ceiling for {id} is not a number: {e}"));
            ceilings.insert(id.trim().to_string(), ceiling);
        }
    }
    assert!(
        !ceilings.is_empty(),
        "the baseline artifact carries no fp-baseline fence, so this probe would \
         measure nothing"
    );
    ceilings
}

/// What the **whole pipeline** masks in one document, by detector — the
/// product's own answer (`redact`), not a hand-picked detector subset, so
/// what this counts is what a developer would actually lose out of a
/// captured transcript.
fn pipeline_hits(contents: &str) -> BTreeMap<String, u32> {
    redact(&SecretBytes::from(contents))
        .expect("redaction must not fail on repository text")
        .report
        .by_detector
}

/// What the pipeline masks over a corpus, per file and in total.
///
/// **Why this is a value and not a loop inside an assertion.** The arms
/// that keep the baseline honest — an alarm that
/// bites, a classified file that grows, an entry whose file has left the
/// tree — cannot be calibrated while the walk root and the artifact path
/// are wired into the `#[test]` itself. Measuring is one function, judging
/// is another, and a synthetic corpus can be handed to both.
///
/// Per-file attribution costs no second walk: [`visit_text_files`] already
/// hands the callback the path it read.
#[derive(Debug)]
struct FpCensus {
    /// Every corpus file the walk actually read, root-relative.
    ///
    /// Kept in full — not just the files that carry hits — because "this
    /// entry names a file that is no longer in the corpus" and "this file
    /// no longer carries the hits the entry records" are different
    /// findings that want different sentences, and a map keyed only by
    /// hits cannot tell them apart.
    files: BTreeSet<String>,
    /// `(file, detector) → hits in that file`.
    per_file: BTreeMap<(String, String), u32>,
    /// `detector → hits over the whole corpus`.
    totals: BTreeMap<String, u32>,
    /// Bytes read, so the failure line can say how much was measured.
    bytes: usize,
}

/// A corpus path as the artifact writes it: relative to the walk root,
/// `/`-separated on every platform, and `None` for anything that is not a
/// plain descendant of the root.
fn corpus_key(root: &std::path::Path, path: &std::path::Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => {
                parts.push(part.to_string_lossy().into_owned());
            }
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Measure a corpus: every tracked or unignored text file under `root`,
/// minus the three regions that are not developer content.
///
/// Two of them *are* the seeded-secret corpus — this crate's own sources
/// and `fuzz/` — and counting them would count the tests' own needles as
/// repository text. The third is `docs/`, the written guide to this
/// toolchain: it is prose *about* the pipeline rather than material the
/// pipeline protects, so its worked examples (identifiers, sample hashes,
/// example keys) would otherwise be tallied as this repository's secrets
/// and would spend headroom that belongs to real source files.
///
/// **The exclusion is root-relative, and that is a deliberate correction.**
/// It used to test the absolute path for a `fuzz` component, so a checkout
/// living under a directory called `fuzz` would have excluded every file in
/// the workspace and reported a clean baseline over an empty corpus. The
/// regions are named the way the artifact names them, relative to the root
/// being measured; on this repository the two forms select exactly the same
/// files.
fn fp_census(root: &std::path::Path) -> FpCensus {
    let mut census = FpCensus {
        files: BTreeSet::new(),
        per_file: BTreeMap::new(),
        totals: BTreeMap::new(),
        bytes: 0,
    };
    visit_text_files(root, &mut |path, contents| {
        let Some(relative) = corpus_key(root, path) else {
            return;
        };
        if relative.starts_with("crates/rift-shadow-redact/")
            || relative.starts_with("docs/")
            || relative.split('/').any(|part| part == "fuzz")
        {
            return;
        }
        census.bytes += contents.len();
        census.files.insert(relative.clone());
        for (id, n) in pipeline_hits(contents) {
            *census.totals.entry(id.clone()).or_insert(0) += n;
            *census.per_file.entry((relative.clone(), id)).or_insert(0) += n;
        }
    });
    census
}

/// Judge a measured corpus against the recorded ceilings and the recorded
/// classified entries.
///
/// Returns the violation lines; empty means the baseline holds. Two arms,
/// and the classification rules are careful about which sensitivity each carries:
///
/// - **per detector**, `raw − Σ classified <= ceiling`. Hits in files
///   nobody has classified consume the shared headroom exactly as they do
///   today, so classification is not a whitelist: it moves a *counted* hit
///   out of the total, it does not stop counting a file.
/// - **per classified row**, `per_file[(path, detector)] == file_total`,
///   **exactly**. A hit added to a classified file beyond its recorded total
///   is a violation at the row, in the commit that adds it, rather than
///   headroom spent quietly. The claim "sensitivity is unchanged" is not
///   made and must not be made; what is preserved is that no file is ever
///   whitelisted.
///
/// A detector the artifact does not list is a violation whatever its count,
/// because a detector without a recorded false-positive cost is a detector
/// nobody measured. And the subtraction is **checked**: a table that
/// subtracts more hits than the corpus holds is a violation rather than a
/// floor at zero, because flooring is how an over-subtraction stops looking
/// like one.
fn fp_check(
    census: &FpCensus,
    ceilings: &BTreeMap<String, u32>,
    classified: &[FpClassified],
) -> Vec<String> {
    let mut subtracted: BTreeMap<&str, u32> = BTreeMap::new();
    let mut v: Vec<String> = Vec::new();
    for entry in classified {
        let FpClassified {
            path,
            detector,
            file_total,
            ..
        } = entry;
        *subtracted.entry(detector.as_str()).or_insert(0) += entry.classified;
        let measured = census
            .per_file
            .get(&(path.clone(), detector.clone()))
            .copied();
        match measured {
            Some(n) if n == *file_total => {}
            Some(n) if n > *file_total => v.push(format!(
                "{FP_ARTIFACT}: `{path}` now carries {n} `{detector}` hit(s) and its entry \
                 records {file_total} — a hit added to a classified file is a violation at \
                 the row, not shared headroom. Re-measure the file and correct the row in \
                 the commit that added the hit, or shed it."
            )),
            Some(n) => v.push(format!(
                "{FP_ARTIFACT}: `{path}` carries {n} `{detector}` hit(s) and its entry \
                 records {file_total} — shrink the entry in the commit that shed the hits."
            )),
            None if census.files.contains(path) => v.push(format!(
                "{FP_ARTIFACT}: `{path}` no longer carries any `{detector}` hit and its \
                 entry records {file_total} — shrink the entry in the commit that shed the \
                 hits, which here means deleting it."
            )),
            None => v.push(format!(
                "{FP_ARTIFACT}: `{path}` has left the corpus and still carries an entry — \
                 delete the entry in the commit that removed the file."
            )),
        }
    }
    for (id, raw) in &census.totals {
        let sub = subtracted.get(id.as_str()).copied().unwrap_or(0);
        // CHECKED, not saturating. A saturating floor turns "the table
        // subtracts more than the corpus carries" — a table that has
        // stopped describing the tree — into a serene zero that passes
        // every ceiling. It is the second half of the whitelist the
        // duplicate-key repair closes at parse, and it is held here too
        // because an orphan row reaches it without any duplicate.
        let Some(net) = raw.checked_sub(sub) else {
            v.push(format!(
                "{FP_ARTIFACT}: the classified entries subtract {sub} `{id}` hit(s) from a \
                 corpus that carries {raw} — a table that subtracts more hits than the tree \
                 holds has stopped describing it, and must not be allowed to floor to zero."
            ));
            continue;
        };
        let ceiling = ceilings.get(id.as_str());
        if ceiling.is_some_and(|c| net <= *c) {
            continue;
        }
        let recorded =
            ceiling.map_or_else(|| "none — unlisted detector".to_string(), u32::to_string);
        v.push(if sub == 0 {
            format!("{id}: {raw} (recorded ceiling {recorded})")
        } else {
            format!("{id}: {net} = {raw} raw − {sub} classified (recorded ceiling {recorded})")
        });
    }
    v
}

/// Three tokens measured to cost exactly **one** `entropy.high` hit each
/// and to trip no other detector, so a synthetic corpus built from them has
/// a census known by construction rather than by re-measuring it. The
/// property is asserted, not assumed — see the calibration below.
pub(super) const FP_TOKEN_A: &str = "Zq7mVx2Lp9Rw4Kd8Nb3Ty6Hs";

pub(super) const FP_TOKEN_B: &str = "Qw8rTy3uIo5pAs9dFg2hJk";

const FP_TOKEN_C: &str = "Xn4bVc7zLm1qWe6rTy0u";

const FP_TOKEN_D: &str = "Bg5nHj8kLo2pQr7sTu1v";

/// Every planted token, so the calibration below cannot forget one.
const FP_TOKENS: [&str; 4] = [FP_TOKEN_A, FP_TOKEN_B, FP_TOKEN_C, FP_TOKEN_D];

/// A synthetic corpus on disk, so the checker can be calibrated against a
/// corpus whose every hit is known instead of by mutating this repository.
fn fp_corpus(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp corpus");
    // `git init` so the walk takes its PRIMARY arm rather than the
    // packaged-source fallback: the arm being calibrated should be the arm
    // that runs over the repository. Failure is not fatal — the fallback
    // reads the same files — and every arm asserts the exact file count, so
    // a walk that goes blind reddens instead of reporting an empty corpus
    // as a clean one.
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["init", "-q"])
        .output();
    for (name, body) in files {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("corpus directory");
        }
        std::fs::write(&path, body).expect("corpus file");
    }
    dir
}

/// **A corpus key is a plain descendant of the root, `/`-joined.** Both
/// halves are load-bearing and neither is reachable from the walk, which is
/// why they are pinned here rather than left to the census tests: the
/// separator is what makes an entry in the artifact mean the same file on
/// every platform, and the refusal is what stops anything that is not a
/// descendant — a parent escape, a root, a prefix — from being keyed as if
/// it were one.
#[test]
fn a_corpus_key_is_a_plain_descendant_of_the_root_and_is_slash_joined() {
    let root = std::path::Path::new("/repo");
    assert_eq!(
        corpus_key(root, &root.join("benchmarks").join("baseline.md")),
        Some("benchmarks/baseline.md".to_string())
    );
    assert_eq!(
        corpus_key(root, root),
        None,
        "the root is not a corpus file"
    );
    assert_eq!(
        corpus_key(root, std::path::Path::new("/elsewhere/file.md")),
        None,
        "a path outside the root has no key here"
    );
    assert_eq!(
        corpus_key(root, &root.join("..").join("file.md")),
        None,
        "an escape above the root is not a descendant"
    );
}

/// **Every hit is attributed to the file that carries it**, and the walk
/// reads what it claims to read.
///
/// The instrument is calibrated before it is trusted, the same way the two
/// probes below are: each planted token is asserted to be worth exactly one
/// hit of exactly one detector first, so a census that disagrees is the
/// census being wrong and not the corpus being surprising. The file count
/// is asserted too — a walk that silently stops finding files is the
/// failure mode a per-file census exists to make visible.
#[test]
fn the_false_positive_census_attributes_every_hit_to_the_file_that_carries_it() {
    for token in FP_TOKENS {
        let calibration = pipeline_hits(token);
        assert_eq!(
            calibration.get("entropy.high"),
            Some(&1),
            "planted token is not worth one entropy hit: {calibration:?}"
        );
        assert_eq!(
            calibration.len(),
            1,
            "planted token trips a second detector: {calibration:?}"
        );
    }

    let one = format!("alpha {FP_TOKEN_A} beta\n{FP_TOKEN_B}\n");
    let two = format!("// {FP_TOKEN_C}\n");
    let three = format!("{FP_TOKEN_A}\n");
    let corpus = fp_corpus(&[
        ("corpus_one.md", one.as_str()),
        ("nested/corpus_two.rs", two.as_str()),
        // Outside the eleven text extensions: present on disk, absent from
        // the corpus, so its token must not appear in any count below.
        ("corpus_three.png", three.as_str()),
    ]);
    let census = fp_census(corpus.path());

    assert_eq!(
        census.files,
        ["corpus_one.md", "nested/corpus_two.rs"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<String>>(),
        "the walk read a different set of files than the corpus holds"
    );
    assert_eq!(census.bytes, one.len() + two.len());
    assert_eq!(census.totals.get("entropy.high"), Some(&3));
    assert_eq!(census.totals.len(), 1, "unexpected detector: {census:?}");
    assert_eq!(
        census
            .per_file
            .get(&("corpus_one.md".to_string(), "entropy.high".to_string())),
        Some(&2)
    );
    assert_eq!(
        census.per_file.get(&(
            "nested/corpus_two.rs".to_string(),
            "entropy.high".to_string()
        )),
        Some(&1)
    );
    assert_eq!(census.per_file.len(), 2);
}

/// A `fp-classified` fence around `rows`, as the artifact writes one.
fn fp_fence(rows: &str) -> String {
    format!("prose above\n\n```fp-classified\n{rows}```\n\nprose below\n")
}

/// **The artifact's own fence is readable**, and the two classes it may use
/// are the two the guard knows. Read through the same reader the guard
/// uses, so a fence that stops parsing reddens here rather than at the far
/// end of a five-minute corpus walk.
#[test]
fn the_classified_fence_in_the_artifact_parses() {
    let entries = fp_classified_entries();
    for entry in &entries {
        assert!(entry.classified >= 1 && entry.classified <= entry.file_total);
        assert!(!entry.path.is_empty() && !entry.detector.is_empty());
    }
    let text = fp_artifact_text();
    assert!(
        text.contains("\n```fp-classified\n"),
        "the artifact carries no classified fence"
    );
    for class in FP_CLASSES {
        assert!(
            text.contains(&format!("\n--- {class}\n")),
            "the artifact's fence does not carry the `{class}` section"
        );
    }
}

/// **An empty table is the identity; an absent fence is refused.** The
/// distinction is the whole reason this reader returns a `Result` instead
/// of a map: `raw − 0` is the arithmetic that ran before classification existed, so a fence
/// with no rows is the correct starting state, while a fence deleted by an
/// edit would leave a reader that silently returns nothing.
#[test]
fn an_empty_classified_fence_is_the_identity_and_a_missing_one_is_refused() {
    assert_eq!(fp_parse_classified(&fp_fence("")), Ok(Vec::new()));
    assert_eq!(
        fp_parse_classified(&fp_fence("--- pinned-evidence\n--- frozen-bytes\n")),
        Ok(Vec::new())
    );
    let missing = fp_parse_classified("prose with no fence at all\n");
    assert!(
        missing.is_err_and(|e| e.contains("no fp-classified fence")),
        "a deleted fence must not read as an empty table"
    );
    // A fence of another kind is not this one: the ceiling fence must stay
    // invisible to this reader, the way this one is invisible to it.
    let other = fp_parse_classified("```fp-baseline\nentropy.high = 460\n```\n");
    assert!(other.is_err_and(|e| e.contains("no fp-classified fence")));
}

/// **A row is grouped, classed, numbered and reasoned**, and the reader
/// returns exactly what the row says.
#[test]
fn a_classified_row_carries_its_group_class_and_both_numbers() {
    let parsed = fp_parse_classified(&fp_fence(
        "--- pinned-evidence\n\
         benches/pinned/fixtures/pins.md\n\
         \tentropy.high\t4\t6\tpinned-evidence: upstream ids the sweep cites\n\
         --- frozen-bytes\n\
         benches/pinned/fixtures/pins.md\n\
         \tpii.email\t1\t1\tfrozen-bytes: an address inside an import fixture\n",
    ));
    assert_eq!(
        parsed,
        Ok(vec![
            FpClassified {
                path: "benches/pinned/fixtures/pins.md".to_string(),
                detector: "entropy.high".to_string(),
                classified: 4,
                file_total: 6,
            },
            FpClassified {
                path: "benches/pinned/fixtures/pins.md".to_string(),
                detector: "pii.email".to_string(),
                classified: 1,
                file_total: 1,
            },
        ])
    );
}

/// **Every refusal, planted.** Each row of the table is a shape that must
/// not reach the arithmetic, with the substring its message has to carry;
/// the control above is the same grammar without the plant, so none of
/// these is passing for the wrong reason.
#[test]
fn the_classified_fence_refuses_every_shape_that_could_park_debt() {
    const OK_ROW: &str = "\tentropy.high\t2\t2\tpinned-evidence: an upstream id the sweep cites\n";
    let planted: [(&str, &str); 11] = [
        (
            "--- invented-class\nsome/file.md\n",
            "not one of the two classes",
        ),
        (
            "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\t2\tpinned-evidence: one\n\tentropy.high\t1\t2\tpinned-evidence: two\n",
            "already listed",
        ),
        (
            "--- pinned-evidence\nsome/file.md\nsome/file.md\n",
            "already opened a group",
        ),
        (
            "--- pinned-evidence\n\tentropy.high\t1\t1\tpinned-evidence: no group\n",
            "before any path line",
        ),
        (
            "some/file.md\n\tentropy.high\t1\t1\tpinned-evidence: no section\n",
            "before any `--- <class>` section",
        ),
        (
            "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\n",
            "expected",
        ),
        (
            "--- pinned-evidence\nsome/file.md\n\tentropy.high\tmany\t2\tpinned-evidence: one\n",
            "is not a classified count",
        ),
        (
            "--- pinned-evidence\nsome/file.md\n\tentropy.high\t0\t2\tpinned-evidence: one\n",
            "classifies nothing",
        ),
        (
            "--- pinned-evidence\nsome/file.md\n\tentropy.high\t3\t2\tpinned-evidence: one\n",
            "headroom the file does not have",
        ),
        (
            "--- pinned-evidence\ncrates/rift-shadow-redact/src/lib.rs\n\tentropy.high\t1\t1\tpinned-evidence: one\n",
            "outside the corpus",
        ),
        (
            "--- pinned-evidence\nsome/fuzz/corpus/seed.txt\n\tentropy.high\t1\t1\tpinned-evidence: one\n",
            "outside the corpus",
        ),
    ];
    for (rows, expected) in planted {
        let refusal = fp_parse_classified(&fp_fence(rows));
        assert!(
            refusal.as_ref().is_err_and(|e| e.contains(expected)),
            "expected a refusal carrying {expected:?}, got {refusal:?}"
        );
    }
    // The same grammar with a well-formed row: the refusals above are the
    // plants biting, not the reader refusing everything.
    assert!(fp_parse_classified(&fp_fence(&format!(
        "--- pinned-evidence\nsome/file.md\n{OK_ROW}"
    )))
    .is_ok_and(|e| e.len() == 1));
    // A row filed under the wrong section reads as correct to a reader that
    // trusts its headers — this one does not.
    let misfiled = fp_parse_classified(&fp_fence(
        "--- frozen-bytes\nsome/file.md\n\tentropy.high\t1\t1\tpinned-evidence: one\n",
    ));
    assert!(misfiled.is_err_and(|e| e.contains("filed under the wrong reason")));
}

/// The corpus the whitelist arms share: one file carrying FOUR entropy hits
/// and one email. The file carries two detectors on purpose: the legal
/// control needs a second detector to be listed under the second class, and
/// a row may only name hits the file actually has. The census is asserted
/// before it is trusted, so an arm cannot pass over a corpus it misread.
fn census_with_two_detectors_in_one_file() -> FpCensus {
    let census = fp_calibration(&[(
        "corpus_one.md",
        &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n{FP_TOKEN_C}\n{FP_TOKEN_D}\ndev@rift.invalid\n"),
    )]);
    assert_eq!(
        census.totals,
        BTreeMap::from([
            ("entropy.high".to_string(), 4u32),
            ("pii.email".to_string(), 1u32),
        ]),
        "the calibration corpus is not what this arm thinks it is"
    );
    census
}

/// **The verifier's whitelist, planted end to end and refused.**
///
/// This is the defect C9 repairs, kept as its own arm because a regression
/// here is silent: every other arm stays green while the ceiling stops
/// binding. A file carrying FOUR hits, two rows for the same
/// `(path, detector)` split across the two class sections, each row telling
/// the truth about the file on its own — and a ceiling of **zero**. Before
/// the repair the pair parsed, `classified` summed to 8, and the
/// saturating floor turned 4 − 8 into a serene 0 that passed. Now the table
/// never reaches the arithmetic.
///
/// Route 1: the same key under the two different class sections.
#[test]
fn one_file_listed_under_both_classes_cannot_be_subtracted_twice() {
    let split = fp_parse_classified(&fp_fence(
        "--- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t4\t4\tpinned-evidence: upstream ids the sweep cites\n\
         --- frozen-bytes\n\
         corpus_one.md\n\
         \tentropy.high\t4\t4\tfrozen-bytes: ids inside an import fixture\n",
    ));
    assert!(
        split
            .as_ref()
            .is_err_and(|e| e.contains("is already listed for") && e.contains("SUM")),
        "the same key under two classes must be refused: {split:?}"
    );
}

/// Route 2 of the same whitelist: one class section reopened, which used to
/// clear the per-section bookkeeping and hand back the same hole.
#[test]
fn a_reopened_class_section_cannot_list_a_file_twice() {
    let reopened = fp_parse_classified(&fp_fence(
        "--- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t4\t4\tpinned-evidence: upstream ids the sweep cites\n\
         --- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t4\t4\tpinned-evidence: the same ids again\n",
    ));
    assert!(
        reopened.is_err_and(|e| e.contains("section was already opened")),
        "a reopened class section must be refused"
    );
}

/// The control the whitelist arms need: the shape that stays legal, and why
/// the key is `(path, detector)` and not `path` — one file may be listed
/// under both classes for DIFFERENT detectors, and the arithmetic is sound:
/// 4 − 1 = 3 against a ceiling of 3.
#[test]
fn one_file_may_be_listed_under_both_classes_for_different_detectors() {
    let census = census_with_two_detectors_in_one_file();
    let legal = fp_classified_from(
        "--- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t1\t4\tpinned-evidence: one upstream id the sweep cites\n\
         --- frozen-bytes\n\
         corpus_one.md\n\
         \tpii.email\t1\t1\tfrozen-bytes: an address inside an import fixture\n",
    );
    assert_eq!(legal.len(), 2);
    assert_eq!(
        fp_check(
            &census,
            &fp_ceilings(&[("entropy.high", 3), ("pii.email", 1)]),
            &legal
        ),
        Vec::<String>::new()
    );
}

/// The shape the whitelist repair protects: had a duplicate pair reached the
/// arithmetic, the subtraction is CHECKED and reddens rather than flooring to
/// a zero that passes a ceiling of zero. Constructed directly, because the
/// parser no longer lets a table say it.
#[test]
fn a_doubled_pair_reaching_the_arithmetic_reddens_rather_than_flooring() {
    let census = census_with_two_detectors_in_one_file();
    let doubled = vec![
        FpClassified {
            path: "corpus_one.md".to_string(),
            detector: "entropy.high".to_string(),
            classified: 4,
            file_total: 4,
        },
        FpClassified {
            path: "corpus_one.md".to_string(),
            detector: "entropy.high".to_string(),
            classified: 4,
            file_total: 4,
        },
    ];
    let violations = fp_check(
        &census,
        &fp_ceilings(&[("entropy.high", 0), ("pii.email", 1)]),
        &doubled,
    );
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(
        violations[0].contains("subtract 8 `entropy.high` hit(s) from a corpus that carries 4"),
        "{violations:?}"
    );
}

/// **An orphan row reaches the over-subtraction without any duplicate**,
/// which is why the checked arm is held in the checker and not only at
/// parse: the entry for a file that has left the corpus goes on subtracting
/// hits nothing measures.
#[test]
fn an_orphan_entry_cannot_subtract_the_corpus_below_zero() {
    let census = fp_calibration(&[("corpus_one.md", &format!("{FP_TOKEN_A}\n"))]);
    let table = fp_classified_from(
        "--- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t1\t1\tpinned-evidence: one upstream id the sweep cites\n\
         --- frozen-bytes\n\
         corpus_gone.md\n\
         \tentropy.high\t3\t3\tfrozen-bytes: ids inside an import fixture\n",
    );
    let violations = fp_check(&census, &fp_ceilings(&[("entropy.high", 0)]), &table);
    assert_eq!(violations.len(), 2, "{violations:?}");
    assert!(
        violations[0].contains("has left the corpus"),
        "{violations:?}"
    );
    assert!(
        violations[1].contains("subtract 4 `entropy.high` hit(s) from a corpus that carries 1"),
        "{violations:?}"
    );
}

/// **The reason discipline is enforced by the counter itself.** The rule
/// forbids quoting the token and forbids naming a file by its full
/// path, because this fence sits inside the corpus it measures. Prose is
/// free; a quoted high-entropy token is refused at parse rather than billed
/// to the ceiling the row exists to relieve.
#[test]
fn a_classified_reason_may_not_cost_the_ceiling_it_relieves() {
    let free = fp_parse_classified(&fp_fence(
        "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\t1\tpinned-evidence: an \
         upstream blob id the sweep cites, named in the pin file beside it\n",
    ));
    assert!(free.is_ok_and(|e| e.len() == 1), "ordinary prose is free");
    let quoted = fp_parse_classified(&fp_fence(&format!(
        "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\t1\tpinned-evidence: the id \
         {FP_TOKEN_A}\n"
    )));
    assert!(
        quoted.is_err_and(|e| e.contains("fires the pipeline itself")),
        "quoting the token must be refused"
    );
    // **What it does NOT refuse, asserted so the claim cannot drift back.**
    // The parser's own comment once said a spelled-out path was refused
    // here; measured, an ordinary repository path sits under the entropy
    // layer's threshold and no detector fires on it. The basename half of
    // the discipline is an ARGUMENT this guard does not enforce, and the
    // artifact now says so rather than borrowing this guard's authority.
    let spelled_out = fp_parse_classified(&fp_fence(
        "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\t1\tpinned-evidence: cited by \
         crates/rift-shadow-hooks/tests/adapter_sweep.rs\n",
    ));
    assert!(
        spelled_out.is_ok_and(|e| e.len() == 1),
        "the guard does not refuse a path, and this arm exists to keep that stated"
    );
}

// -----------------------------------------------------------------------
// The calibration, eight arms
//
// Each arm is planted AND run without the plant, and each asserts an EXACT
// violation count, because "it reddened" is satisfied by a checker that
// reddens at everything. Two arms live above with the reader they
// calibrate (unknown class, duplicate `(path, detector)`); the six here
// need a corpus, which the pure checker is what makes possible — the guard
// used to wire the walk root and the artifact path into the `#[test]`
// itself, so calibrating it meant mutating this repository.
//
// The control arm asserts on the census the helper RETURNS, never on
// printed output: `assert!` evaluates its message only on failure, so a
// green run of the real guard prints nothing at all.
// -----------------------------------------------------------------------

/// A ceiling table, as the artifact's fence would parse to.
fn fp_ceilings(rows: &[(&str, u32)]) -> BTreeMap<String, u32> {
    rows.iter().map(|(id, n)| ((*id).to_string(), *n)).collect()
}

/// A classified table, read through the real reader so a calibration arm
/// cannot assert against a shape the artifact could not express.
fn fp_classified_from(rows: &str) -> Vec<FpClassified> {
    fp_parse_classified(&fp_fence(rows)).expect("the calibration table must parse")
}

/// Measure a synthetic corpus, asserting first that the walk read exactly
/// the files the arm planted: an arm whose corpus is not what it thinks it
/// is measures nothing, and a walk that goes blind reports zero violations
/// for the wrong reason.
fn fp_calibration(files: &[(&str, &str)]) -> FpCensus {
    let corpus = fp_corpus(files);
    let census = fp_census(corpus.path());
    assert_eq!(
        census.files.len(),
        files.len(),
        "the calibration corpus is not the corpus the arm planted: {census:?}"
    );
    census
}

/// **Arm 1 — the unclassified alarm bites.** The plant is one hit over the
/// ceiling with nothing classified at all; the control is the same corpus
/// under a ceiling that admits it.
#[test]
fn an_unclassified_hit_over_the_ceiling_reddens_and_the_control_does_not() {
    let census = fp_calibration(&[
        ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
    ]);
    assert_eq!(census.totals.get("entropy.high"), Some(&3));

    let planted = fp_check(&census, &fp_ceilings(&[("entropy.high", 2)]), &[]);
    assert_eq!(planted.len(), 1, "{planted:?}");
    assert_eq!(planted[0], "entropy.high: 3 (recorded ceiling 2)");

    let control = fp_check(&census, &fp_ceilings(&[("entropy.high", 3)]), &[]);
    assert_eq!(control, Vec::<String>::new());
}

/// **Arm 2 — classification is NOT a whitelist**, and this is the
/// load-bearing arm. A classified file's recorded hits leave the total;
/// nothing else about the corpus is blessed. A novel token in an
/// *unclassified* file consumes the shared headroom exactly as it did
/// before classification existed, and reddens on the detector line with the arithmetic
/// spelled out.
#[test]
fn classification_is_not_a_whitelist_a_novel_hit_elsewhere_still_reddens() {
    let table = fp_classified_from(
        "--- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t2\t2\tpinned-evidence: upstream ids the sweep cites\n",
    );
    let ceilings = fp_ceilings(&[("entropy.high", 1)]);

    let control = fp_calibration(&[
        ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
    ]);
    assert_eq!(control.totals.get("entropy.high"), Some(&3));
    assert_eq!(fp_check(&control, &ceilings, &table), Vec::<String>::new());

    let planted = fp_calibration(&[
        ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n{FP_TOKEN_D}\n")),
    ]);
    assert_eq!(planted.totals.get("entropy.high"), Some(&4));
    let violations = fp_check(&planted, &ceilings, &table);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert_eq!(
        violations[0],
        "entropy.high: 2 = 4 raw − 2 classified (recorded ceiling 1)"
    );
}

/// **Arm 2b — growth inside a classified file reddens AT THE ROW**, in the
/// commit that adds it, and the ceiling is deliberately generous so that
/// only the row arm can be what fires. Without this arm the extra hit would
/// merely rejoin the shared headroom and stay silent until the whole corpus
/// crossed the ceiling — the weaker property this table refuses to claim.
#[test]
fn growth_inside_a_classified_file_reddens_at_the_row() {
    let table = fp_classified_from(
        "--- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t2\t2\tpinned-evidence: upstream ids the sweep cites\n",
    );
    let ceilings = fp_ceilings(&[("entropy.high", 10)]);

    let control = fp_calibration(&[
        ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
    ]);
    assert_eq!(fp_check(&control, &ceilings, &table), Vec::<String>::new());

    let planted = fp_calibration(&[
        (
            "corpus_one.md",
            &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n{FP_TOKEN_D}\n"),
        ),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
    ]);
    let violations = fp_check(&planted, &ceilings, &table);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(
        violations[0].contains("now carries 3 `entropy.high` hit(s) and its entry records 2")
            && violations[0].contains("violation at the row, not shared headroom"),
        "{violations:?}"
    );
    // The shared-headroom arm stayed silent, which is the whole point: 4
    // raw − 2 classified is 2, comfortably under a ceiling of 10.
    assert_eq!(planted.totals.get("entropy.high"), Some(&4));
}

/// **Arm 3 — a stale entry reddens.** Two plants, because shedding has two
/// endings: the file keeps some of its hits, or it keeps none. Both are the
/// same instruction — shrink the entry in the commit that shed them.
#[test]
fn an_entry_whose_file_shed_hits_reddens_as_stale() {
    let table = fp_classified_from(
        "--- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t2\t2\tpinned-evidence: upstream ids the sweep cites\n",
    );
    let ceilings = fp_ceilings(&[("entropy.high", 10)]);

    let control = fp_calibration(&[
        ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
    ]);
    assert_eq!(fp_check(&control, &ceilings, &table), Vec::<String>::new());

    let some_shed = fp_calibration(&[
        ("corpus_one.md", &format!("{FP_TOKEN_A}\n")),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
    ]);
    let violations = fp_check(&some_shed, &ceilings, &table);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(
        violations[0].contains("carries 1 `entropy.high` hit(s) and its entry records 2")
            && violations[0].contains("shrink the entry in the commit that shed the hits"),
        "{violations:?}"
    );

    // The unclassified file carries two hits here so that the entry's two
    // classified hits still have a corpus to come out of: this arm is about
    // the stale entry, and the over-subtraction has an arm of its own.
    let all_shed = fp_calibration(&[
        ("corpus_one.md", "ordinary prose, nothing to see\n"),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n{FP_TOKEN_D}\n")),
    ]);
    let violations = fp_check(&all_shed, &ceilings, &table);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(
        violations[0].contains("no longer carries any `entropy.high` hit"),
        "{violations:?}"
    );
}

/// **Arm 4 — an orphan entry reddens.** A file that has left the corpus
/// takes its entry with it; an entry that outlives its file would otherwise
/// go on subtracting hits nothing measures, which is the shape that turns a
/// shrink-only table into a parking lot.
#[test]
fn an_entry_whose_file_left_the_corpus_reddens_as_an_orphan() {
    let table = fp_classified_from(
        "--- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t2\t2\tpinned-evidence: upstream ids the sweep cites\n",
    );
    let ceilings = fp_ceilings(&[("entropy.high", 10)]);

    let control = fp_calibration(&[
        ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
    ]);
    assert_eq!(fp_check(&control, &ceilings, &table), Vec::<String>::new());

    // Two hits in the surviving file, so the vanished entry's two
    // classified hits still have a corpus to come out of: this arm is about
    // the orphan, and the over-subtraction has an arm of its own.
    let planted = fp_calibration(&[("corpus_two.md", &format!("{FP_TOKEN_C}\n{FP_TOKEN_D}\n"))]);
    let violations = fp_check(&planted, &ceilings, &table);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(
        violations[0].contains("has left the corpus")
            && violations[0].contains("delete the entry in the commit that removed the file"),
        "{violations:?}"
    );
}

/// **Arm 7 — the control, asserted on the value and not on the output.**
/// A green run of the real guard prints nothing: `assert!` evaluates its
/// format arguments only on failure. So the arm that shows the instrument
/// is reading the corpus it claims to read has to hold the census in its
/// hand — the exact file set, the exact per-file attribution, the exact
/// totals — and then show the checker silent over it.
#[test]
fn the_calibration_control_is_green_on_the_census_the_helper_returns() {
    let table = fp_classified_from(
        "--- pinned-evidence\n\
         corpus_one.md\n\
         \tentropy.high\t1\t2\tpinned-evidence: one upstream id the sweep cites\n",
    );
    let census = fp_calibration(&[
        ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
        ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
    ]);
    assert_eq!(
        census.files,
        ["corpus_one.md", "corpus_two.md"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<String>>()
    );
    assert_eq!(
        census
            .per_file
            .get(&("corpus_one.md".to_string(), "entropy.high".to_string())),
        Some(&2)
    );
    assert_eq!(
        census
            .per_file
            .get(&("corpus_two.md".to_string(), "entropy.high".to_string())),
        Some(&1)
    );
    assert_eq!(
        census.totals,
        BTreeMap::from([("entropy.high".to_string(), 3u32)])
    );
    // 3 raw − 1 classified = 2, which is the ceiling exactly: the control
    // sits ON the boundary rather than comfortably inside it, so an
    // off-by-one in the comparison shows up here.
    assert_eq!(
        fp_check(&census, &fp_ceilings(&[("entropy.high", 2)]), &table),
        Vec::<String>::new()
    );
}

/// **A detector cannot join this crate without a measured false-positive
/// cost.** The artifact's id set and the built-in census are the same set,
/// asserted in both directions: a new detector with no ceiling reddens here,
/// and a ceiling for a detector that no longer exists reddens here too, so
/// the artifact cannot rot into a list of names that used to mean something.
#[test]
fn the_false_positive_baseline_prices_every_builtin_detector() {
    let mut priced: Vec<String> = fp_baseline_ceilings().into_keys().collect();
    priced.sort();
    assert_eq!(
        priced,
        builtin_detector_ids(),
        "benchmarks/redaction-fp-baseline.md and the built-in census disagree: every \
         detector needs a measured false-positive ceiling, and every ceiling needs a \
         detector"
    );
}

/// **The recorded baseline still holds**, measured the way it was recorded.
///
/// Ceilings rather than equalities, because the corpus is a live repository
/// and ordinary growth moves the count; a ceiling with recorded headroom
/// fails on a *class* of false positive appearing, which is the thing worth
/// knowing. The numeric PII detectors carry exact zeros: they are the claim,
/// so they get no headroom.
///
/// The instrument is calibrated before it is trusted, the same way the
/// numeric probe above is: a control document with one known credential in
/// it must produce exactly one hit, attributed to the right detector. A
/// probe that would stay silent on a real hit measures nothing.
///
/// **The quantity compared to the ceiling is `raw − Σ
/// classified`**, and the artifact's classified fence is the only place
/// that difference may live. With that fence empty the arithmetic is the
/// identity and this is the same assertion it has always been.
#[test]
fn the_recorded_false_positive_baseline_holds_over_this_repositorys_own_text() {
    let control = pipeline_hits("aws AKIAIOSFODNN7EXAMPLE\nharmless 1.2.3\n");
    assert_eq!(
        control.get("vendor.aws.access_key_id"),
        Some(&1),
        "probe is blind: {control:?}"
    );
    assert_eq!(control.len(), 1, "probe over-reports: {control:?}");

    let census = fp_census(&workspace_root());
    let scanned = census.files.len();
    // The occupancy, stated by the instrument. The artifact asks every
    // contributor to record the current occupancy before writing, and one
    // that has
    // to build a scratch probe to read it will estimate instead — three
    // separate instruments were built for that reason in one session. A
    // green run still says nothing by default (`cargo test … -- --nocapture`
    // is the ask), because `assert!` evaluates its message only on failure.
    println!(
        "fp census: {scanned} files, {} bytes, spans {}, {:?}",
        census.bytes,
        census.totals.values().sum::<u32>(),
        census.totals
    );
    println!(
        "fp census: this artifact contributes {:?}",
        census
            .per_file
            .iter()
            .filter(|((path, _), _)| path == FP_ARTIFACT)
            .map(|((_, id), n)| (id.as_str(), *n))
            .collect::<Vec<_>>()
    );
    assert!(
        scanned >= 400,
        "the probe scanned only {scanned} text files (floor 400) — a walk that stops \
         finding files reports a clean baseline for the wrong reason"
    );
    let over = fp_check(&census, &fp_baseline_ceilings(), &fp_classified_entries());
    assert!(
        over.is_empty(),
        "the redaction false-positive baseline is exceeded over this repository's own \
         text ({scanned} files, {} bytes). Re-measure, classify the new hits by \
         hand, and record both in benchmarks/redaction-fp-baseline.md:\n{}",
        census.bytes,
        over.join("\n")
    );
}
