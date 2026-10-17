//! The **session-id trust boundary**.
//!
//! A session id arrives on hook stdin, written by the agent, and Rift turns it
//! into filenames: `<sessions>/<id>.json` for the transient
//! session cache, `.<id>.json.<pid>-<seq>.tmp` for each writer's atomic write,
//! and the
//! first-writer-wins claim markers beside them. Nothing validated the string,
//! so an id of `../../../../etc/cron.d/evil` escaped the directory and gave a
//! hostile payload an arbitrary-file-write primitive with the developer's own
//! privileges — from a hook that, by contract, must never fail the agent and so
//! swallows every error silently.
//!
//! One predicate closes it, applied in two places: at the wire
//! ([`build_raw_hook`](crate::wire::build_raw_hook)), where an unsafe id defers
//! the whole hook before a single row is written, and at the sink
//! ([`cache_path`](crate::session_cache::cache_path)), so a caller that obtains
//! an id from anywhere else — a database row, a future claim marker — still
//! cannot address a path outside the sessions directory.
//!
//! The rule deliberately accepts what real agents actually emit (UUIDs, ULIDs,
//! and the `agent-1234` forms) and nothing that can be read as a path.

/// Longest accepted session id. Real ids are UUIDs (36) or ULIDs (26); the
/// ceiling exists so a pathological id cannot push a filename past a
/// filesystem's own limit and fail the write in a less obvious way.
pub const MAX_SESSION_ID_BYTES: usize = 200;

/// Whether `session_id` may be used as a path component.
///
/// Accepts a non-empty, at most [`MAX_SESSION_ID_BYTES`] byte string of ASCII
/// alphanumerics, `-`, `_` and `.`, that does not begin with `.`. That rejects,
/// by construction:
///
/// - `/` and `\` — no traversal and no absolute path, on any platform;
/// - `.` and `..` and every id that starts with one (a leading dot also keeps
///   an id out of the `.<id>.…tmp` namespace the atomic writes own, and out of
///   the hidden-file namespace generally);
/// - NUL and every other control byte, which truncate paths in C APIs;
/// - non-ASCII, which normalizes differently across filesystems (two distinct
///   ids could otherwise name one file, silently merging two sessions);
/// - `:` and `*`/`?`/`<`/`>`/`|`/`"` — Windows path punctuation and stream
///   syntax.
#[must_use]
pub fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= MAX_SESSION_ID_BYTES
        && !session_id.starts_with('.')
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ids_real_agents_emit_are_accepted() {
        for id in [
            "f5927747-7be6-4b5e-b379-579c7193520a", // Claude Code / Cursor UUID
            "01JQ8ZK6M3S9V0N7R2T4W5X6Y7",           // ULID
            "session_2026-08-06T00.00.00Z",
            "a",
            &"9".repeat(MAX_SESSION_ID_BYTES),
        ] {
            assert!(is_safe_session_id(id), "must accept {id}");
        }
    }

    #[test]
    fn an_id_that_can_be_read_as_a_path_is_refused() {
        for id in [
            "",
            "..",
            ".",
            "../evil",
            "../../../../etc/cron.d/evil",
            "..\\evil",
            "/etc/passwd",
            "a/b",
            ".hidden",
            ".s.tmp",
            "nul\0byte",
            "new\nline",
            "space id",
            "sess:stream",
            "sess*glob",
            "sess\"quote",
            "sessión",
            &"9".repeat(MAX_SESSION_ID_BYTES + 1),
        ] {
            assert!(!is_safe_session_id(id), "must refuse {id:?}");
        }
    }

    proptest::proptest! {
        /// Whatever an agent sends, an accepted id is one path component and
        /// stays inside the directory it is joined to.
        #[test]
        fn an_accepted_id_is_always_exactly_one_inner_component(id in "(?s).{0,64}") {
            if is_safe_session_id(&id) {
                let file = format!("{id}.json");
                let joined = std::path::Path::new("/sessions").join(&file);
                proptest::prop_assert_eq!(joined.components().count(), 3);
                proptest::prop_assert!(joined.starts_with("/sessions"));
                // The two assertions above are BLIND to the class this
                // generator can now draw, and that is the whole lesson: `a\nb`
                // is one path component too, so an accepted id carrying a
                // newline satisfies both while being exactly the injection
                // this screen exists to stop. A session id is written into the
                // cache file name AND read back out of line-oriented places.
                // So: the file name is the id, byte for byte…
                proptest::prop_assert_eq!(
                    joined.file_name().and_then(std::ffi::OsStr::to_str),
                    Some(file.as_str())
                );
                // …and it carries nothing a line-oriented reader could split on
                // or a C API could truncate at.
                proptest::prop_assert!(
                    !id.chars().any(char::is_control),
                    "an accepted id carries a control character: {:?}", id
                );
            }
        }

        /// The same claim, where the screen actually has a decision to make.
        ///
        /// Measured, not assumed: an arbitrary 64-character draw is essentially
        /// never ACCEPTED at all — every byte has to be in the allowed set — so
        /// the property above can draw newlines all day and still never see an
        /// accepted id that contains one. Reach is not assembly. This one is
        /// built from the accepted alphabet with one control byte spliced in,
        /// which is the shape an agent would actually send to smuggle a line
        /// break into a filename.
        #[test]
        fn an_id_of_accepted_bytes_with_one_control_byte_spliced_in_is_refused(
            head in "[a-zA-Z0-9]{1,8}",
            ctrl in "[\u{0}\t\n\r\u{1b}\u{7f}]",
            tail in "[a-zA-Z0-9._-]{0,8}",
        ) {
            let id = format!("{head}{ctrl}{tail}");
            proptest::prop_assert!(
                !is_safe_session_id(&id),
                "accepted an id carrying a control byte: {:?}", id
            );
        }
    }
}
