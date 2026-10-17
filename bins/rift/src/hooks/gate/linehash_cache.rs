//! The line-hash cache: one small file per revision, named by the SHA-256 of
//! its bytes, holding the hash of each of its lines.
//!
//! Separate because it is the second family cache the decision path reads,
//! and the read is admitted on the same terms as the outline cache's: keyed
//! by the family's common directory and the content address, so it can
//! neither split per checkout nor describe bytes it was not built from. The
//! detached worker writes it; a hook only reads it, and on a miss hashes the
//! bytes it already holds. Run state, rebuildable from any copy of the bytes,
//! and never the only holder of a fact.

use std::path::{Path, PathBuf};

use rift_agent::window_journal::output_match::{decode_line_hashes, MAX_LINE_HASH_FILE_BYTES};

use crate::hooks::family_state::family_state_dir;

/// The cache's subdirectory of the family's run-state directory.
const LINEHASH_DIR: &str = "linehash";

/// The cache directory of the family whose git common dir is `common_dir`.
///
/// The one place the path is spelled: the worker writes through it, the hook
/// reads through it, and the sweep collects under it.
pub(in crate::hooks) fn linehash_cache_dir(common_dir: &Path) -> PathBuf {
    family_state_dir(common_dir).join(LINEHASH_DIR)
}

/// The file one revision's line hashes are cached under.
pub(in crate::hooks) fn linehash_cache_name(content_sha256: &str) -> String {
    format!("{content_sha256}.lh")
}

/// The cached line hashes of the revision `hash`, a file of `total_lines`
/// lines and `byte_count` bytes, or `None` — a miss — when no file holds
/// them, the file is past its ceiling, or it describes another revision.
///
/// The decision path's second family-cache read: the common dir from the git
/// dir's own pointer file, then one bounded file read. No store, no
/// tokenizer, no git object database.
pub(in crate::hooks) fn load_linehash_cache(
    git_dir: &Path,
    hash: rift_core::Sha256Id,
    total_lines: u32,
    byte_count: u64,
) -> Option<Vec<u64>> {
    let common_dir = rift_git::read_common_dir(git_dir);
    let file = linehash_cache_dir(&common_dir).join(linehash_cache_name(&hash.to_hex()));
    let meta = std::fs::symlink_metadata(&file).ok()?;
    if !meta.is_file() || meta.len() > MAX_LINE_HASH_FILE_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    decode_line_hashes(&bytes, total_lines, byte_count)
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::output_match::{encode_line_hashes, file_line_hashes};

    use super::*;

    /// ★ A cache file under the revision's name is read back; one under
    /// another name, one describing other counts, and a symlink are misses.
    #[test]
    fn the_cache_is_read_only_under_its_own_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let bytes = b"alpha\nbeta\n";
        let sha = rift_agent::content_sha256(bytes);
        let dir = linehash_cache_dir(&git_dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hashes = file_line_hashes(bytes);
        std::fs::write(
            dir.join(linehash_cache_name(&sha.to_hex())),
            encode_line_hashes(&hashes, 11),
        )
        .unwrap();
        assert_eq!(load_linehash_cache(&git_dir, sha, 2, 11), Some(hashes));
        assert_eq!(load_linehash_cache(&git_dir, sha, 3, 11), None);
        let other = rift_agent::content_sha256(b"other");
        assert_eq!(load_linehash_cache(&git_dir, other, 2, 11), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                dir.join(linehash_cache_name(&sha.to_hex())),
                dir.join(linehash_cache_name(&other.to_hex())),
            )
            .unwrap();
            assert_eq!(load_linehash_cache(&git_dir, other, 2, 11), None);
        }
    }

    /// The directory is the family's, under the common dir.
    #[test]
    fn the_cache_lives_under_the_family_directory() {
        assert_eq!(
            linehash_cache_dir(Path::new("/repo/.git")),
            Path::new("/repo/.git/rift-family/linehash")
        );
    }
}
