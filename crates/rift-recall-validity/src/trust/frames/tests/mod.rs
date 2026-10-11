//! The tests of the frames, one file per subject.
//!
//! The production half stays one file (`../mod.rs`) because the intercept gate
//! names it as the one renderer and reads it whole; the tests it once carried in
//! a single trailing module are split here by what they are about: the wording
//! and its bytes, the reason frames' behaviour, the delta manifest's, and the
//! section a subagent's window opens with about its coordinator's reads.

mod coordinator;
mod manifest;
mod outline;
mod reasons;
mod wording;

use super::ManifestEntry;

/// A path and a hash used by every frame test, so a golden and a behaviour
/// test cannot drift onto different inputs.
const SRC: &str = "src/lib.rs";
/// A full 64-hex content hash: 12 of it is the display form, 16 the
/// argument form.
const HASH: &str = "3f9a1c2e8b7d4f60a1c2e8b7d4f603f9a1c2e8b7d4f60a1c2e8b7d4f603f9a1c";
/// A second hash, so a golden with two entries cannot pass by sharing one.
const HASH2: &str = "8b7d4f60a1c2e8b73f9a1c2e8b7d4f603f9a1c2e8b7d4f60a1c2e8b7d4f603f9";

/// Entries that name a path and claim nothing else — the shape a manifest
/// drawn from the baseline alone renders, and the one every test written
/// before entries carried a revision was written against.
fn path_only<'a>(paths: &'a [&'a str]) -> Vec<ManifestEntry<'a>> {
    paths
        .iter()
        .copied()
        .map(ManifestEntry::path_only)
        .collect()
}
