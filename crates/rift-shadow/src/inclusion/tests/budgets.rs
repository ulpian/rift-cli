//! The four caps, charged on what actually happened: a refusal is a record,
//! a read is a read, and a spent global budget ends the walk.

use std::path::Path;

use crate::inclusion::{
    capture_document, INCLUDE_FILE, MAX_ENTRIES, MAX_FILE_BYTES, MAX_TOTAL_BYTES,
};

use super::fixtures::doc;

/// **The half-budget property, exercised in the band where it decides
/// anything: over the per-file cap, UNDER the document budget.**
///
/// That band is the whole point. While the cap was a flat 256 KiB inside an
/// 8 KiB budget the refusal still fired — but only for files 32× larger than
/// the whole document, and everything between 8 KiB and 256 KiB was refused
/// by the BUDGET instead, so the per-file cap decided nothing. A fixture
/// that only tripped it at `MAX_FILE_BYTES + 1` could not tell the two
/// apart; this one asserts its own premise so it cannot quietly stop being
/// about fairness if either constant moves again.
///
/// Fairness is the second half and is what a refusal alone does not prove:
/// the oversize file is declined and **its neighbours are still captured**.
/// One large file must not crowd out the rest of the include list.
#[test]
fn one_oversize_file_is_refused_without_crowding_out_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    let cap = usize::try_from(MAX_FILE_BYTES).unwrap();
    let hog = "x".repeat(cap + 1);
    assert!(
        hog.len() < MAX_TOTAL_BYTES,
        "premise: the hog must fit the DOCUMENT budget and be refused by the \
         PER-FILE cap — otherwise this tests the budget, not fairness \
         ({} bytes vs budget {MAX_TOTAL_BYTES})",
        hog.len()
    );
    std::fs::write(dir.path().join("hog.txt"), &hog).unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "beta\n").unwrap();
    std::fs::write(dir.path().join(INCLUDE_FILE), "a.txt\nb.txt\nhog.txt\n").unwrap();

    let text = doc(dir.path());
    assert!(
        text.contains("x hog.txt too-large"),
        "the per-file cap refused it, and said so as `too-large` rather than \
         `budget-spent`: {text}"
    );
    assert!(
        text.contains("alpha") && text.contains("beta"),
        "and the files it would have crowded out are still captured: {text}"
    );
}

#[test]
fn an_oversize_file_is_refused_whole_never_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let big = "x".repeat(usize::try_from(MAX_FILE_BYTES).unwrap() + 1);
    std::fs::write(dir.path().join("big.txt"), &big).unwrap();
    std::fs::write(dir.path().join(INCLUDE_FILE), "big.txt\n").unwrap();
    assert_eq!(doc(dir.path()), "rift-capture/1\nx big.txt too-large\n");
}

#[test]
fn a_spent_budget_stops_the_walk_instead_of_narrating_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("many");
    std::fs::create_dir_all(&big).unwrap();
    for i in 0..(MAX_ENTRIES + 32) {
        std::fs::write(big.join(format!("f{i:04}.txt")), "x").unwrap();
    }
    std::fs::write(dir.path().join(INCLUDE_FILE), "many/\n").unwrap();

    let text = doc(dir.path());
    assert_eq!(
        text.lines().filter(|l| l.starts_with("f ")).count(),
        MAX_ENTRIES,
        "captured up to the cap"
    );
    // …and exactly ONE refusal, not one per remaining file: a per-file
    // narration of a spent budget is how `node_modules/` turns a hook into a
    // multi-megabyte document the redactor has to re-scan every commit.
    assert_eq!(
        text.lines().filter(|l| l.starts_with("x ")).count(),
        1,
        "the walk stops at the cap: {}",
        text.lines().filter(|l| l.starts_with("x ")).count()
    );
    assert!(text.trim_end().ends_with("too-many"), "and says why");
}

