//! The hook-timing ring: the last [`SLOTS`] hook runs of one family on this
//! machine, one fixed-size record each, so `rift doctor --report` can say
//! p50/p95/max per verb from real use without any telemetry.
//!
//! The cost on the hook is the point of the layout. A hook writes one
//! 16-byte header read, one 128-byte record and one header write into a file
//! it never reads whole: slot = counter mod [`SLOTS`], so the file stops
//! growing at 512 KiB and the oldest record is the one overwritten. Two hooks
//! racing on the counter can land in one slot and one record is lost; a
//! percentile over thousands of runs does not notice, and a lock would cost
//! every hook to save a sample.
//!
//! A record holds a verb, the total and the three slowest phases, as
//! microseconds and lowercase tags. Never a path, a session or any content:
//! the report prints it beside the scrubbed doctor output.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

/// The ring's file name inside the family's directory under the cache root.
pub(crate) const RING_FILE: &str = "hook-timings.bin";
/// How many runs the ring keeps.
const SLOTS: u64 = 4096;
/// The file's magic, which is also its version.
const MAGIC: [u8; 8] = *b"RIFTHKT1";
/// Header: the magic, then the little-endian count of records ever written.
const HEADER: u64 = 16;
/// One record's size on disk.
const RECORD: usize = 128;
/// What marks a slot as written.
const RECORD_TAG: [u8; 4] = *b"rht1";
/// Widths of the two tag fields.
const VERB_BYTES: usize = 32;
const PHASE_BYTES: usize = 24;
/// The run time a hook is measured against: the git-latency budget.
const BUDGET_US: u32 = 100_000;

/// One run as the ring holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) verb: String,
    pub(crate) total_us: u32,
    pub(crate) slowest: Vec<(String, u32)>,
}

/// The ring under a family's directory.
pub(crate) fn ring_path(family_dir: &Path) -> PathBuf {
    family_dir.join(RING_FILE)
}

/// Append one run. Best-effort and silent: a ring that cannot be written
/// costs a sample, never a hook. The family's directory is never created
/// here, so a hook in a repository not set up on this machine leaves nothing.
pub(crate) fn append(family_dir: &Path, verb: &str, total_us: u128, phases: &[(&str, u128)]) {
    let record = encode(verb, total_us, phases);
    let Ok(mut file) = open_for_append(&ring_path(family_dir)) else {
        return;
    };
    let _ = write_record(&mut file, &record);
}

fn open_for_append(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// Read the counter, write the record at its slot, then the next counter.
fn write_record(file: &mut File, record: &[u8; RECORD]) -> std::io::Result<()> {
    let mut header = [0u8; 16];
    let counter = match file.read_exact(&mut header) {
        Ok(()) if header.first_chunk::<8>() == Some(&MAGIC) => header
            .last_chunk::<8>()
            .map_or(0, |count| u64::from_le_bytes(*count)),
        _ => 0,
    };
    let slot = counter % SLOTS;
    file.seek(SeekFrom::Start(HEADER + slot * RECORD as u64))?;
    file.write_all(record)?;
    let mut next = MAGIC.to_vec();
    next.extend_from_slice(&counter.wrapping_add(1).to_le_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&next)
}

/// The record bytes: tag, verb, total, then the three slowest phases (a
/// phase run twice is summed, since the question is where the time went).
fn encode(verb: &str, total_us: u128, phases: &[(&str, u128)]) -> [u8; RECORD] {
    let mut out = Vec::with_capacity(RECORD);
    out.extend_from_slice(&RECORD_TAG);
    put_tag(&mut out, verb, VERB_BYTES);
    out.extend_from_slice(&clamp_us(total_us).to_le_bytes());
    for (name, us) in slowest_three(phases) {
        put_tag(&mut out, name, PHASE_BYTES);
        out.extend_from_slice(&clamp_us(us).to_le_bytes());
    }
    out.resize(RECORD, 0);
    let mut record = [0u8; RECORD];
    record.copy_from_slice(&out);
    record
}

fn slowest_three<'a>(phases: &[(&'a str, u128)]) -> Vec<(&'a str, u128)> {
    let mut summed: Vec<(&str, u128)> = Vec::new();
    for (name, us) in phases {
        match summed.iter_mut().find(|(seen, _)| seen == name) {
            Some(entry) => entry.1 = entry.1.saturating_add(*us),
            None => summed.push((name, *us)),
        }
    }
    summed.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    summed.truncate(3);
    summed
}

fn clamp_us(us: u128) -> u32 {
    u32::try_from(us).unwrap_or(u32::MAX)
}

/// A tag field: lowercase ASCII letters, digits and `_ : -`, anything else
/// as `_`, cut to `width` and padded with NULs.
fn put_tag(out: &mut Vec<u8>, text: &str, width: usize) {
    let start = out.len();
    out.extend(
        text.bytes()
            .take(width)
            .map(|b| match b.to_ascii_lowercase() {
                c @ (b'a'..=b'z' | b'0'..=b'9' | b'_' | b':' | b'-') => c,
                _ => b'_',
            }),
    );
    out.resize(start + width, 0);
}

/// Every written record of the ring under `family_dir`; empty when there is
/// no ring or it is not one.
pub(crate) fn read_runs(family_dir: &Path) -> Vec<Run> {
    let Ok(bytes) = std::fs::read(ring_path(family_dir)) else {
        return Vec::new();
    };
    let Some((head, body)) = bytes.split_at_checked(16) else {
        return Vec::new();
    };
    if head.first_chunk::<8>() != Some(&MAGIC) {
        return Vec::new();
    }
    body.chunks_exact(RECORD)
        .take(usize::try_from(SLOTS).unwrap_or(usize::MAX))
        .filter_map(decode)
        .collect()
}

