//! The tests of the shell recogniser, one file per subject.
//!
//! The production half stays one file (`../mod.rs`) because the intercept
//! gate walks it function by function as part of the decision path; the tests
//! it once carried in two trailing modules are split here by what they are
//! about: the published vocabulary and the range each form supports, the
//! three compositions and what each one reads, the labels a chain writes
//! between its reads, the `sed` print scripts beyond one range, the line
//! pick written in `awk`, the shapes left alone, the properties a generated
//! line has to hold, and the witness the fuzz target checks every recognised
//! target against. The helpers every file spells its expectations with live
//! here.

mod awk_line_pick;
mod compositions;
mod labels;
mod properties;
mod quoting;
mod refusals;
mod sed_scripts;
// The fuzz target includes this same file by path, so the witness the
// fuzzer asserts with is the one these tests pin.
mod shell_read_oracle;
mod vocabulary;
mod witness;

use crate::shell_read::{recognise, Narrowing, ShellCall, ShellListing, ShellRead, ShellSpan};
use crate::{ReadChannel, ReadRequest, ReadSpan};

/// The read a command line supports, as `(path, span)`.
fn read_of(command: &str) -> Option<(String, ReadSpan)> {
    let ReadRequest {
        path,
        span,
        channel,
    } = recognise(command)?.read_request()?;
    assert_eq!(channel, ReadChannel::Shell, "a shell read is a shell read");
    Some((path, span))
}

/// Every read a command line performs, as `(path, span)` apiece.
fn reads_of(command: &str) -> Vec<(String, ReadSpan)> {
    recognise(command)
        .map(|call| call.read_requests())
        .unwrap_or_default()
        .into_iter()
        .map(|request| (request.path, request.span))
        .collect()
}

/// One member of a recognised read, spelled as compactly as the table
/// needs it.
fn member(path: &str, span: ShellSpan) -> ShellRead {
    ShellRead {
        path: path.to_owned(),
        span,
        narrowing: None,
    }
}

/// The same, with the trailing pipe that narrowed it.
fn narrowed(path: &str, span: ShellSpan, narrowing: Narrowing) -> ShellRead {
    ShellRead {
        narrowing: Some(narrowing),
        ..member(path, span)
    }
}

/// A recognised read of one file.
fn read(path: &str, span: ShellSpan) -> ShellCall {
    ShellCall::Read(vec![member(path, span)])
}

/// A recognised listing, likewise.
fn listing(dir: &str, pattern: Option<&str>) -> ShellCall {
    ShellCall::Listing(ShellListing {
        dir: dir.to_owned(),
        pattern: pattern.map(str::to_owned),
    })
}

/// Every command line in a table is recognised as exactly the call beside it.
fn assert_each_recognised(cases: &[(&str, ShellCall)]) {
    for (command, expected) in cases {
        assert_eq!(recognise(command).as_ref(), Some(expected), "{command}");
    }
}
