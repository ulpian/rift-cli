//! The coordinator section: when it is said, what it names, what it costs,
//! and what it never writes.

use rift_agent::window_journal::{inherited_from_read, InheritClass, Interval, Line};
use rift_recall_policy::Mode;
use rift_shadow::hooks::RawHook;

use super::*;
use crate::window_journal::WindowJournal;

const CC: AgentId = AgentId::CLAUDE_CODE;
const SESSION: &str = "s-coord";
const CHILD: &str = "agent-7";

fn hash(seed: u8) -> rift_core::Sha256Id {
    rift_hashchain::leaf_hash(&[seed])
}

fn short(seed: u8) -> String {
    hash(seed).to_hex()[..16].to_owned()
}

fn read(path: &str, revision: u8, at_ms: u64) -> Line {
    Line::Read {
        turn: 1,
        path: path.to_owned(),
        hash: hash(revision),
        range: Interval::new(1, 40).unwrap(),
        total_lines: 40,
        at_ms,
    }
}

fn coordinator_read(repo: &Path, line: &Line) {
    WindowJournal::open(repo, SESSION, None)
        .append(line)
        .unwrap();
}

fn opening(repo: &Path) -> Opening<'_> {
    Opening {
        repo,
        agent: &CC,
        verb: "SubagentStart",
        session: SESSION,
        window: CHILD,
    }
}

fn held(paths: &[&str]) -> Vec<CoordinatorRead> {
    paths
        .iter()
        .zip(0_u8..)
        .map(|(path, seed)| CoordinatorRead {
            path: (*path).to_owned(),
            hash: hash(seed),
            at_ms: u64::from(seed),
        })
        .collect()
}

#[test]
fn the_section_names_what_the_coordinator_read_last_first() {
    let repo = tempfile::tempdir().unwrap();
    coordinator_read(repo.path(), &read("src/older.rs", 1, 10));
    coordinator_read(repo.path(), &read("src/newer.rs", 2, 20));
    let text = coordinator_section(&opening(repo.path()), "").expect("two reads to name");
    let (newer, older) = (hash(2).to_hex(), hash(1).to_hex());
    let in_order = [
        rift_recall_serve::CoordinatorRead {
            path: "src/newer.rs",
            hash: &newer,
        },
        rift_recall_serve::CoordinatorRead {
            path: "src/older.rs",
            hash: &older,
        },
    ];
    let expected = rift_recall_serve::render_coordinator_reads_naming(&in_order, 0).unwrap();
    assert_eq!(text, expected.0);
    assert!(
        text.contains(&format!("src/newer.rs@{}", short(2))),
        "{text}"
    );
}

#[test]
fn a_coordinator_that_has_read_nothing_is_silence() {
    let repo = tempfile::tempdir().unwrap();
    assert_eq!(coordinator_section(&opening(repo.path()), ""), None);
}

#[test]
fn what_the_subagent_already_holds_is_not_named() {
    let repo = tempfile::tempdir().unwrap();
    coordinator_read(repo.path(), &read("src/a.rs", 1, 10));
    WindowJournal::open(repo.path(), SESSION, Some(CHILD))
        .append(&read("src/a.rs", 1, 5))
        .unwrap();
    assert_eq!(coordinator_section(&opening(repo.path()), ""), None);
}

/// Injection switched off is switched off for the section too.
#[test]
fn a_zero_turn_budget_is_silence() {
    let repo = tempfile::tempdir().unwrap();
    let policies = repo.path().join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(policies.join("serve.toml"), "injection_turn_budget = 0\n").unwrap();
    coordinator_read(repo.path(), &read("src/a.rs", 1, 10));
    assert_eq!(coordinator_section(&opening(repo.path()), ""), None);
}