fn decode(record: &[u8]) -> Option<Run> {
    let (tag, rest) = record.split_at_checked(RECORD_TAG.len())?;
    if tag != RECORD_TAG {
        return None;
    }
    let (verb, rest) = take_tag(rest, VERB_BYTES)?;
    let (total_us, mut rest) = take_u32(rest)?;
    let mut slowest = Vec::new();
    for _ in 0..3 {
        let (name, after) = take_tag(rest, PHASE_BYTES)?;
        let (us, after) = take_u32(after)?;
        rest = after;
        if !name.is_empty() {
            slowest.push((name, us));
        }
    }
    Some(Run {
        verb,
        total_us,
        slowest,
    })
}

fn take_tag(bytes: &[u8], width: usize) -> Option<(String, &[u8])> {
    let (field, rest) = bytes.split_at_checked(width)?;
    let text: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| char::from(*b))
        .collect();
    Some((text, rest))
}

fn take_u32(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let (field, rest) = bytes.split_first_chunk::<4>()?;
    Some((u32::from_le_bytes(*field), rest))
}

/// Per verb: how many runs, p50/p95/max in milliseconds, and the share of
/// runs over the 100 ms budget. Verbs in name order.
pub(crate) fn summary(runs: &[Run]) -> serde_json::Value {
    let mut by_verb: std::collections::BTreeMap<&str, Vec<u32>> = std::collections::BTreeMap::new();
    for run in runs {
        by_verb.entry(&run.verb).or_default().push(run.total_us);
    }
    let verbs: serde_json::Map<String, serde_json::Value> = by_verb
        .into_iter()
        .map(|(verb, mut totals)| {
            totals.sort_unstable();
            (verb.to_owned(), verb_summary(&totals))
        })
        .collect();
    serde_json::json!({ "runs": runs.len(), "verbs": verbs })
}

fn verb_summary(sorted: &[u32]) -> serde_json::Value {
    let over = sorted.iter().filter(|us| **us > BUDGET_US).count();
    serde_json::json!({
        "runs": sorted.len(),
        "p50_ms": ms(nearest_rank(sorted, 50)),
        "p95_ms": ms(nearest_rank(sorted, 95)),
        "max_ms": ms(sorted.last().copied().unwrap_or(0)),
        "over_100ms_share": share(over, sorted.len()),
    })
}

/// The nearest-rank percentile of an ascending list.
fn nearest_rank(sorted: &[u32], percent: usize) -> u32 {
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted.get(rank - 1).copied().unwrap_or(0)
}

/// Microseconds as milliseconds to one decimal.
fn ms(us: u32) -> f64 {
    f64::from(us / 100) / 10.0
}

/// `part / whole` to three decimals; zero of nothing.
fn share(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    let permille = part.saturating_mul(1000) / whole;
    f64::from(u32::try_from(permille).unwrap_or(1000)) / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_reads_back_as_written_with_its_three_slowest_phases_summed() {
        let dir = tempfile::tempdir().unwrap();
        append(
            dir.path(),
            "claude-code:PreToolUse",
            120_000,
            &[
                ("discover", 5),
                ("ingest", 70),
                ("redact", 30),
                ("redact", 50),
                ("enrich", 1),
            ],
        );

        let runs = read_runs(dir.path());

        assert_eq!(
            runs,
            vec![Run {
                verb: "claude-code:pretooluse".into(),
                total_us: 120_000,
                slowest: vec![
                    ("redact".into(), 80),
                    ("ingest".into(), 70),
                    ("discover".into(), 5)
                ],
            }]
        );
    }

    #[test]
    fn the_ring_stops_growing_at_its_slot_count_and_overwrites_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(SLOTS + 3) {
            append(dir.path(), "post-commit", u128::from(i), &[]);
        }

        let len = std::fs::metadata(ring_path(dir.path())).unwrap().len();
        let runs = read_runs(dir.path());

        assert_eq!(len, HEADER + SLOTS * RECORD as u64);
        assert_eq!(runs.len(), 4096);
        let totals: Vec<u32> = runs.iter().take(4).map(|r| r.total_us).collect();
        assert_eq!(
            totals,
            vec![4096, 4097, 4098, 3],
            "the first three slots were reused"
        );
    }

    #[test]
    fn a_missing_family_directory_is_never_created() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("not-set-up");
        append(&absent, "post-commit", 1, &[]);
        assert!(!absent.exists());
        assert!(read_runs(&absent).is_empty());
    }

    #[test]
    fn a_file_that_is_not_a_ring_reads_as_no_runs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(ring_path(dir.path()), vec![7u8; 400]).unwrap();
        assert!(read_runs(dir.path()).is_empty());
    }

    #[test]
    fn the_summary_names_percentiles_and_the_share_over_budget_per_verb() {
        let run = |verb: &str, ms: u32| Run {
            verb: verb.into(),
            total_us: ms * 1000,
            slowest: Vec::new(),
        };
        let mut runs: Vec<Run> = (1..=20).map(|ms| run("post-commit", ms * 10)).collect();
        runs.push(run("agent:stop", 5));

        let s = summary(&runs);

        assert_eq!(s["runs"], 21);
        let pc = &s["verbs"]["post-commit"];
        assert_eq!(pc["runs"], 20);
        assert_eq!(pc["p50_ms"], 100.0);
        assert_eq!(pc["p95_ms"], 190.0);
        assert_eq!(pc["max_ms"], 200.0);
        assert_eq!(pc["over_100ms_share"], 0.5);
        assert_eq!(s["verbs"]["agent:stop"]["over_100ms_share"], 0.0);
    }
}