/// A refusal is a line in the document and a read is work the hook paid
/// for, so both budgets charge them. Measured before the change: an
/// opted-in directory of 600 × 64 KiB binary files produced **600** refusal
/// lines and read **39 MB**, with no budget in the way — the very
/// multi-megabyte-in-a-commit-hook shape [`Walk::exhausted`] describes,
/// reached by the one road it did not cover, and shippable in a repository.
///
/// Many small declines: the RECORD cap ends the walk.
#[test]
fn many_small_declines_spend_the_record_cap() {
    let many = tempfile::tempdir().unwrap();
    let small = many.path().join("bin");
    std::fs::create_dir_all(&small).unwrap();
    for i in 0..(MAX_ENTRIES + 32) {
        std::fs::write(small.join(format!("b{i:04}.bin")), [0xff_u8]).unwrap();
    }
    std::fs::write(many.path().join(INCLUDE_FILE), "bin/\n").unwrap();
    let text = doc(many.path());
    assert_eq!(
        text.lines().filter(|l| l.starts_with("x ")).count(),
        MAX_ENTRIES + 1,
        "the declines themselves are capped, plus the one record that says \
         the walk stopped: {text}"
    );
    assert!(
        text.trim_end().ends_with("too-many"),
        "and says why: {text}"
    );
}

/// A few large declines: the BYTE cap ends the walk, charged on the read
/// rather than on the record, because reading is what it cost.
///
/// Sized against the BUDGET, not against `MAX_FILE_BYTES`. It used to be
/// one `MAX_FILE_BYTES` chunk per file, which made the expected count
/// `MAX_TOTAL_BYTES / 256 KiB` — 4 at the old 1 MiB ceiling and **0** at
/// the 8 KiB one, an assertion that passes without reading anything and
/// proves nothing about the budget. A quarter-budget chunk keeps the
/// shape (4 fit, 4 refused) at any ceiling.
#[test]
fn a_few_large_declines_spend_the_byte_cap_on_the_read() {
    let big = tempfile::tempdir().unwrap();
    let heavy = big.path().join("bin");
    std::fs::create_dir_all(&heavy).unwrap();
    let chunk = vec![0xff_u8; MAX_TOTAL_BYTES / 4];
    for i in 0..8 {
        std::fs::write(heavy.join(format!("b{i}.bin")), &chunk).unwrap();
    }
    std::fs::write(big.path().join(INCLUDE_FILE), "bin/\n").unwrap();
    let text = doc(big.path());
    assert_eq!(
        text.lines().filter(|l| l.ends_with("binary")).count(),
        MAX_TOTAL_BYTES / chunk.len(),
        "exactly the budget's worth is read before the walk stops: {text}"
    );
    assert!(
        text.trim_end().ends_with("budget-spent"),
        "and says why: {text}"
    );
}

/// The shape [`Walk::exhausted`] names, at the size that produces its
/// numbers: 64 KiB declines, sixteen of which fit the byte budget. Its
/// docstring claimed "16 reads and 17 records" while the code read 17 —
/// nothing bound either number. Both are bound here now.
#[test]
fn declined_files_spend_the_budgets_they_cost() {
    let sized = tempfile::tempdir().unwrap();
    let (fits, _held_open) = declines_that_overrun_the_byte_budget(sized.path());
    let text = doc(sized.path());
    assert_eq!(
        (
            text.lines().skip(1).count(),
            text.lines().filter(|l| l.ends_with("binary")).count(),
        ),
        (fits + 1, fits),
        "16 reads and 17 records: sixteen files are read, charged and \
         declined, and the seventeenth ends the walk — so a 600-file \
         directory of these costs the same, the other 583 never opened: \
         {text}"
    );
    let last = format!("x bin/b{fits:02}.bin budget-spent");
    assert_eq!(
        text.lines().last(),
        Some(last.as_str()),
        "and the file that overruns is refused WITHOUT being opened — it \
         is unreadable, so a walk that opened it would say `missing`: \
         {text}"
    );
}

