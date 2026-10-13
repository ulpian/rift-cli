//! What a terminal result delivered, as lines of the files it carried.
//!
//! A shell command line is recognised only where a grammar can name what it
//! read, and most cannot. The bytes the agent received can still be read:
//! each delivered line, stripped of the prefixes `grep -n`, `cat -n` and `nl`
//! add, is hashed and found in the line hashes of the files the command
//! named, at the revision on disk. Pure, like the rest of the journal: it
//! opens nothing, and the bytes and hashes arrive as arguments.
//!
//! Nothing here decides anything. It records which lines of which revision a
//! member of the command line showed the agent, and a decision leans on that
//! record only where its lever allows.

mod align;
mod chain;
mod hash;
mod normalise;
mod place;

pub use self::align::{align, Booking, Owners};
pub use self::chain::{chain_members, ChainMember, MAX_CHAIN_COMMAND_BYTES, MAX_CHAIN_MEMBERS};
pub use self::hash::{
    decode_line_hashes, encode_line_hashes, file_line_hashes, line_body, line_hash,
    MAX_LINE_HASH_FILE_BYTES,
};
pub use self::normalise::{printed_paths, readings, Reading};
pub use self::place::{place_lines, LineReading, Placement, GROUNDING_RUN};

/// The files a delivery is matched against and who may own their lines.
pub struct Matching<'a> {
    /// Each file's line hashes, by index.
    pub files: &'a [&'a [u64]],
    /// Who may own each file's lines.
    pub owners: Owners<'a>,
    /// The one file the command line named, when it named exactly one: a
    /// number printed with no path (`cat -n`, `grep -n` on one file) is
    /// read as a line of it.
    pub sole_file: Option<usize>,
}

/// The bookings a delivered output makes against `matching`'s files.
///
/// `resolve` maps a printed path to a file index; only a path it resolves
/// is read as one.
#[must_use]
pub fn match_delivery(
    matching: &Matching<'_>,
    output: &str,
    resolve: &dyn Fn(&str) -> Option<usize>,
) -> Vec<Booking> {
    let lines = delivered_readings(output, matching.sole_file, resolve);
    let placed = place_lines(matching.files, &lines);
    align(&matching.owners, &placed)
}

/// The readings of every delivered line, printed paths resolved: what
/// [`place_lines`] takes. Public for a caller that wants the placements as
/// well as the bookings.
#[must_use]
pub fn delivered_readings(
    output: &str,
    sole_file: Option<usize>,
    resolve: &dyn Fn(&str) -> Option<usize>,
) -> Vec<Vec<LineReading>> {
    let diff = output.lines().any(|line| line.starts_with("@@ "));
    let is_path = |path: &str| resolve(path).is_some();
    output
        .lines()
        .map(|line| {
            readings(line, diff, &is_path)
                .into_iter()
                .filter_map(|reading| line_reading(reading, sole_file, resolve))
                .collect()
        })
        .collect()
}

/// A reading with its printed path resolved, or `None` when the path does
/// not resolve; a printed number with no path is read against the sole
/// named file, when there is one.
fn line_reading(
    reading: Reading,
    sole_file: Option<usize>,
    resolve: &dyn Fn(&str) -> Option<usize>,
) -> Option<LineReading> {
    let anchor_file = match (&reading.anchor_path, reading.anchor_line) {
        (Some(path), _) => Some(resolve(path)?),
        (None, Some(_)) => sole_file,
        (None, None) => None,
    };
    Some(LineReading {
        hash: reading.hash,
        trivial: reading.trivial,
        anchor_file,
        anchor_line: reading.anchor_line,
    })
}

#[cfg(test)]
mod tests;
