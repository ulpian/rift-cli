//! One native tool call's three outcomes, joined by the client's own id.
//!
//! A decision Rift makes about a call is one fact, the bytes it wrote to
//! stdout and the code it exited with are a second, and what the client's
//! next event about the same call shows is a third. They are kept apart so a
//! diagnostic can say "Rift emitted a decline and the client ran the call
//! anyway" instead of blaming the client for a response that never left the
//! hook. The join key is the id the client itself gave the call; a call that
//! arrived without one is recorded with no id and is never paired by time or
//! by path, because a guessed pairing is a claim nobody verified.

use serde_json::{json, Map, Value};

use super::line::text_at;
use super::Line;

/// Which of the three outcomes a `call` line records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallFact {
    /// Rift decided to decline the call.
    Planned,
    /// What the hook did with the response it composed for the call.
    Emitted {
        /// Whether the whole response was written and flushed to stdout.
        wrote: bool,
        /// The status the hook process exits with.
        exit: i32,
    },
    /// The client reported a result for the call, so the call ran.
    Applied,
}

impl CallFact {
    const fn token(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Emitted { .. } => "emitted",
            Self::Applied => "applied",
        }
    }
}

/// The fields of a `call` row past the shared header.
pub(super) fn insert_call_fields(line: &Line, object: &mut Map<String, Value>) {
    let Line::Call {
        call, fact, at_ms, ..
    } = line
    else {
        return;
    };
    object.insert("id".to_owned(), json!(call));
    object.insert("f".to_owned(), json!(fact.token()));
    if let CallFact::Emitted { wrote, exit } = fact {
        object.insert("w".to_owned(), json!(wrote));
        object.insert("x".to_owned(), json!(exit));
    }
    object.insert("at".to_owned(), json!(at_ms));
}

/// The `call` line a row states, or `None` when any field is malformed.
pub(super) fn call_line_from(object: &Map<String, Value>, turn: u32, at_ms: u64) -> Option<Line> {
    let call = match object.get("id")? {
        Value::Null => None,
        Value::String(id) if !id.is_empty() => Some(id.clone()),
        _ => return None,
    };
    let fact = match text_at(object, "f")? {
        "planned" => CallFact::Planned,
        "applied" => CallFact::Applied,
        "emitted" => CallFact::Emitted {
            wrote: object.get("w")?.as_bool()?,
            exit: i32::try_from(object.get("x")?.as_i64()?).ok()?,
        },
        _ => return None,
    };
    Some(Line::Call {
        turn,
        call,
        fact,
        at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_call_fact_reads_back_as_written_with_or_without_an_id() {
        for call in [Some("call_1".to_owned()), None] {
            for fact in [
                CallFact::Planned,
                CallFact::Emitted {
                    wrote: false,
                    exit: 0,
                },
                CallFact::Applied,
            ] {
                let line = Line::Call {
                    turn: 2,
                    call: call.clone(),
                    fact,
                    at_ms: 9,
                };
                assert_eq!(Line::from_json_line(&line.to_json_line()), Some(line));
            }
        }
    }

    #[test]
    fn a_call_row_with_an_unknown_fact_or_an_empty_id_is_dropped() {
        let bad_fact = r#"{"v":1,"k":"call","t":1,"id":"c","f":"guessed","at":1}"#;
        let empty_id = r#"{"v":1,"k":"call","t":1,"id":"","f":"planned","at":1}"#;
        assert_eq!(Line::from_json_line(bad_fact), None);
        assert_eq!(Line::from_json_line(empty_id), None);
    }
}
