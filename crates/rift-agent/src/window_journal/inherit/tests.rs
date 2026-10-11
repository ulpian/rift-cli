use serde_json::Value;

use super::super::fixtures::{hash, inherited, one_of_each, read_line, resolved, run};
use super::super::{decide, record, DenyShape, EditEcho, EvictReason, Interval};
use super::*;
use crate::respond::Channel;

/// A channel a decline may travel on, read off the pinned capability table
/// rather than spelled here: the confirmed reason channel of the one family
/// that has one.
fn confirmed() -> Channel {
    crate::respond::CAPABILITIES
        .iter()
        .find_map(|(_, _, channel)| match channel {
            Channel::DenyWithReason {
                confirmed: true, ..
            } => Some(*channel),
            _ => None,
        })
        .expect("a family with a confirmed reason channel")
}

fn limits_with(by_name: Mode) -> Limits {
    Limits {
        min_slice_lines: 1,
        inherit: InheritModes {
            by_name,
            ..InheritModes::default()
        },
        ..Limits::default()
    }
}

fn decided(journal: &Journal, lo: u32, hi: u32, limits: Limits) -> Decision {
    let read = resolved("src/lib.rs", 0x40, lo, hi, 900);
    decide(
        &read,
        journal,
        Mode::Enforce,
        Some(confirmed()),
        EditEcho::Pinned,
        64,
        limits,
    )
}

fn spawn(call: &str, agent_type: &str) -> Line {
    Line::Spawn {
        turn: 1,
        call: call.to_owned(),
        prompt: "p-1".to_owned(),
        agent_type: agent_type.to_owned(),
        spawner: None,
        names: vec!["src/lib.rs".to_owned()],
        at_ms: 1,
    }
}

fn claim(agent: &str, call: Option<&str>) -> Line {
    Line::Claim {
        turn: 1,
        agent: agent.to_owned(),
        call: call.map(str::to_owned),
        at_ms: 2,
    }
}

fn spawn_call(line: Option<&Line>) -> Option<&str> {
    match line? {
        Line::Spawn { call, .. } => Some(call.as_str()),
        _ => None,
    }
}

#[test]
fn every_class_has_one_spelling_and_one_audit_tag() {
    for class in InheritClass::ALL {
        assert_eq!(InheritClass::from_str(class.as_str()), Some(class));
        assert_eq!(class.audit_tag(), format!("inherited:{}", class.as_str()));
    }
    assert_eq!(InheritClass::from_str("by-name"), None);
}

#[test]
fn a_journal_written_before_inheritance_reads_back_unchanged() {
    let before = [
        read_line(1, "src/lib.rs", 0x40, 1, 40).to_json_line(),
        r#"{"v":1,"k":"observe","t":2,"p":"src/lib.rs","h":"4040404040404040404040404040404040404040404040404040404040404040","lo":1,"hi":40,"at":3}"#.to_owned(),
    ]
    .join("\n");
    let journal = Journal::parse(&before);
    assert_eq!(journal.lines().len(), 2);
    assert!(matches!(
        journal.lines().get(1),
        Some(Line::Observe { via: None, .. })
    ));
}

#[test]
fn a_journal_with_inherited_rows_reads_back_every_one_of_them() {
    let text: Vec<String> = one_of_each().iter().map(Line::to_json_line).collect();
    let journal = Journal::parse(&text.join("\n"));
    assert_eq!(journal.lines(), one_of_each().as_slice());
}

#[test]
fn a_class_or_an_identifier_this_version_cannot_read_drops_its_row() {
    let good: Value = serde_json::from_str(
        &inherited(InheritClass::ByName, "src/lib.rs", 0x40, 1, 9).to_json_line(),
    )
    .unwrap();
    let mut unknown = good.clone();
    unknown["c"] = Value::from("telepathy");
    assert_eq!(Line::from_json_line(&unknown.to_string()), None);
    let mut observed: Value =
        serde_json::from_str(&read_line(1, "a", 1, 1, 2).to_json_line()).unwrap();
    observed["k"] = Value::from("observe");
    observed["c"] = Value::from(7);
    assert_eq!(Line::from_json_line(&observed.to_string()), None);
    for bad in ["", "has space", "ctrl\u{7}", &"x".repeat(129)] {
        let mut row: Value = serde_json::from_str(&claim("a1", Some("c1")).to_json_line()).unwrap();
        row["agent"] = Value::from(bad);
        assert_eq!(Line::from_json_line(&row.to_string()), None, "{bad:?}");
    }
    let mut names: Value = serde_json::from_str(&spawn("c1", "Explore").to_json_line()).unwrap();
    names["names"] = serde_json::json!(["ok", "bad\nname"]);
    assert_eq!(Line::from_json_line(&names.to_string()), None);
}