/// The section fits the room the turn's budget leaves after the pointer,
/// dropping names into the count; when not even the count fits, nothing is
/// said.
#[test]
fn the_section_fits_the_room_the_pointer_leaves() {
    let six = held(&[
        "src/m0.rs",
        "src/m1.rs",
        "src/m2.rs",
        "src/m3.rs",
        "src/m4.rs",
        "src/m5.rs",
    ]);
    let whole = fitted_section(&six, u32::MAX).unwrap();
    let room = rift_tokens::count(&whole) - 1;
    let fitted = fitted_section(&six, room).unwrap();
    assert!(rift_tokens::count(&fitted) <= room, "{fitted}");
    assert!(fitted.contains("src/m0.rs"), "{fitted}");
    assert!(
        !fitted.contains("src/m5.rs"),
        "the tail is dropped: {fitted}"
    );
    assert_eq!(fitted_section(&six, 3), None);

    let repo = tempfile::tempdir().unwrap();
    coordinator_read(repo.path(), &read("src/a.rs", 1, 10));
    let pointer = "word ".repeat(2_000);
    assert_eq!(coordinator_section(&opening(repo.path()), &pointer), None);
}

/// The longest span the frames guard renders unaltered, in characters: a
/// path one character longer is truncated, and a truncated path is withheld
/// from the section rather than shortened. Spelled as a number and proved
/// against the guard below, so the worst case here is measured, not guessed.
const LONGEST_RENDERED_PATH: usize = 120;

/// The revisions the fit cannot name past the frame's cap, counted behind
/// the names: enough that the count itself costs tokens.
const COUNTED_TAIL: usize = 1_000;

/// The repeating units a worst-case path may be spelled from, every character
/// one the pushed alphabet admits and a checkout spelling keeps. Which unit
/// the tokenizer splits finest is measured, so a tokenizer change moves the
/// worst case rather than silently retiring it.
const PATH_UNITS: [&str; 10] = [
    "a/",
    "0/",
    "a.",
    "a_",
    "a-",
    "a+",
    "a@",
    "a:",
    "a ",
    "1/2.3_4-5+6@7:8 9/",
];

/// A path of exactly [`LONGEST_RENDERED_PATH`] characters cycling `unit`,
/// made distinct by its leading `tag` digit, with no empty part and no
/// trailing blank the guard would trim.
fn longest_admitted_path(unit: &str, tag: usize) -> String {
    let mut path = tag.to_string();
    path.extend(unit.chars().cycle().take(LONGEST_RENDERED_PATH - 1));
    if !path.ends_with(char::is_alphanumeric) {
        path.pop();
        path.push('z');
    }
    path
}

/// The unit whose longest path costs the most tokens as a rendered entry.
fn hardest_unit() -> &'static str {
    PATH_UNITS
        .into_iter()
        .max_by_key(|unit| {
            rift_tokens::count(&format!("{}@{}", longest_admitted_path(unit, 0), short(0)))
        })
        .unwrap_or_default()
}

