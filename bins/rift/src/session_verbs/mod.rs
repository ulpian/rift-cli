//! The session verbs an operator runs by hand, one module per concern:
//! `intake` adopts transcripts an agent already wrote (`attach`, `import`),
//! `inspect` reads sessions back (`list`, `info`), `sweep` removes what is
//! spent (`stop`, `clean`), `restore` continues prior work (`resume`) and
//! `retry` re-runs a session's parked jobs.
//! Three readers sit under them: `journals` enumerates the family's session
//! caches, `transcripts` knows where an agent keeps its own logs, and
//! `shard` resolves the checkpoint a branch claims.
//!
//! What every one of them shares lives here: the hook-refusal rail, the
//! terminal-safe rendering of caller-supplied names, and the checkpoint ref
//! the fetch ladder targets.

mod inspect;
mod intake;
mod journals;
mod restore;
mod retry;
mod shard;
mod sweep;
mod transcripts;

use crate::cli::exit;
use crate::cli::Outcome;

pub(crate) use inspect::{info, list};
pub(crate) use intake::{attach, import};
pub(crate) use journals::{family_journal, MemberJournal};
pub(crate) use restore::resume;
pub(crate) use retry::retry;
pub(crate) use sweep::{clean, document_store, shadow_refs, stop};

/// The checkpoint branch every rung of the fetch ladder targets — the same
/// ref the walker reads and the backend writes (append-only; fetches are
/// non-forced, so a rewritten remote history is refused rather than
/// absorbed).
const CHECKPOINTS_REF: &str = rift_shadow::backend::NATIVE_CHECKPOINT_REF;

/// The hook-refusal rail: attach and import must never run
/// from inside a git hook or a rift hook child. The live coverage is the
/// variables git exports to every hook it runs (`GIT_DIR` /
/// `GIT_INDEX_FILE`); `RIFT_HOOK` is a **reserved** marker — tested here,
/// honored if any rift-spawned context ever exports it, but nothing in
/// production sets it today (P84Bv wording note). The refusal is typed and
/// quiet — never a panic, never a hang.
pub(crate) fn refuse_in_hooks(verb: &str) -> Option<Outcome> {
    let hook_env = ["GIT_DIR", "GIT_INDEX_FILE", "RIFT_HOOK"]
        .iter()
        .find(|k| std::env::var_os(k).is_some());
    hook_env.map(|k| {
        Outcome::coded(
            exit::PRECONDITION,
            format!(
                "rift session {verb} must not run from a hook ({k} is set); \
                 run it from your own terminal"
            ),
            serde_json::json!({ "error": "refused_in_hook", "verb": verb, "env": k }),
        )
    })
}

fn read_yes() -> bool {
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes")
}

/// Terminal-safe rendering of caller-supplied names (branch names and ids are
/// data; the label charset is the existing neutralizer).
fn safe(s: &str) -> crate::provenance::Label {
    crate::provenance::Label::of(s)
}

fn short(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Object ids are abbreviated for display, and a short or empty id is
    /// returned whole rather than panicking on a byte range.
    #[test]
    fn abbreviation_never_slices_past_the_end() {
        assert_eq!(short("0123456789abcdef"), "0123456");
        assert_eq!(short("0123456"), "0123456");
        assert_eq!(short("012"), "012");
        assert_eq!(short(""), "");
        // A multi-byte id is never sliced THROUGH a char boundary: byte 7 is
        // a boundary in the first (h é l l o - = 7 bytes) so it truncates, and
        // is mid-character in the second, where the whole id is returned
        // rather than panicking on the range.
        assert_eq!(short("héllo-世界"), "héllo-");
        assert_eq!(short("héllo世界"), "héllo世界");
    }
}
