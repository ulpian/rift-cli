//! The new side of each chunk of a one-file update, read out of a patch
//! document by the grammar pinned in `fixtures/codex-tool-field-surface.md`.
//!
//! No line number comes out of this module: the patch states none. What comes
//! out is, per chunk, the text it leaves in the file and how many lines it took
//! out, so the caller can look for that text in the bytes on disk afterwards —
//! the same evidence one replacement is held to, chunk by chunk.

use rift_agent::{EditKind, Replacement};

use crate::patch;

/// What a patch did to the one file it updates, as far as its chunks say.
///
/// Anything this reading cannot stand behind — an add section, a line that
/// is none of the grammar's forms, an update with no chunk — is
/// [`EditKind::Other`]: the change is recorded and no site is claimed.
pub(crate) fn update_kind(patch_text: &str) -> EditKind {
    match update_chunks(patch_text) {
        Some(regions) if !regions.is_empty() => EditKind::Regions { regions },
        _ => EditKind::Other,
    }
}

/// One chunk of an update, while it is being read.
#[derive(Default)]
struct Chunk<'a> {
    /// The context and `+` lines, in order: the text the chunk leaves.
    new: Vec<&'a str>,
    /// How many context and `-` lines it had: the lines it took out.
    old: u32,
    /// Whether any change line was seen, so an empty chunk is no region.
    seen: bool,
}

impl Chunk<'_> {
    /// The finished chunk as a replacement, or nothing when it was empty.
    ///
    /// The new side is joined without a final line break so that its span is
    /// exactly its count of lines, which is how the old side is counted: the
    /// shift the caller checks is the difference of two counts in one
    /// convention.
    fn take(&mut self) -> Option<Replacement> {
        let chunk = std::mem::take(self);
        chunk.seen.then(|| Replacement {
            new_string: chunk.new.join("\n"),
            old_string_lines: chunk.old,
            replace_all: false,
        })
    }
}

/// The chunks of the update section, or `None` when a line in it is not one
/// the grammar spells.
fn update_chunks(patch_text: &str) -> Option<Vec<Replacement>> {
    let mut lines = patch_text.lines();
    lines.find(|line| line.trim_start().starts_with(patch::UPDATE))?;
    let mut regions = Vec::new();
    let mut chunk = Chunk::default();
    for line in lines {
        if let Some(rest) = line.strip_prefix(' ') {
            chunk.new.push(rest);
            chunk.old = chunk.old.saturating_add(1);
        } else if let Some(rest) = line.strip_prefix('+') {
            chunk.new.push(rest);
        } else if line.starts_with('-') {
            chunk.old = chunk.old.saturating_add(1);
        } else {
            let marker = line.trim();
            if marker != patch::END_OF_FILE && !marker.starts_with(patch::CHUNK) {
                // The end of the patch closes the last chunk; anything else
                // is a form this reading does not know, and a guess about it
                // would be a guess about where the file changed.
                return (marker == patch::END_PATCH).then(|| {
                    regions.extend(chunk.take());
                    regions
                });
            }
            regions.extend(chunk.take());
            continue;
        }
        chunk.seen = true;
    }
    regions.extend(chunk.take());
    Some(regions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(new_string: &str, old_string_lines: u32) -> Replacement {
        Replacement {
            new_string: new_string.to_owned(),
            old_string_lines,
            replace_all: false,
        }
    }

    #[test]
    fn each_chunk_of_an_update_is_one_region_with_its_new_side_whole() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@ fn a\n a\n-b\n+B\n+B2\n c\n@@\n x\n-y\n*** End of File\n*** End Patch\n";
        assert_eq!(
            update_kind(patch),
            EditKind::Regions {
                regions: vec![region("a\nB\nB2\nc", 3), region("x", 2)]
            }
        );
    }

    #[test]
    fn a_chunk_with_no_header_before_it_is_still_a_chunk() {
        let patch = "*** Begin Patch\n*** Update File: a.rs\n-old\n+new\n*** End Patch";
        assert_eq!(
            update_kind(patch),
            EditKind::Regions {
                regions: vec![region("new", 1)]
            }
        );
    }

    #[test]
    fn a_patch_this_reading_cannot_stand_behind_claims_no_region() {
        for patch in [
            "*** Begin Patch\n*** Add File: a.rs\n+x\n*** End Patch",
            "*** Begin Patch\n*** Update File: a.rs\n*** End Patch",
            "*** Begin Patch\n*** Update File: a.rs\n@@\n a\nstray\n*** End Patch",
            "*** Begin Patch\n*** Update File: a.rs\n@@\n a\n\n b\n*** End Patch",
        ] {
            assert_eq!(update_kind(patch), EditKind::Other, "{patch}");
        }
    }
}
