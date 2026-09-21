//! The record of what the agent itself wrote: the two moments of one edit and
//! the line each leaves in the window journal.
//!
//! Separate from the ingest half because this is a decision-path module. It
//! opens no store and reads nothing but the file the payload named, and the
//! walk that proves that is easier to keep true when the capture side lives in
//! its own files.

use std::path::Path;

use rift_shadow::hooks::AdapterRegistry;

use crate::hooks::gate::{gate_file, gate_rel_path, gate_turn, payload_cwd};

/// What this hook does about a change to a file, if anything.
///
/// The two moments are one mechanism. Before a whole-file write lands, the
/// hash of the bytes the payload carries is written down; after it lands, the
/// file is hashed and the two are compared. That equality is what separates
/// "the agent wrote these bytes" from "something reformatted them on the way
/// in", and it is the only reason a window that never read a file may be said
/// to hold it.
enum EditStep {
    /// A whole-file write is about to happen: record the hash its own bytes
    /// have, so that afterwards there is something to check against.
    Expect,
    /// A change has happened: hash the file and record what follows.
    Record,
}

/// Which moment of an edit a `(family, verb)` is, if it is one.
///
/// `PostToolUse` is the *successful* completion of a tool call on every family
/// whose event list this tree pins — a call that failed announces itself under
/// its own name and never reaches here — so a change is recorded only where
/// one really happened. A payload that names no edit at all is the adapter's
/// answer, not this function's.
fn edit_step(verb: &str) -> Option<EditStep> {
    match verb {
        "PreToolUse" => Some(EditStep::Expect),
        "PostToolUse" => Some(EditStep::Record),
        _ => None,
    }
}

/// What both moments of an edit need beyond the call itself: the journal the
/// line goes in, and which turn and instant it belongs to.
///
/// One struct rather than three arguments repeated twice, because the two
/// moments must agree about all three or the expectation and the change it was
/// for land in different windows.
struct EditMoment {
    journal: crate::window_journal::WindowJournal,
    turn: u32,
    at_ms: u64,
}

/// The edit record itself, guarded and timed by the step that calls it.
///
/// It opens no store, hashes at most the policy's inline cap, and every
/// refusal below leaves the journal knowing less than really happened — which
/// costs a saving and can never claim bytes a window does not hold.
pub(in crate::hooks) fn edit_record(
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) {
    // The same lever, so the same switch: a repository that turned the read
    // journal off keeps no journal, and what a window wrote is a line of one.
    if effective.read_deny == rift_recall_policy::Mode::Off || raw.session.is_empty() {
        return;
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return;
    };
    let Some(step) = edit_step(&raw.verb) else {
        return;
    };
    let Some(request) = adapter.edit_request(&raw.verb, &raw.payload) else {
        return;
    };
    let facts = adapter.facts(&raw.verb, &raw.payload);
    let cwd = payload_cwd(&facts);
    let window = facts.child_session;
    let moment = EditMoment {
        journal: crate::window_journal::WindowJournal::open(
            work_tree,
            &raw.session,
            window.as_deref(),
        ),
        turn: gate_turn(raw, git_dir),
        at_ms: u64::try_from(raw.occurred_at_ms).unwrap_or_default(),
    };
    match step {
        EditStep::Expect => note_expected_write(&moment, work_tree, &request, cwd.as_deref()),
        EditStep::Record => note_landed_change(
            &moment,
            work_tree,
            &request,
            cwd.as_deref(),
            effective.max_inline_hash_bytes,
        ),
    }
}

/// Write down the hash a whole-file write promised, so that after it lands
/// there is something to check the file against.
///
/// Only a whole-file write has an expectation to record: it is the one payload
/// that carries the file it is about to make.
fn note_expected_write(
    moment: &EditMoment,
    work_tree: &Path,
    request: &rift_agent::EditRequest,
    cwd: Option<&Path>,
) {
    let rift_agent::EditKind::Write { content_sha256 } = request.kind else {
        return;
    };
    let Ok(root) = work_tree.canonicalize() else {
        return;
    };
    let Some(path) = gate_rel_path(&root, &request.path, cwd) else {
        return;
    };
    let _ = moment
        .journal
        .append(&rift_agent::window_journal::Line::Expect {
            turn: moment.turn,
            path,
            hash: content_sha256,
            at_ms: moment.at_ms,
        });
}

