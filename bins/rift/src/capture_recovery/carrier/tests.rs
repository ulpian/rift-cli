use super::*;
use crate::capture_recovery::eligibility::CacheProjection;

const REPO: &str = "family-1";

fn hex(c: char) -> String {
    c.to_string().repeat(64)
}

fn condense(session: &str, context: char) -> QueuedCondense {
    QueuedCondense {
        agent: "claude-code".to_owned(),
        session_id: session.to_owned(),
        repo_id: REPO.to_owned(),
        context_sha256: hex(context),
        ticket_sha256: hex('b'),
    }
}

fn cache(session: &str, state: &str) -> CacheProjection {
    CacheProjection {
        cache_sha256: hex('c'),
        schema: 1,
        session_id: session.to_owned(),
        agent: "claude-code".to_owned(),
        state: state.to_owned(),
        ordinal: 3,
        turn_open: false,
        work_tree: None,
    }
}

fn end(key: char) -> EndEvent {
    EndEvent {
        idempotency_key: hex(key),
        payload_sha256: hex('e'),
    }
}

struct Case {
    condense: QueuedCondense,
    cache: Option<CacheProjection>,
    completion: Completion,
    agents: Vec<String>,
    ends: Vec<EndEvent>,
}

impl Case {
    fn genuine(session: &str, context: char) -> Self {
        Self {
            condense: condense(session, context),
            cache: Some(cache(session, "ending")),
            completion: Completion::None,
            agents: vec!["claude-code".to_owned()],
            ends: vec![end('1')],
        }
    }

    fn facts(&self) -> CarrierFacts<'_> {
        CarrierFacts {
            condense: &self.condense,
            cache: self.cache.as_ref(),
            completion: self.completion,
            identity_agents: &self.agents,
            session_end: &self.ends,
        }
    }
}

fn choose(cases: &[Case]) -> Result<Admitted, CarrierOwed> {
    let facts: Vec<_> = cases.iter().map(Case::facts).collect();
    choose_carrier(REPO, &facts)
}

fn refused_only(cases: &[Case], reason: Reason) {
    let Err(CarrierOwed::NoneAdmissible(counts)) = choose(cases) else {
        panic!("expected no admissible carrier");
    };
    assert_eq!(counts.into_iter().collect::<Vec<_>>(), vec![(reason, 1)]);
}

#[test]
fn a_queued_condense_with_an_ending_cache_and_one_end_event_is_the_carrier() {
    let admitted = choose(&[Case::genuine("s-1", 'a')]).unwrap();
    let expected = Carrier::new("claude-code", "s-1", &hex('a'), &hex('b')).unwrap();
    assert_eq!(admitted.carrier, expected);
    assert_eq!(admitted.cache_sha256, hex('c'));
    assert_eq!(admitted.end_event_key, hex('1'));
    assert_eq!(admitted.end_payload_sha256, hex('e'));
}

#[test]
fn two_admissible_condenses_refuse_as_ambiguous_and_never_pick_one() {
    let same_session = [Case::genuine("s-1", 'a'), Case::genuine("s-1", 'd')];
    assert_eq!(
        choose(&same_session),
        Err(CarrierOwed::Ambiguous { admissible: 2 })
    );
    let two_sessions = [Case::genuine("s-1", 'a'), Case::genuine("s-2", 'd')];
    assert_eq!(
        choose(&two_sessions),
        Err(CarrierOwed::Ambiguous { admissible: 2 })
    );
}

#[test]
fn an_inadmissible_condense_beside_one_admissible_does_not_block_the_choice() {
    let mut ended = Case::genuine("s-2", 'd');
    ended.cache = Some(cache("s-2", "ended"));
    let admitted = choose(&[ended, Case::genuine("s-1", 'a')]).unwrap();
    assert_eq!(admitted.end_event_key, hex('1'));
}

#[test]
fn no_queued_condense_is_owed_and_says_so() {
    assert_eq!(choose(&[]), Err(CarrierOwed::NoCondense));
}

#[test]
fn the_end_event_must_be_present_and_unique() {
    let mut missing = Case::genuine("s-1", 'a');
    missing.ends.clear();
    refused_only(&[missing], Reason::EndMissing);
    let mut two = Case::genuine("s-1", 'a');
    two.ends.push(end('2'));
    refused_only(&[two], Reason::EndAmbiguous);
    let mut redelivered = Case::genuine("s-1", 'a');
    redelivered.ends.push(end('1'));
    refused_only(&[redelivered], Reason::EndAmbiguous);
    let mut malformed = Case::genuine("s-1", 'a');
    malformed.ends[0].payload_sha256 = "not-hex".to_owned();
    refused_only(&[malformed], Reason::EndMalformed);
}

#[test]
fn the_cache_must_be_present_ending_and_the_same_identity() {
    let mut absent = Case::genuine("s-1", 'a');
    absent.cache = None;
    refused_only(&[absent], Reason::CacheAbsent);
    let mut active = Case::genuine("s-1", 'a');
    active.cache = Some(cache("s-1", "active"));
    refused_only(&[active], Reason::CacheNotEnding);
    let mut ended = Case::genuine("s-1", 'a');
    ended.cache = Some(cache("s-1", "ended"));
    refused_only(&[ended], Reason::Completed);
    let mut other_agent = Case::genuine("s-1", 'a');
    if let Some(cache) = other_agent.cache.as_mut() {
        cache.agent = "codex".to_owned();
    }
    refused_only(&[other_agent], Reason::CacheIdentity);
    let mut other_session = Case::genuine("s-1", 'a');
    other_session.cache = Some(cache("s-9", "ending"));
    refused_only(&[other_session], Reason::CacheIdentity);
}

#[test]
fn a_native_session_under_two_agents_refuses_rather_than_choosing_one() {
    let mut two = Case::genuine("s-1", 'a');
    two.agents.push("codex".to_owned());
    refused_only(&[two], Reason::IdentityAmbiguous);
    let mut none = Case::genuine("s-1", 'a');
    none.agents.clear();
    refused_only(&[none], Reason::IdentityMissing);
    let mut other = Case::genuine("s-1", 'a');
    other.agents = vec!["codex".to_owned()];
    refused_only(&[other], Reason::IdentityMismatch);
}

#[test]
fn a_prior_completion_or_an_unreadable_one_is_never_grafted_onto() {
    let mut done = Case::genuine("s-1", 'a');
    done.completion = Completion::Completed;
    refused_only(&[done], Reason::Completed);
    let mut unknown = Case::genuine("s-1", 'a');
    unknown.completion = Completion::Unknown;
    refused_only(&[unknown], Reason::CompletionUnknown);
}

#[test]
fn a_condense_of_another_repository_or_with_an_invalid_digest_refuses() {
    let mut foreign = Case::genuine("s-1", 'a');
    foreign.condense.repo_id = "family-2".to_owned();
    refused_only(&[foreign], Reason::RepoMismatch);
    let mut unattributed = Case::genuine("s-1", 'a');
    unattributed.condense.repo_id = String::new();
    refused_only(&[unattributed], Reason::RepoMismatch);
    let mut bad = Case::genuine("s-1", 'a');
    bad.condense.ticket_sha256 = "zz".to_owned();
    refused_only(&[bad], Reason::TicketInvalid);
}
