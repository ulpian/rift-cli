//! What a terminal result delivered, as lines of the files it carried.
//!
//! A shell command line is recognised only where a grammar can name what it
//! read, and most cannot. The bytes the agent received can still be read:
//! each delivered line, stripped of the prefixes `grep -n`, `cat -n` and `nl`
//! add, is hashed and found in the line hashes of the files the command
//! named, at the revision on disk. Pure, like the rest of the journal: it
//! opens nothing, and the bytes and hashes arrive as arguments.

mod hash;

pub use self::hash::{
    decode_line_hashes, encode_line_hashes, file_line_hashes, line_body, line_hash,
    MAX_LINE_HASH_FILE_BYTES,
};
