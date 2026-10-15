use super::*;
use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

struct Output {
    bytes: Vec<u8>,
    limit: usize,
    fail_flush: bool,
    events: Rc<RefCell<Vec<&'static str>>>,
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let accepted = bytes.len().min(self.limit.saturating_sub(self.bytes.len()));
        if accepted == 0 {
            return Err(io::Error::other("fixture write refused"));
        }
        self.bytes.extend_from_slice(&bytes[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.events.borrow_mut().push("flush");
        if self.fail_flush {
            Err(io::Error::other("fixture flush refused"))
        } else {
            Ok(())
        }
    }
}

fn pending_response(repo: &Path) -> (rift_recall_types::ContextPlan, serde_json::Value) {
    let raw = rift_shadow::hooks::RawHook {
        agent: rift_core::AgentId::ANTIGRAVITY,
        verb: "PreToolUse".to_owned(),
        session: "emission".to_owned(),
        turn: None,
        occurred_at_ms: 10,
        payload: serde_json::json!({}),
    };
    arm_pending_replacement(repo, &raw, "[rift] unchanged lines 4-20");
    let candidate = pending_replacement_candidate(
        repo,
        &rift_core::AgentId::ANTIGRAVITY,
        "PreInvocation",
        br#"{"session_id":"emission","invocationNum":7}"#,
    )
    .unwrap();
    let plan = rift_recall_serve::ServeDispatcher::plan_context([candidate]);
    let response = rift_agent::respond::respond_with_plan(
        &rift_core::AgentId::ANTIGRAVITY,
        "PreInvocation",
        &plan,
        &rift_agent::respond::GateDecision::Allow,
    )
    .unwrap();
    (plan, response)
}

fn failed_emission_keeps_pending(limit: Option<usize>, fail_flush: bool) {
    let repo = tempfile::tempdir().unwrap();
    let (plan, response) = pending_response(repo.path());
    let marker = pending_replacement_path(repo.path(), "emission");
    let original = std::fs::read(&marker).unwrap();
    let encoded = format!("{response}\n").into_bytes();
    let limit = limit.unwrap_or(encoded.len() - 1);
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut output = Output {
        bytes: Vec::new(),
        limit,
        fail_flush,
        events: events.clone(),
    };
    let result = emit_response(&response, &mut output, || {
        events.borrow_mut().push("settled");
        complete_pending_replacement_in(repo.path(), "emission", &plan);
    });
    assert!(result.is_err());
    assert_eq!(output.bytes, encoded[..limit.min(encoded.len())]);
    assert!(
        !events.borrow().contains(&"settled"),
        "failed local emission must not settle"
    );
    assert_eq!(std::fs::read(marker).unwrap(), original);
}

#[test]
fn immediate_write_failure_keeps_pending_without_settlement() {
    failed_emission_keeps_pending(Some(0), false);
}

#[test]
fn partial_json_write_failure_keeps_pending_without_settlement() {
    failed_emission_keeps_pending(Some(3), false);
}

#[test]
fn newline_write_failure_keeps_pending_without_settlement() {
    failed_emission_keeps_pending(None, false);
}

#[test]
fn flush_failure_after_complete_json_keeps_pending_without_settlement() {
    failed_emission_keeps_pending(Some(usize::MAX), true);
}

#[test]
fn successful_local_flush_precedes_exactly_one_completion() {
    let repo = tempfile::tempdir().unwrap();
    let (plan, response) = pending_response(repo.path());
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut output = Output {
        bytes: Vec::new(),
        limit: usize::MAX,
        fail_flush: false,
        events: events.clone(),
    };
    emit_response(&response, &mut output, || {
        assert_eq!(*events.borrow(), ["flush"]);
        events.borrow_mut().push("settled");
        complete_pending_replacement_in(repo.path(), "emission", &plan);
    })
    .unwrap();
    assert_eq!(output.bytes, format!("{response}\n").as_bytes());
    assert_eq!(*events.borrow(), ["flush", "settled"]);
    assert!(!pending_replacement_path(repo.path(), "emission").exists());
}