#[test]
fn inherited_lines_are_never_coverage_of_their_own() {
    let journal = Journal::from_lines(vec![inherited(
        InheritClass::ByName,
        "src/lib.rs",
        0x40,
        1,
        900,
    )]);
    assert!(journal.coverage("src/lib.rs", hash(0x40)).is_empty());
    assert!(journal.read_coverage("src/lib.rs", hash(0x40)).is_empty());
    let inherited = journal.inherited_coverage("src/lib.rs", hash(0x40), |_| true);
    assert!(inherited.contains(run(1, 900)));
    assert!(journal
        .inherited_coverage("src/lib.rs", hash(0x40), |c| c == InheritClass::Paste)
        .is_empty());
}

#[test]
fn an_observed_class_records_the_read_and_an_observation_and_never_declines() {
    let journal = Journal::from_lines(vec![inherited(
        InheritClass::ByName,
        "src/lib.rs",
        0x40,
        1,
        900,
    )]);
    let decision = decided(&journal, 10, 60, limits_with(Mode::Observe));
    assert_eq!(
        decision,
        Decision::Inherited {
            shape: DenyShape::Plain,
            class: InheritClass::ByName
        }
    );
    let read = resolved("src/lib.rs", 0x40, 10, 60, 900);
    assert_eq!(
        record(&read, &decision, 64, Limits::default(), 2, 5).kind(),
        "read"
    );
    assert!(matches!(
        record_inheritance(&read, &decision, 2, 5),
        Some(Line::Observe {
            via: Some(InheritClass::ByName),
            range,
            ..
        }) if range == run(10, 60)
    ));
    assert_eq!(record_inheritance(&read, &Decision::Allow, 2, 5), None);
}

#[test]
fn an_enforced_class_declines_once_and_an_off_class_decides_nothing() {
    let mut journal = Journal::from_lines(vec![inherited(
        InheritClass::ByName,
        "src/lib.rs",
        0x40,
        1,
        900,
    )]);
    let enforced = decided(&journal, 10, 60, limits_with(Mode::Enforce));
    assert_eq!(enforced, Decision::Deny(DenyShape::Plain));
    let read = resolved("src/lib.rs", 0x40, 10, 60, 900);
    journal.push(record(&read, &enforced, 64, Limits::default(), 2, 5));
    assert_eq!(
        decided(&journal, 10, 60, limits_with(Mode::Enforce)),
        Decision::Allow,
        "one decline per key, inherited or not"
    );
    let fresh = Journal::from_lines(vec![inherited(
        InheritClass::ByName,
        "src/lib.rs",
        0x40,
        1,
        900,
    )]);
    assert_eq!(
        decided(&fresh, 10, 60, limits_with(Mode::Off)),
        Decision::Allow
    );
}

#[test]
fn an_emptied_window_inherits_nothing_it_held_before() {
    let journal = Journal::from_lines(vec![
        inherited(InheritClass::ByName, "src/lib.rs", 0x40, 1, 900),
        Line::Evict {
            turn: 3,
            reason: EvictReason::Compact,
            at_ms: 3,
        },
    ]);
    assert_eq!(
        decided(&journal, 10, 60, limits_with(Mode::Enforce)),
        Decision::Allow
    );
    assert_eq!(
        decided(&journal, 10, 60, limits_with(Mode::Observe)),
        Decision::Allow
    );
}

#[test]
fn a_spawn_is_claimed_only_when_it_is_the_sole_unclaimed_match() {
    let alone = Journal::from_lines(vec![spawn("c1", "Explore"), spawn("c2", "Plan")]);
    assert_eq!(spawn_call(alone.sole_spawn("p-1", "Explore")), Some("c1"));
    assert_eq!(alone.sole_spawn("p-2", "Explore"), None, "another prompt");
    let twins = Journal::from_lines(vec![spawn("c1", "Explore"), spawn("c2", "Explore")]);
    assert_eq!(twins.sole_spawn("p-1", "Explore"), None, "never a guess");
    let one_taken = Journal::from_lines(vec![
        spawn("c1", "Explore"),
        claim("a1", Some("c1")),
        spawn("c2", "Explore"),
    ]);
    assert_eq!(
        spawn_call(one_taken.sole_spawn("p-1", "Explore")),
        Some("c2")
    );
    assert_eq!(one_taken.claim_of("a1"), Some(Some("c1")));
    let unnamed = Journal::from_lines(vec![claim("a9", None)]);
    assert_eq!(unnamed.claim_of("a9"), Some(None));
    assert_eq!(unnamed.claim_of("a1"), None);
}

