#![no_main]
//! Fuzz target for the strict shell read-and-listing recogniser.
//!
//! Contract: for *any* command line, `recognise` returns without panicking, in
//! time linear in the input, and allocates nothing beyond the words it keeps.
//! The input is untrusted twice over — it is a string an agent composed, and
//! it arrives through a hook payload written by another process — and the
//! recogniser runs before anything else on that path, so a panic here is a
//! hook that dies on a command line rather than leaving it alone.
//!
//! The corpus is capped at the recogniser's own limit plus a margin, so the
//! fuzzer spends its budget on the classification rather than on lines the
//! length check refuses in one comparison.
//!
//! What is asserted beyond "no panic": a recognised call never invents a
//! target. Every path or directory it reports is either a word that was on the
//! line, one word read from under the directory another word named (which is
//! the only thing a leading `cd` may do to a path), or the one directory the
//! grammar implies — the property a mangled input could otherwise break
//! silently.
//!
//! The composition rules widened what reaches the classification rather than
//! what it may conclude, so the same assertions are made over **every** read
//! the line performs instead of over one. A chain is also where a quadratic
//! walk would first show, which is why the member cap is asserted here too:
//! the corpus is allowed to be a line of four thousand separators, and the
//! answer to one must still be bounded.
//!
//! **Labels are the case this target now earns its keep on.** A member that
//! reads nothing neither adds a target nor stops the walk, so a line of
//! labels is the cheapest way to make the recogniser do work for an answer of
//! `None` — and the `cd` peel steps over them, which is a second loop a hostile
//! line gets to drive. Both are covered by the two standing assertions: a
//! recognised read always names at least one file (a line of labels alone must
//! come back `None`, never an empty read), and it never names more than the
//! cap.

use libfuzzer_sys::fuzz_target;
use rift_agent::shell_read::{recognise, ShellCall, MAX_CHAIN_READS, MAX_COMMAND_BYTES};

/// Whether a reported target is made of names the line actually wrote: a word
/// verbatim, or one word under a directory another word named.
fn names_a_word(target: &str, command: &str) -> bool {
    let under = |name: &str| {
        target
            .strip_suffix(name)
            .is_some_and(|prefix| prefix.ends_with('/'))
    };
    // The grammar's own splitter: a word ends at a space or a tab, as a
    // shell's does, so a Unicode separator inside a word is part of it.
    target == "."
        || under(".")
        || command
            .split([' ', '\t'])
            .any(|word| word == target || under(word))
}

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(MAX_COMMAND_BYTES + 64)];
    let Ok(command) = std::str::from_utf8(data) else {
        return;
    };
    let Some(call) = recognise(command) else {
        return;
    };
    let targets: Vec<String> = match &call {
        ShellCall::Read(reads) => reads.iter().map(|read| read.path.clone()).collect(),
        ShellCall::Listing(listing) => vec![listing.dir.clone()],
    };
    assert!(
        !targets.is_empty(),
        "a recognised call names no file at all — a line whose members all read \
         nothing must not come back as a read"
    );
    assert!(
        targets.len() <= MAX_CHAIN_READS.max(1),
        "a recognised line reports more targets than the cap allows"
    );
    for target in &targets {
        assert!(
            names_a_word(target, command),
            "recognised a target no word on the line names"
        );
    }
    // Every read the line performs, not only the one a caller with room for a
    // single fact would take: a range that starts at line zero or ends before
    // it starts is a range no later step would catch.
    for request in call.read_requests() {
        let Some((lo, hi)) = request.conservative_lines() else {
            continue;
        };
        assert!(lo >= 1, "a line number starts at one");
        assert!(hi >= lo, "a range never ends before it starts");
    }
});