/// An opted-in `bin/` of declines sized so that `fits` of them spend the byte
/// budget exactly and the next one overruns it. Returns `fits`, and the
/// handle the Windows arm must hold until the walk has finished.
///
/// A sixteenth of the budget, so sixteen fit and the seventeenth overruns.
/// Hard-coding 64 KiB made `fits` 16 at the old ceiling and **0** at 8 KiB,
/// which silently moved "the seventeenth file" onto `b00.bin` and left the
/// read-count proof asserting nothing.
///
/// The file that overruns is unreadable. A walk that read before it decided
/// would answer `missing` there; only a walk that decides on the size it
/// already has can answer `budget-spent`. That is the whole proof of the
/// read count, since a read leaves no other trace in the document. Each
/// platform has one way to make a file `stat` can size but `read` cannot
/// open: mode `0o000` on Unix; on Windows a handle held open with no share
/// mode, which turns every later open into a sharing violation while the
/// directory listing and `symlink_metadata` still answer.
fn declines_that_overrun_the_byte_budget(work: &Path) -> (usize, Option<std::fs::File>) {
    let bin = work.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let block = vec![0xff_u8; MAX_TOTAL_BYTES / 16];
    let fits = MAX_TOTAL_BYTES / block.len();
    for i in 0..(fits + 4) {
        std::fs::write(bin.join(format!("b{i:02}.bin")), &block).unwrap();
    }
    let overruns = bin.join(format!("b{fits:02}.bin"));
    #[cfg(unix)]
    let held_open: Option<std::fs::File> = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&overruns, std::fs::Permissions::from_mode(0o000)).unwrap();
        None
    };
    #[cfg(windows)]
    let held_open = {
        use std::os::windows::fs::OpenOptionsExt as _;
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&overruns)
            .unwrap();
        Some(handle)
    };
    assert!(
        std::fs::read(&overruns).is_err(),
        "premise of the probe: this process cannot read the file that \
         overruns. A run with the privilege to read it (root) cannot \
         distinguish the two walks and must not report that it did."
    );
    std::fs::write(work.join(INCLUDE_FILE), "bin/\n").unwrap();
    (fits, held_open)
}

/// [`MAX_TOTAL_BYTES`] bounds bytes **read**, and this is the gap between
/// that and the document those bytes become — measured at the ceiling, on
/// the shape that maximises it.
///
/// The constant is the input budget for a git hook, so it is the number
/// anyone sizing the *downstream* cost reaches for, and it is the wrong one:
/// [`Walk::file`] writes a one-space prefix per content line, so a file that
/// is all newlines doubles. The whole budget spent on that shape hands
/// `redact` a document just over **16 KiB**. Pinned here rather than argued
/// because the doubling is invisible in the constant and the arithmetic is
/// the easy half — a cost that is worse than linear in its input pays the
/// square of a 2× error, which is the difference between a budget missed
/// and a budget missed by 4×.
#[test]
fn the_document_handed_on_is_up_to_twice_the_bytes_read() {
    // Quarter-budget files, so four spend the budget exactly. Against
    // `MAX_FILE_BYTES` this computed **zero** files at the 8 KiB ceiling and
    // the test reddened outright — the honest half of the fallout, and the
    // reason the other two sites above were worth checking rather than
    // trusting.
    let per_file = MAX_TOTAL_BYTES / 4;
    let files = MAX_TOTAL_BYTES / per_file;
    let body = "\n".repeat(per_file);
    let dir = tempfile::tempdir().unwrap();
    let mut list = String::new();
    for i in 0..files {
        std::fs::write(dir.path().join(format!("f{i}")), &body).unwrap();
        list.push_str(&format!("f{i}\n"));
    }
    std::fs::write(dir.path().join(INCLUDE_FILE), &list).unwrap();

    let document = capture_document(dir.path()).as_slice().len();
    assert!(
        document > 2 * MAX_TOTAL_BYTES,
        "the whole read budget spent on empty lines is more than twice \
         itself as a document: {document} bytes from {MAX_TOTAL_BYTES} read"
    );
    assert!(
        document < 2 * MAX_TOTAL_BYTES + 1024,
        "and the overhead beyond the doubling is per-record framing, not \
         per-byte, so it stays small: {document}"
    );
}
