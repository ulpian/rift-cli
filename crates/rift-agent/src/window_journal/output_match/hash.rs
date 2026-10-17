//! The line hash both sides of a match agree on, and the cache file that
//! carries a revision's line hashes between the worker that builds it and the
//! hook that reads it.
//!
//! Its own module because the hash is a format: the worker writes it to disk
//! under one version and a later hook reads it back, so the function and the
//! file grammar change together or not at all.

/// The first bytes of every cache file: the grammar and the hash it carries.
/// A file that opens any other way is a miss, never a guess.
const MAGIC: &[u8; 4] = b"rlh1";

/// The header past the magic: the line count (`u32`) and the byte count
/// (`u64`) of the revision the hashes describe.
const HEADER_BYTES: usize = 4 + 4 + 8;

/// The largest cache file a reader accepts.
///
/// A ceiling on what a hook spends, not on correctness: past it the hook
/// hashes the bytes it already holds, which is never slower than reading a
/// file eight times the size of the lines it describes.
pub const MAX_LINE_HASH_FILE_BYTES: usize = 1024 * 1024;

/// A fast, non-cryptographic hash of one line's bytes, without its line
/// ending.
///
/// Non-cryptographic on purpose: a match is a lookup, and what makes a
/// matched line a claim is the run it sits in and the revision's SHA-256 the
/// file was read at, never this value alone. Stable across versions, because
/// cache files outlive the binary that wrote them; a change here is a new
/// magic at the head of the cache file.
#[must_use]
pub fn line_hash(bytes: &[u8]) -> u64 {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let mut hash: u64 = 0x9E37_79B9_7F4A_7C15 ^ len.wrapping_mul(0xff51_afd7_ed55_8ccd);
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_le_bytes(chunk.try_into().unwrap_or_default());
        hash = (hash ^ word).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        hash ^= hash >> 31;
    }
    let mut tail = [0u8; 8];
    for (slot, byte) in tail.iter_mut().zip(chunks.remainder()) {
        *slot = *byte;
    }
    hash = (hash ^ u64::from_le_bytes(tail)).wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash ^ (hash >> 32)
}

/// One line without its ending: a trailing `\n`, then a trailing `\r`.
#[must_use]
pub fn line_body(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The hash of every line of a file's bytes, one per line as the gate counts
/// lines: a terminated run and an unterminated tail are each one line.
#[must_use]
pub fn file_line_hashes(bytes: &[u8]) -> Vec<u64> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(|line| line_hash(line_body(line)))
        .collect()
}

/// The cache file for a revision of `byte_count` bytes whose lines hash to
/// `hashes`.
#[must_use]
pub fn encode_line_hashes(hashes: &[u64], byte_count: u64) -> Vec<u8> {
    let lines = u32::try_from(hashes.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(HEADER_BYTES + hashes.len() * 8);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&lines.to_le_bytes());
    out.extend_from_slice(&byte_count.to_le_bytes());
    for hash in hashes {
        out.extend_from_slice(&hash.to_le_bytes());
    }
    out
}

/// The line hashes a cache file holds, or `None` when it is not one this
/// version wrote or does not describe a revision of `lines` lines and
/// `byte_count` bytes.
///
/// The counts are checked against the file in hand because the name is only
/// the content address: a file that says anything else about the revision is
/// not its cache, whatever it is called.
#[must_use]
pub fn decode_line_hashes(file: &[u8], lines: u32, byte_count: u64) -> Option<Vec<u64>> {
    if file.len() > MAX_LINE_HASH_FILE_BYTES {
        return None;
    }
    let (magic, rest) = file.split_first_chunk::<4>()?;
    let (stated_lines, rest) = rest.split_first_chunk::<4>()?;
    let (stated_bytes, body) = rest.split_first_chunk::<8>()?;
    let expected_len = usize::try_from(lines).ok()?.checked_mul(8)?;
    if magic != MAGIC
        || u32::from_le_bytes(*stated_lines) != lines
        || u64::from_le_bytes(*stated_bytes) != byte_count
        || body.len() != expected_len
    {
        return None;
    }
    Some(
        body.chunks_exact(8)
            .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap_or_default()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ The hash is a format: these values are what every cache file on
    /// disk was written with, and a change here must come with a new magic.
    #[test]
    fn the_line_hash_is_pinned() {
        assert_eq!(line_hash(b""), line_hash(b""));
        assert_ne!(line_hash(b"fn main() {"), line_hash(b"fn main() {}"));
        assert_ne!(line_hash(b"abcdefgh"), line_hash(b"abcdefgh\0"));
        assert_eq!(line_hash(b"fn main() {"), 0x9765_1837_1cc3_6291);
    }

    /// A line's ending is not part of it, whichever ending it has.
    #[test]
    fn line_endings_do_not_change_a_line() {
        let unix = file_line_hashes(b"one\ntwo\nthree");
        let dos = file_line_hashes(b"one\r\ntwo\r\nthree");
        assert_eq!(unix, dos);
        assert_eq!(unix.len(), 3);
        assert!(file_line_hashes(b"").is_empty());
        assert_eq!(file_line_hashes(b"a\n").len(), 1);
    }

    /// ★ A cache file round-trips, and one whose counts disagree with the
    /// revision in hand, or whose magic or length is wrong, is a miss.
    #[test]
    fn a_cache_file_round_trips_and_a_wrong_one_is_a_miss() {
        let bytes = b"alpha\nbeta\ngamma\n";
        let hashes = file_line_hashes(bytes);
        let file = encode_line_hashes(&hashes, 17);
        assert_eq!(decode_line_hashes(&file, 3, 17), Some(hashes));
        assert_eq!(decode_line_hashes(&file, 4, 17), None);
        assert_eq!(decode_line_hashes(&file, 3, 18), None);
        let mut bad_magic = file.clone();
        bad_magic[0] = b'x';
        assert_eq!(decode_line_hashes(&bad_magic, 3, 17), None);
        assert_eq!(decode_line_hashes(&file[..file.len() - 1], 3, 17), None);
        assert_eq!(decode_line_hashes(b"rl", 0, 0), None);
    }
}