/// The push is the pointer, a newline and the section, and token counts do
/// not add across a join, while the fit measures the section alone against
/// the room the pointer leaves. This holds the **whole push** to the default
/// turn budget in the worst case the cap admits — eight named revisions whose
/// paths are the longest the guard renders, in the characters the tokenizer
/// splits finest, with a long tail counted behind them — through the real
/// room computation, and shows the worst case is still said rather than
/// collapsing to silence.
#[test]
fn the_whole_push_stays_inside_the_default_budget_in_the_worst_case() {
    let unit = hardest_unit();
    let worst: Vec<String> = (0..8).map(|tag| longest_admitted_path(unit, tag)).collect();
    for path in &worst {
        assert_eq!(path.chars().count(), LONGEST_RENDERED_PATH);
        assert_eq!(
            rift_recall_serve::neutral_pushed_span(path).as_deref(),
            Some(path.as_str()),
            "the guard renders this length unaltered"
        );
        let longer = format!("{path}z");
        assert_ne!(
            rift_recall_serve::neutral_pushed_span(&longer).as_deref(),
            Some(longer.as_str()),
            "one character more and the guard truncates, so this is the longest"
        );
    }
    let repo = tempfile::tempdir().unwrap();
    for i in 0..COUNTED_TAIL {
        coordinator_read(repo.path(), &read(&format!("t/{i}.rs"), 1, 1));
    }
    for (i, path) in worst.iter().enumerate() {
        let seed = u8::try_from(100 + i).unwrap();
        coordinator_read(
            repo.path(),
            &read(path, seed, 1_000 + u64::try_from(i).unwrap()),
        );
    }

    let pointer = crate::agent_help::session_pointer();
    let section =
        coordinator_section(&opening(repo.path()), &pointer).expect("the worst case is still said");
    let push = format!("{pointer}\n{section}");
    let tokens = rift_tokens::count(&push);
    assert!(
        tokens <= rift_recall_policy::DEFAULT_INJECTION_TURN_BUDGET,
        "the whole push costs {tokens} tokens (pointer {}, section {}): {section}",
        rift_tokens::count(&pointer),
        rift_tokens::count(&section)
    );
    assert!(
        section.contains(&format!("{}@{}", worst[7], short(107))),
        "the newest worst-case path is named: {section}"
    );
    assert!(!section.contains("withheld"), "{section}");
    let counted: usize = section
        .rsplit_once('(')
        .and_then(|(_, tail)| tail.split(' ').next())
        .and_then(|n| n.parse().ok())
        .expect("a counted tail");
    assert!(counted >= COUNTED_TAIL, "{section}");
}

/// Three reads by the main window, two of which a spawning prompt names.
fn briefed_coordinator(repo: &Path) {
    coordinator_read(repo, &read("src/a.rs", 1, 10));
    coordinator_read(repo, &read("src/b.rs", 2, 20));
    coordinator_read(repo, &read("src/c.rs", 3, 30));
}

/// The two ends of a spawn of `CHILD`, run through the journal step with the
/// lever at `by_name`: the main window's call naming `src/a.rs` and
/// `src/b.rs`, then the child's opening.
fn spawn_and_open(repo: &Path, by_name: Mode) {
    let hook = |verb: &str, payload: serde_json::Value| RawHook {
        agent: CC,
        verb: verb.to_owned(),
        session: SESSION.to_owned(),
        turn: Some(2),
        occurred_at_ms: 1_700_000_000_000,
        payload,
    };
    let spawning = hook(
        "PreToolUse",
        serde_json::json!({
            "session_id": SESSION,
            "prompt_id": "p-1",
            "tool_name": "Agent",
            "tool_use_id": "toolu_1",
            "tool_input": { "subagent_type": "Explore", "prompt": "Read src/a.rs and src/b.rs." },
        }),
    );
    let opening = hook(
        "SubagentStart",
        serde_json::json!({ "session_id": SESSION, "prompt_id": "p-1", "agent_type": "Explore" }),
    );
    for (raw, window) in [(&spawning, None), (&opening, Some(CHILD))] {
        spawn_step(&SpawnHook {
            raw,
            work_tree: repo,
            window,
            by_name,
            turn: 2,
            at_ms: 50,
        });
    }
}

/// How many lines of `class` the child's journal holds.
fn inherited_lines(repo: &Path, class: InheritClass) -> usize {
    WindowJournal::open(repo, SESSION, Some(CHILD))
        .load()
        .journal
        .lines()
        .iter()
        .filter(|line| matches!(line, Line::Inherit { class: c, .. } if *c == class))
        .count()
}

/// Write the repository's `inherit_by_name` lever.
fn write_lever(repo: &Path, by_name: Mode) {
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        format!(
            "[interception]\ninherit_by_name = \"{}\"\n",
            by_name.as_str()
        ),
    )
    .unwrap();
}