/// Record what a change that has already happened left behind, and whether the
/// window that made it can be said to hold the result.
fn note_landed_change(
    moment: &EditMoment,
    work_tree: &Path,
    request: &rift_agent::EditRequest,
    cwd: Option<&Path>,
    max_inline_hash_bytes: u64,
) {
    // The bytes the change left behind: this checkout's own spelling of the
    // path, the revision it is at now, and how long it is. A file this hook
    // cannot stand behind — outside the checkout, past the inline cap, not
    // text — leaves no line at all.
    let Some(file) = gate_file(work_tree, &request.path, cwd, max_inline_hash_bytes) else {
        return;
    };
    let held = moment.journal.load().journal;
    let prior = crate::window_journal::prior_revision(&held, &file.rel_path);
    let facts = crate::window_journal::edit_facts(
        &request.kind,
        file.text(),
        file.total_lines,
        file.hash,
        crate::window_journal::expected_write(&held, &file.rel_path),
        prior,
    );
    if facts.is_silent() && prior.is_some_and(|p| p.hash == file.hash) {
        // Nothing about the file changed and nothing was learned: a call that
        // did not land leaves the journal exactly as it found it.
        return;
    }
    let _ = moment
        .journal
        .append(&rift_agent::window_journal::Line::Edit {
            turn: moment.turn,
            path: file.rel_path,
            hash_old: prior.map(|p| p.hash),
            hash_new: file.hash,
            sites: facts.sites,
            shift: facts.shift,
            full: facts.full,
            at_ms: moment.at_ms,
        });
}

/// What a change to a file leaves behind: the hash it promised, the hash it
/// has, and whether the window that made it holds the result.
#[cfg(test)]
mod edit_record_tests {
    use rift_agent::window_journal::{Interval, Line};
    use rift_shadow::hooks::RawHook;

    use super::*;
    use crate::hooks::gate::read_gate;

    /// A hook as the record reads one.
    fn hook(agent: &str, verb: &str, session: &str, payload: serde_json::Value) -> RawHook {
        RawHook {
            agent: rift_core::AgentId::new(agent),
            verb: verb.to_owned(),
            session: session.to_owned(),
            turn: Some(7),
            occurred_at_ms: 1_700_000_000_000,
            payload,
        }
    }

    /// A whole-file write, as the pinned fields spell one.
    fn write_of(path: &Path, content: &str) -> serde_json::Value {
        serde_json::json!({
            "tool_name": "Write",
            "tool_input": {
                "file_path": path.display().to_string(),
                "content": content,
            },
        })
    }

    /// A work tree, with nothing in it yet.
    fn checkout() -> tempfile::TempDir {
        tempfile::tempdir().expect("a work tree")
    }

    fn lines_of(work_tree: &Path, session: &str) -> Vec<Line> {
        crate::window_journal::WindowJournal::open(work_tree, session, None)
            .load()
            .journal
            .lines()
            .to_vec()
    }

    /// The levers as this repository ships them: observe, with the defaults.
    fn observing() -> rift_recall_policy::Effective {
        rift_recall_policy::resolve(
            &rift_recall_policy::ServePolicy::default(),
            "claude-code",
            crate::recall::family_capability(&rift_core::AgentId::CLAUDE_CODE),
        )
    }

    fn record(raw: &RawHook, work_tree: &Path) {
        edit_record(
            &AdapterRegistry::with_builtin(),
            raw,
            &work_tree.join(".git"),
            work_tree,
            &observing(),
        );
    }

