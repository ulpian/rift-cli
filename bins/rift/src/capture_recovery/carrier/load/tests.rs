use super::*;
use crate::capture_recovery::carrier::{QueuedCondense, Reason};
use crate::capture_recovery::eligibility::fixtures::{Rig, SeededEvent};
use crate::capture_recovery::inventory::tests::queue_dir;
use rift_shadow::hooks::session_cache::SessionCache;

const NATIVE: &str = "carrier-native";
const AGENT: &str = "claude-code";

struct Case {
    rig: Rig,
    queue: Namespace,
    sessions: Namespace,
    ticket: CondenseTicket,
}

fn hex(c: char) -> String {
    std::iter::repeat_n(c, 64).collect()
}

impl Case {
    /// A queued condense whose cache is observed ending, with one identity and
    /// one stored `session_end`.
    fn genuine() -> Self {
        let rig = Rig::new();
        let queue_dir = queue_dir(&rig.fixture);
        let bytes = b"{\"ticket\":1}";
        std::fs::write(queue_dir.join("condense.json"), bytes).unwrap();
        let ticket = CondenseTicket {
            name: "condense.json".into(),
            context_id: hex('c'),
            job: QueuedCondense {
                agent: AGENT.into(),
                session_id: NATIVE.into(),
                repo_id: rig.repo_id.as_str().into(),
                context_sha256: hex('a'),
                ticket_sha256: format!("{:x}", Sha256::digest(bytes)),
            },
        };
        rig.write_cache(&SessionCache::new(NATIVE, AGENT, "ending", 5000));
        rig.seed_turn(AGENT, NATIVE, 1, None);
        let case = Self {
            queue: Namespace::open(&queue_dir).unwrap(),
            sessions: Namespace::open(&rig.sessions_dir).unwrap(),
            rig,
            ticket,
        };
        case.end_event(1);
        case
    }

    fn end_event(&self, event_ordinal: u32) {
        self.rig.seed_event(&SeededEvent {
            firing: AGENT,
            native: NATIVE,
            verb: "SessionEnd",
            ordinal: 1,
            event_ordinal,
            kind: "session_end",
            trust: "git_fact",
            sha: &hex('e'),
        });
    }

    fn sources(&self) -> Sources<'_> {
        Sources {
            queue: &self.queue,
            sessions: &self.sessions,
        }
    }

    fn admit(&self) -> Result<Admission, LoadOwed> {
        let repo = self.rig.repo_id.as_str();
        let tickets = std::slice::from_ref(&self.ticket);
        self.rig.rt.block_on(admit_carrier(
            &self.rig.store,
            &self.sources(),
            repo,
            tickets,
        ))
    }

    fn recheck(&self, admission: &Admission) -> Result<(), Changed> {
        let repo = self.rig.repo_id.as_str();
        self.rig
            .rt
            .block_on(admission.recheck(&self.rig.store, &self.sources(), repo))
    }
}

#[test]
fn a_genuine_condense_is_loaded_admitted_and_survives_its_recheck() {
    let case = Case::genuine();
    let admission = case.admit().unwrap();
    assert_eq!(admission.ticket, case.ticket);
    assert_eq!(admission.admitted.end_payload_sha256, hex('e'));
    assert_eq!(case.recheck(&admission), Ok(()));
}

#[test]
fn a_second_end_event_after_admission_refuses_the_recheck() {
    let case = Case::genuine();
    let admission = case.admit().unwrap();
    case.end_event(2);
    assert_eq!(case.recheck(&admission), Err(Changed::Facts));
}

#[test]
fn a_changed_ticket_after_admission_refuses_the_recheck() {
    let case = Case::genuine();
    let admission = case.admit().unwrap();
    let path = queue_dir(&case.rig.fixture).join("condense.json");
    std::fs::write(path, b"{\"ticket\":2}").unwrap();
    assert_eq!(case.recheck(&admission), Err(Changed::Ticket));
}

#[test]
fn a_completion_marker_in_the_queue_is_never_grafted_onto() {
    let case = Case::genuine();
    let queue_dir = queue_dir(&case.rig.fixture);
    std::fs::write(
        queue_dir.join(format!("completed-{}.done", hex('c'))),
        b"{}",
    )
    .unwrap();
    let owed = [(Reason::Completed, 1)].into_iter().collect();
    assert_eq!(
        case.admit(),
        Err(LoadOwed::Carrier(CarrierOwed::NoneAdmissible(owed)))
    );
}

#[test]
fn an_absent_cache_is_an_absence_but_an_unreadable_one_refuses_the_load() {
    let case = Case::genuine();
    let cache = case.rig.sessions_dir.join(format!("{NATIVE}.json"));
    std::fs::remove_file(&cache).unwrap();
    let owed = [(Reason::CacheAbsent, 1)].into_iter().collect();
    assert_eq!(
        case.admit(),
        Err(LoadOwed::Carrier(CarrierOwed::NoneAdmissible(owed)))
    );
    std::fs::create_dir(&cache).unwrap();
    assert_eq!(
        case.admit(),
        Err(LoadOwed::Evidence(ReadReason::CacheNotRegular))
    );
}

#[test]
fn more_condenses_than_one_pass_reads_refuse_before_any_read() {
    let case = Case::genuine();
    let tickets = vec![case.ticket.clone(); MAX_CURRENT_CANDIDATES + 1];
    let repo = case.rig.repo_id.as_str();
    let owed = case.rig.rt.block_on(admit_carrier(
        &case.rig.store,
        &case.sources(),
        repo,
        &tickets,
    ));
    assert_eq!(owed, Err(LoadOwed::Overflow));
    assert_eq!(case.rig.store.calls(), 0);
}