/// One generated step of a window's history.
fn step_line(kind: u8, class: usize, seed: u8, lo: u32, span: u32, turn: u32) -> Line {
    let class = InheritClass::ALL
        .get(class)
        .copied()
        .unwrap_or(InheritClass::ByName);
    let range = Interval::new(lo, lo + span).unwrap();
    let path = "src/lib.rs".to_owned();
    match kind {
        0 => read_line(turn, "src/lib.rs", seed, lo, lo + span),
        1 => Line::Inherit {
            turn,
            class,
            path,
            hash: hash(seed),
            range,
            total_lines: 900,
            at_ms: u64::from(turn),
        },
        2 => Line::Deny {
            turn,
            path,
            hash: hash(seed),
            range,
            at_ms: u64::from(turn),
        },
        _ => Line::Evict {
            turn,
            reason: EvictReason::Clear,
            at_ms: u64::from(turn),
        },
    }
}

fn journal_of(steps: &[(u8, usize, u8, u32, u32)]) -> Journal {
    let lines = steps
        .iter()
        .zip(1u32..)
        .map(|(&(kind, class, seed, lo, span), turn)| step_line(kind, class, seed, lo, span, turn))
        .collect();
    Journal::from_lines(lines)
}

fn without_inheritance(journal: &Journal) -> Journal {
    Journal::from_lines(
        journal
            .lines()
            .iter()
            .filter(|line| !matches!(line, Line::Inherit { .. }))
            .cloned()
            .collect(),
    )
}

fn arbitrary_steps() -> impl proptest::strategy::Strategy<Value = Vec<(u8, usize, u8, u32, u32)>> {
    proptest::collection::vec(
        (0u8..=3, 0usize..=2, 0x3fu8..=0x40, 1u32..=400, 0u32..=200),
        0..24,
    )
}

fn arbitrary_mode() -> impl proptest::strategy::Strategy<Value = Mode> {
    proptest::prop_oneof![
        proptest::strategy::Just(Mode::Off),
        proptest::strategy::Just(Mode::Observe),
        proptest::strategy::Just(Mode::Enforce),
    ]
}

proptest::proptest! {
    /// ★ With every class at or below observe, no decision ever rests on an
    /// inherited line: whatever the journal, the decision is the one its own
    /// lines reach, or — where those allow the read — the observation of the
    /// class that would have declined it. Never a decline.
    #[test]
    fn no_inherited_line_below_enforce_ever_declines(
        steps in arbitrary_steps(),
        fork in proptest::prop_oneof![proptest::strategy::Just(Mode::Off), proptest::strategy::Just(Mode::Observe)],
        paste in proptest::prop_oneof![proptest::strategy::Just(Mode::Off), proptest::strategy::Just(Mode::Observe)],
        by_name in proptest::prop_oneof![proptest::strategy::Just(Mode::Off), proptest::strategy::Just(Mode::Observe)],
        mode in arbitrary_mode(),
        (seed, lo, span) in (0x3fu8..=0x40, 1u32..=400, 0u32..=200),
    ) {
        let journal = journal_of(&steps);
        let limits = Limits {
            min_slice_lines: 1,
            narrow_mode: Mode::Enforce,
            inherit: InheritModes { fork, paste, by_name },
            ..Limits::default()
        };
        let read = resolved("src/lib.rs", seed, lo, lo + span, 900);
        let decide_over = |journal: &Journal| {
            decide(&read, journal, mode, Some(confirmed()), EditEcho::Pinned, 64, limits)
        };
        let with = decide_over(&journal);
        let own = decide_over(&without_inheritance(&journal));
        if let Decision::Inherited { class, .. } = &with {
            proptest::prop_assert_eq!(&own, &Decision::Allow);
            proptest::prop_assert_eq!(limits.inherit.of(*class), Mode::Observe);
        } else {
            proptest::prop_assert_eq!(&with, &own);
        }
    }

    /// ★ With a class at enforce, a decline still rests only on lines in the
    /// window's own journal: the run it declines is covered by that journal's
    /// reads and its enforced inherited lines, nothing else.
    #[test]
    fn an_enforced_decline_rests_only_on_the_windows_own_lines(
        steps in arbitrary_steps(),
        (seed, lo, span) in (0x3fu8..=0x40, 1u32..=400, 0u32..=200),
    ) {
        let journal = journal_of(&steps);
        let limits = Limits {
            min_slice_lines: 1,
            narrow_mode: Mode::Enforce,
            inherit: InheritModes { by_name: Mode::Enforce, ..InheritModes::default() },
            ..Limits::default()
        };
        let read = resolved("src/lib.rs", seed, lo, lo + span, 900);
        let decision = decide(&read, &journal, Mode::Enforce, Some(confirmed()), EditEcho::Pinned, 64, limits);
        let mut held = journal.read_coverage("src/lib.rs", hash(seed));
        for run in journal
            .inherited_coverage("src/lib.rs", hash(seed), |c| c == InheritClass::ByName)
            .runs()
        {
            held.insert(*run);
        }
        match decision {
            Decision::Deny(DenyShape::Narrow { held: run, .. }) => {
                proptest::prop_assert!(held.contains(run));
            }
            Decision::Deny(_) => proptest::prop_assert!(held.contains(read.range)),
            _ => {}
        }
    }
}