    /// The whole mechanism in one pass: the hash the payload promised is
    /// written down before the write, the file is hashed after it, and the two
    /// agreeing is what says the window holds every line of a file it never
    /// read.
    #[test]
    fn a_write_whose_bytes_survive_grants_coverage_of_every_line() {
        let tmp = checkout();
        let file = tmp.path().join("new.rs");
        let body = "one\ntwo\nthree\n";
        record(
            &hook("claude-code", "PreToolUse", "s-1", write_of(&file, body)),
            tmp.path(),
        );
        std::fs::write(&file, body).expect("the write lands");
        record(
            &hook("claude-code", "PostToolUse", "s-1", write_of(&file, body)),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-1");
        let [Line::Expect { path, .. }, Line::Edit {
            path: edited,
            hash_old,
            sites,
            full,
            ..
        }] = lines.as_slice()
        else {
            panic!("an expectation and the change it was for, got {lines:?}");
        };
        assert_eq!(path, "new.rs", "the checkout's own spelling of a new file");
        assert_eq!(edited, "new.rs");
        assert_eq!(*hash_old, None, "the file did not exist to have a revision");
        assert_eq!(sites.as_slice(), [Interval::new(1, 3).expect("a run")]);
        assert!(
            *full,
            "the agent authored every byte and they are all there"
        );

        let journal = crate::window_journal::WindowJournal::open(tmp.path(), "s-1", None)
            .load()
            .journal;
        let hash = match lines.get(1) {
            Some(Line::Edit { hash_new, .. }) => *hash_new,
            _ => panic!("the change names a revision"),
        };
        assert!(
            journal
                .coverage("new.rs", hash)
                .contains(Interval::new(1, 3).expect("a run")),
            "writing a file in full is holding it in full"
        );
    }

    /// ★ A formatter that rewrites the bytes on their way to disk breaks the
    /// equality the whole claim rests on, so the window is credited with
    /// nothing — the case that separates a correct mechanism from a plausible
    /// one.
    #[test]
    fn a_write_whose_bytes_a_formatter_changed_grants_no_coverage() {
        let tmp = checkout();
        let file = tmp.path().join("fmt.rs");
        let payload = write_of(&file, "fn  f( ) {}\n");
        record(
            &hook("claude-code", "PreToolUse", "s-2", payload.clone()),
            tmp.path(),
        );
        // What a format-on-save hook left there instead.
        std::fs::write(&file, "fn f() {}\n").expect("the formatter wins");
        record(
            &hook("claude-code", "PostToolUse", "s-2", payload),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-2");
        let [Line::Expect { .. }, Line::Edit {
            hash_new,
            sites,
            full,
            ..
        }] = lines.as_slice()
        else {
            panic!("an expectation and the change it was for, got {lines:?}");
        };
        assert!(sites.is_empty(), "no place to point at");
        assert!(!*full, "the file is not the bytes the payload carried");
        let journal = crate::window_journal::WindowJournal::open(tmp.path(), "s-2", None)
            .load()
            .journal;
        assert!(
            journal.coverage("fmt.rs", *hash_new).is_empty(),
            "a revision nobody read and nobody wrote is held by nobody"
        );
    }

    /// A replacement, as the pinned fields spell one.
    fn edit_of(path: &Path, old: &str, new: &str, all: bool) -> serde_json::Value {
        serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {
                "file_path": path.display().to_string(),
                "old_string": old,
                "new_string": new,
                "replace_all": all,
            },
        })
    }

    /// A file this window has read every line of, so that what follows an edit
    /// of it is the question of whether it still holds all of it.
    fn read_in_full(work_tree: &Path, session: &str, path: &str, body: &str) {
        let file = work_tree.join(path);
        std::fs::write(&file, body).expect("the file");
        read_gate(
            &AdapterRegistry::with_builtin(),
            &hook(
                "claude-code",
                "PreToolUse",
                session,
                serde_json::json!({
                    "tool_name": "Read",
                    "tool_input": {
                        "file_path": file.display().to_string(),
                        "offset": 1,
                        "limit": 1_000,
                    },
                }),
            ),
            &work_tree.join(".git"),
            work_tree,
            &observing(),
        );
    }