/// A line the subagent inherits never subtracts from the section — only its
/// own reads do — so the section is the same bytes whatever class the
/// inherited lines carry, whatever the `inherit_by_name` lever is written
/// as, and the same as with no inherited line at all. By-name lines come
/// from the real writer at each lever value; fork and paste, which nothing
/// writes yet, are seeded as the writer would seed them.
#[test]
fn the_section_is_the_same_bytes_whatever_the_subagent_inherits() {
    let plain = tempfile::tempdir().unwrap();
    briefed_coordinator(plain.path());
    let baseline = coordinator_section(&opening(plain.path()), "").expect("three reads to name");
    assert!(baseline.contains("src/a.rs@") && baseline.contains("src/b.rs@"));

    for by_name in [Mode::Off, Mode::Observe, Mode::Enforce] {
        for class in InheritClass::ALL {
            let repo = tempfile::tempdir().unwrap();
            write_lever(repo.path(), by_name);
            briefed_coordinator(repo.path());
            spawn_and_open(repo.path(), by_name);
            if class != InheritClass::ByName {
                let own = WindowJournal::open(repo.path(), SESSION, Some(CHILD));
                for line in [read("src/a.rs", 1, 10), read("src/b.rs", 2, 20)] {
                    own.append(&inherited_from_read(&line, class, 2, 50).unwrap())
                        .unwrap();
                }
            }
            let seeded = inherited_lines(repo.path(), class);
            assert_eq!(
                seeded > 0,
                class != InheritClass::ByName || by_name != Mode::Off,
                "{by_name:?} {class:?}: the child holds the lines the case is about"
            );
            assert_eq!(
                coordinator_section(&opening(repo.path()), "").as_deref(),
                Some(baseline.as_str()),
                "{by_name:?} {class:?}"
            );
        }
    }
}

/// End to end through the boundary composer, on a repository set up on this
/// machine: the subagent's window opens with the pointer and the section in
/// one push, booked as one spend row; a retried opening says nothing; and the
/// subagent's own journal — the only thing a decline may rest on — gains no
/// row from it.
#[test]
fn a_subagent_window_opens_once_with_its_coordinators_reads() {
    use crate::hooks::agent::{drop_projection_store, session_pointer_in, set_up_here};

    let repo = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(repo.path()).unwrap();
    std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
    std::fs::write(
        repo.path().join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let db = crate::family::recall_db(repo.path()).unwrap();
    set_up_here(repo.path());
    coordinator_read(repo.path(), &read("src/briefed.rs", 4, 10));
    let body = serde_json::json!({
        "session_id": SESSION,
        "hook_event_name": "SubagentStart",
        "agent_id": CHILD,
    })
    .to_string();
    let journal = crate::window_journal::journal_dir(repo.path()).join(format!(
        "{}.jsonl",
        crate::window_journal::key(SESSION, Some(CHILD))
    ));

    let text = session_pointer_in(repo.path(), &CC, "SubagentStart", body.as_bytes())
        .expect("the window is introduced");
    let pointer = crate::agent_help::session_pointer();
    let section = text
        .strip_prefix(pointer.as_str())
        .and_then(|rest| rest.strip_prefix('\n'))
        .expect("the pointer, then the section on a line of its own");
    assert!(
        section.contains(&format!("src/briefed.rs@{}", short(4))),
        "{section}"
    );
    assert_eq!(
        session_pointer_in(repo.path(), &CC, "SubagentStart", body.as_bytes()),
        None,
        "a retried opening pushes nothing twice"
    );
    assert!(!journal.exists(), "the subagent's journal gains no row");
    assert_eq!(spend_rows(&db), vec![i64::from(rift_tokens::count(&text))]);

    if let Some(dir) = db.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
    drop_projection_store(repo.path());
}

/// The tokens of every injection row in the family's recall store.
fn spend_rows(db: &Path) -> Vec<i64> {
    use rift_store::CanonicalStore as _;

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let store = rift_store::open_migrated(db, &rift_store::recall_migrations()).unwrap();
    let rows = rt
        .block_on(store.query(
            "SELECT tokens_returned FROM memd_audit_event \
             WHERE verb = 'memd.manifest_injected'",
            &[],
        ))
        .unwrap();
    rows.iter().map(|row| row.get_i64(0).unwrap()).collect()
}
