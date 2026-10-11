//! Test fixtures the window-journal modules share: a hash with a visible
//! byte pattern, a run, one line of every kind, and a read resolved against
//! a file of a stated length.
//!
//! A separate module because the codec, the journal's queries, the delivery
//! rules and the decision all build the same lines, and a fixture copied per
//! module drifts.

use rift_core::Sha256Id;

use super::{EvictReason, FamilyRoute, InheritClass, Interval, Line, ResolvedRead};
use crate::{HiKind, ReadChannel, ReadRequest, ReadSpan};

/// A hash with a distinguishable byte pattern, so a mixed-up hash in a
/// failure message is obvious rather than a wall of zeroes.
pub(super) fn hash(seed: u8) -> Sha256Id {
    Sha256Id::from_bytes([seed; 32])
}

pub(super) fn run(lo: u32, hi: u32) -> Interval {
    Interval::new(lo, hi).expect("a test interval is a run")
}

/// An announcement line, spelled once for the tests below.
pub(super) fn held_hold(path: &str, seed: u8, lo: u32, hi: u32) -> Line {
    Line::Hold {
        turn: 1,
        path: path.to_owned(),
        hash: hash(seed),
        range: run(lo, hi),
        at_ms: 10,
    }
}

/// One line of every kind, for the assertions that walk them all.
pub(super) fn one_of_each() -> Vec<Line> {
    let mut lines = vec![
        Line::Read {
            turn: 2,
            path: "src/lib.rs".to_owned(),
            hash: hash(0x3f),
            range: run(1, 500),
            total_lines: 900,
            at_ms: 1_700_000_000_000,
        },
        Line::Deny {
            turn: 3,
            path: "src/lib.rs".to_owned(),
            hash: hash(0x3f),
            range: run(1, 500),
            at_ms: 12,
        },
        Line::Observe {
            turn: 4,
            path: "a b/c.rs".to_owned(),
            hash: hash(0x01),
            range: run(7, 7),
            at_ms: 13,
            via: None,
        },
        Line::Expect {
            turn: 5,
            path: "src/new.rs".to_owned(),
            hash: hash(0xab),
            at_ms: 14,
        },
        Line::Edit {
            turn: 6,
            path: "src/lib.rs".to_owned(),
            hash_old: Some(hash(0x3f)),
            hash_new: hash(0x40),
            sites: vec![run(37, 44), run(80, 81)],
            shift: -8,
            full: true,
            at_ms: 15,
        },
        Line::Edit {
            turn: 7,
            path: "src/new.rs".to_owned(),
            hash_old: None,
            hash_new: hash(0xab),
            sites: vec![],
            shift: 0,
            full: false,
            at_ms: 16,
        },
        Line::Evict {
            turn: 8,
            reason: EvictReason::PreCompact,
            at_ms: 17,
        },
    ];
    lines.extend(inheritance_rows());
    lines
}

/// One of each line the inheritance between windows writes.
fn inheritance_rows() -> Vec<Line> {
    vec![
        Line::Observe {
            turn: 9,
            path: "src/lib.rs".to_owned(),
            hash: hash(0x40),
            range: run(1, 30),
            at_ms: 18,
            via: Some(InheritClass::ByName),
        },
        inherited(InheritClass::ByName, "src/lib.rs", 0x40, 1, 900),
        Line::Spawn {
            turn: 10,
            call: "toolu_01AbC".to_owned(),
            prompt: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            agent_type: "my-plugin:reviewer".to_owned(),
            spawner: Some("a0e79504".to_owned()),
            names: vec!["src/lib.rs".to_owned(), "a b/c.rs".to_owned()],
            at_ms: 20,
        },
        Line::Spawn {
            turn: 10,
            call: "toolu_01XyZ".to_owned(),
            prompt: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            agent_type: "Explore".to_owned(),
            spawner: None,
            names: vec![],
            at_ms: 21,
        },
        Line::Claim {
            turn: 10,
            agent: "a0e79504".to_owned(),
            call: Some("toolu_01AbC".to_owned()),
            at_ms: 22,
        },
        Line::Claim {
            turn: 10,
            agent: "a2f52a50".to_owned(),
            call: None,
            at_ms: 23,
        },
        Line::Family {
            turn: 11,
            path: "src/lib.rs".to_owned(),
            hash: hash(0x40),
            range: run(1, 900),
            route: FamilyRoute::Outline,
            offered: vec![run(1, 40), run(120, 300)],
            acted: false,
            frame_bytes: 812,
            at_ms: 24,
        },
    ]
}

/// An inherited line of `class` over a nine-hundred-line file.
pub(super) fn inherited(class: InheritClass, path: &str, seed: u8, lo: u32, hi: u32) -> Line {
    Line::Inherit {
        turn: 1,
        class,
        path: path.to_owned(),
        hash: hash(seed),
        range: run(lo, hi),
        total_lines: 900,
        at_ms: 19,
    }
}

/// A read line over a nine-hundred-line file, for the fixtures below.
pub(super) fn read_line(turn: u32, path: &str, seed: u8, lo: u32, hi: u32) -> Line {
    Line::Read {
        turn,
        path: path.to_owned(),
        hash: hash(seed),
        range: run(lo, hi),
        total_lines: 900,
        at_ms: u64::from(turn),
    }
}

pub(super) fn request(path: &str, lo: u32, hi_kind: HiKind) -> ReadRequest {
    ReadRequest {
        path: path.to_owned(),
        span: ReadSpan::lines(lo, hi_kind),
        channel: ReadChannel::Tool,
    }
}

pub(super) fn resolved(path: &str, seed: u8, lo: u32, hi: u32, total_lines: u32) -> ResolvedRead {
    ResolvedRead::new(
        &request(path, lo, HiKind::Explicit(hi)),
        hash(seed),
        total_lines,
    )
    .expect("a test read is a range of its file")
}

/// A limit-less read of a file `total` lines long — the shape almost every
/// real read has, and the one the delivery rules are written about.
pub(super) fn whole_file(total: u32) -> ResolvedRead {
    ResolvedRead::new(
        &request("src/lib.rs", 1, HiKind::DefaultWindow(HiKind::END_OF_FILE)),
        hash(3),
        total,
    )
    .expect("a limit-less read of a file with lines in it is a range of it")
}