    /// The site is where the text the agent wrote actually sits in the bytes
    /// it left behind, in the numbering a reader of those bytes will see — not
    /// an older line number moved along by an offset.
    #[test]
    fn sites_are_in_new_numbering_with_the_shift() {
        let tmp = checkout();
        read_in_full(tmp.path(), "s-10", "shift.rs", "a\nb\nc\nd\n");
        let file = tmp.path().join("shift.rs");
        std::fs::write(&file, "a\nb\nB2\nB3\nc\nd\n").expect("the edit lands");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-10",
                edit_of(&file, "b\n", "b\nB2\nB3\n", false),
            ),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-10");
        let [_, Line::Edit {
            sites, shift, full, ..
        }] = lines.as_slice()
        else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert_eq!(
            sites.as_slice(),
            [Interval::new(2, 4).expect("a run")],
            "lines 2-4 of the file as it is now, where line 2 was all there was"
        );
        assert_eq!(*shift, 2, "the file is two lines longer than it was");
        assert!(
            *full,
            "it held every line before, and it wrote the difference"
        );
    }

    /// ★ The same text in two places is two places it could have gone, and a
    /// mechanism that picks one is guessing — so it picks neither and the
    /// window is credited with nothing.
    #[test]
    fn an_edit_whose_new_string_occurs_twice_grants_no_coverage() {
        let tmp = checkout();
        read_in_full(tmp.path(), "s-11", "twice.rs", "keep\nx\n");
        let file = tmp.path().join("twice.rs");
        std::fs::write(&file, "keep\nkeep\n").expect("the edit lands");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-11",
                edit_of(&file, "x\n", "keep\n", false),
            ),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-11");
        let [_, Line::Edit {
            hash_new,
            sites,
            full,
            ..
        }] = lines.as_slice()
        else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert!(sites.is_empty(), "two candidates are no site at all");
        assert!(!*full);
        let journal = crate::window_journal::WindowJournal::open(tmp.path(), "s-11", None)
            .load()
            .journal;
        assert!(journal.coverage("twice.rs", *hash_new).is_empty());
    }

    /// Every occurrence when every occurrence was asked for — and none at all
    /// when the file's own growth says these replacements were not the only
    /// change in it.
    #[test]
    fn replace_all_records_every_site_or_none() {
        let tmp = checkout();
        read_in_full(tmp.path(), "s-12", "all.rs", "a\nold\nb\nold\n");
        let file = tmp.path().join("all.rs");
        std::fs::write(&file, "a\nnew\nb\nnew\n").expect("the edit lands");
        let call = edit_of(&file, "old\n", "new\n", true);
        record(
            &hook("claude-code", "PostToolUse", "s-12", call.clone()),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-12");
        let [_, Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert_eq!(
            sites.as_slice(),
            [
                Interval::new(2, 2).expect("a run"),
                Interval::new(4, 4).expect("a run"),
            ]
        );
        assert!(*full);

        // The same call, against bytes something else also added a line to.
        read_in_full(tmp.path(), "s-13", "drift.rs", "a\nold\nb\nold\n");
        let drifted = tmp.path().join("drift.rs");
        std::fs::write(&drifted, "a\nnew\nb\nnew\nextra\n").expect("and a formatter");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-13",
                edit_of(&drifted, "old\n", "new\n", true),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-13");
        let [_, Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert!(
            sites.is_empty(),
            "two replacements of one line by one line cannot make a file longer"
        );
        assert!(!*full);
    }

    /// A list of changed places long enough to threaten the one atomic write a
    /// row is has to stop being a list, because a row that cannot be written
    /// leaves the journal nothing at all — and because "the file changed in
    /// thirty-three places" is "the file changed".
    ///
    /// The cap is pinned here against the row it exists to protect: raising it
    /// past what one append can carry turns a recorded change into a silently
    /// dropped one, and this is what says so.
    #[test]
    fn a_change_in_more_places_than_a_row_can_name_names_none_of_them() {
        let tmp = checkout();
        let file = tmp.path().join("wide.rs");
        let body = "x\n".repeat(200);
        std::fs::write(&file, &body).expect("a file of two hundred identical lines");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-17",
                edit_of(&file, "y\n", "x\n", true),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-17");
        let [Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("one change, got {lines:?}");
        };
        assert!(sites.is_empty(), "two hundred places are no place to name");
        assert!(!*full);

        // And a row that does name the most places it may is still one write.
        let widest = Line::Edit {
            turn: u32::MAX,
            path: "a".repeat(120),
            hash_old: Some(rift_core::Sha256Id::from_bytes([0xab; 32])),
            hash_new: rift_core::Sha256Id::from_bytes([0xcd; 32]),
            sites: (0..crate::window_journal::MAX_EDIT_SITES)
                .filter_map(|n| {
                    let lo = u32::try_from(n).ok()? * 1_000 + 900_000;
                    Interval::new(lo, lo + 40)
                })
                .collect(),
            shift: i64::MIN,
            full: true,
            at_ms: u64::MAX,
        };
        assert!(
            widest.to_json_line().len() < crate::window_journal::MAX_APPEND_BYTES,
            "the fullest row this writes, and its newline, still fit one write"
        );
    }

    /// A replacement written with one line ending, in a file that keeps the
    /// other, is the same replacement.
    #[test]
    fn a_replacement_is_found_whichever_line_ending_the_file_keeps() {
        let tmp = checkout();
        let file = tmp.path().join("crlf.rs");
        std::fs::write(&file, "a\r\nnew\r\nb\r\n").expect("a file with the other ending");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-14",
                edit_of(&file, "old\n", "new\n", false),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-14");
        let [Line::Edit { sites, .. }] = lines.as_slice() else {
            panic!("one change, got {lines:?}");
        };
        assert_eq!(sites.as_slice(), [Interval::new(2, 2).expect("a run")]);
    }

    /// A deletion has nothing to look for, so it names no place — and a line
    /// naming no place is one nothing is ever declined on.
    #[test]
    fn a_deletion_names_no_place_and_still_says_the_file_shrank() {
        let tmp = checkout();
        read_in_full(tmp.path(), "s-15", "cut.rs", "a\ngone\nb\n");
        let file = tmp.path().join("cut.rs");
        std::fs::write(&file, "a\nb\n").expect("the deletion lands");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-15",
                edit_of(&file, "gone\n", "", false),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-15");
        let [_, Line::Edit { sites, shift, .. }] = lines.as_slice() else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert!(sites.is_empty());
        assert_eq!(*shift, -1);
    }

    /// A window that had not read the whole file before the edit does not hold
    /// the whole file after it: the lines it never saw are still lines it never
    /// saw. The place it changed is recorded all the same.
    #[test]
    fn an_edit_by_a_window_that_held_only_part_of_the_file_grants_no_coverage() {
        let tmp = checkout();
        let file = tmp.path().join("part.rs");
        std::fs::write(&file, "a\nold\nc\n").expect("a file");
        std::fs::write(&file, "a\nnew\nc\n").expect("the edit lands");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-16",
                edit_of(&file, "old\n", "new\n", false),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-16");
        let [Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("one change, got {lines:?}");
        };
        assert_eq!(sites.as_slice(), [Interval::new(2, 2).expect("a run")]);
        assert!(!*full, "nothing here says this window ever read the file");
    }

    /// Several edits in one call: the change is recorded, because the old
    /// bytes must stop counting, and no coverage follows from it.
    #[test]
    fn a_change_whose_places_do_not_follow_from_its_payload_grants_no_coverage() {
        let tmp = checkout();
        let file = tmp.path().join("many.rs");
        std::fs::write(&file, "a\nb\n").expect("a file to change");
        let payload = serde_json::json!({
            "tool_name": "MultiEdit",
            "tool_input": { "file_path": file.display().to_string() },
        });
        record(
            &hook("claude-code", "PostToolUse", "s-3", payload),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-3");
        let [Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("one change, got {lines:?}");
        };
        assert!(sites.is_empty());
        assert!(!*full);
    }

    /// A call that changed nothing leaves nothing: a tool that failed and a
    /// tool that was never run are the same file on disk, and the journal has
    /// nothing to say about either.
    #[test]
    fn a_change_that_did_not_land_records_nothing() {
        let tmp = checkout();
        let file = tmp.path().join("same.rs");
        std::fs::write(&file, "a\nb\n").expect("a file to change");
        let payload = serde_json::json!({
            "tool_name": "MultiEdit",
            "tool_input": { "file_path": file.display().to_string() },
        });
        record(
            &hook("claude-code", "PostToolUse", "s-4", payload.clone()),
            tmp.path(),
        );
        record(
            &hook("claude-code", "PostToolUse", "s-4", payload),
            tmp.path(),
        );
        assert_eq!(
            lines_of(tmp.path(), "s-4").len(),
            1,
            "the second call found the file exactly as the first left it"
        );
    }

    /// The refusals, each of which leaves the journal knowing less than really
    /// happened and never more.
    #[test]
    fn a_change_this_checkout_cannot_stand_behind_records_nothing() {
        let tmp = checkout();
        let outside = checkout();
        let elsewhere = outside.path().join("theirs.rs");
        std::fs::write(&elsewhere, "a\n").expect("someone else's file");
        let cases = [
            ("another checkout's file", write_of(&elsewhere, "a\n")),
            (
                "a path relative to a working directory no payload states",
                serde_json::json!({
                    "tool_name": "Write",
                    "tool_input": { "file_path": "src.rs", "content": "a\n" },
                }),
            ),
            (
                "a call that is not edit-shaped at all",
                serde_json::json!({
                    "tool_name": "Bash",
                    "tool_input": { "command": "ls" },
                }),
            ),
        ];
        for (why, payload) in cases {
            record(
                &hook("claude-code", "PostToolUse", "s-5", payload.clone()),
                tmp.path(),
            );
            record(
                &hook("claude-code", "PreToolUse", "s-5", payload),
                tmp.path(),
            );
            assert!(lines_of(tmp.path(), "s-5").is_empty(), "{why}");
        }
    }

    /// A window that names no session has no journal to write to, and a family
    /// this build does not carry has no fields to read.
    #[test]
    fn a_hook_with_no_window_to_speak_of_records_nothing() {
        let tmp = checkout();
        let file = tmp.path().join("x.rs");
        std::fs::write(&file, "a\n").expect("a file");
        record(
            &hook("claude-code", "PostToolUse", "", write_of(&file, "a\n")),
            tmp.path(),
        );
        record(
            &hook("nobody", "PostToolUse", "s-6", write_of(&file, "a\n")),
            tmp.path(),
        );
        assert!(lines_of(tmp.path(), "s-6").is_empty());
        assert!(lines_of(tmp.path(), "").is_empty());
    }
}
