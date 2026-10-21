//! The one grammar for the file names the agent queue writes: a job ticket
//! and every marker kind. The queue's writers spell names through the
//! builders here and every reader that must tell entries apart classifies
//! through [`classify`], so a new marker is added in one place and no reader
//! can fall behind it. Recovery once kept a second grammar that did not know
//! two newer markers, and refused the whole queue as unknown.

use std::path::Path;

/// What one direct entry of the agent queue is, by its name alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueName {
    /// An executable job ticket, `<stem>[--<context>].json`.
    Ticket,
    /// A marker the queue leaves beside its tickets; never a job.
    Marker(Marker),
    /// A file a publish is still writing, or a hidden temp.
    Stage,
    /// A name the queue never writes.
    Unknown,
}

/// The marker kinds, each a fact a reader may count but never runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Marker {
    /// `condensed-<h64>`: the per-session mark a finished condense leaves.
    Condensed,
    /// `completed-<h64>.done`: one condense delivery's completion record.
    Completed,
    /// `unavailable-<stem>`: a basis snapshot that completed unavailable.
    Unavailable,
    /// `<key>.error`: a failure note.
    Error,
}

const CONDENSED: &str = "condensed-";
const COMPLETED: &str = "completed-";
const DONE: &str = ".done";
const UNAVAILABLE: &str = "unavailable-";

/// The condense marker for a session, by its hashed key.
pub(crate) fn condensed(session_key: &str) -> String {
    format!("{CONDENSED}{session_key}")
}

/// The completion record of one condense delivery, by its context id.
pub(crate) fn completed(context_id: &str) -> String {
    format!("{COMPLETED}{context_id}{DONE}")
}

/// The record a job left when it completed unavailable, by its logical stem.
pub(crate) fn unavailable(stem: &str) -> String {
    format!("{UNAVAILABLE}{stem}")
}

/// Place one name in the queue's closed grammar. Extensions are compared
/// exactly: the queue spells them in lowercase, so an upper-case twin is a
/// name it never wrote.
pub(crate) fn classify(name: &str) -> QueueName {
    if name.starts_with('.') || has_extension(name, "stage") || has_extension(name, "tmp") {
        return QueueName::Stage;
    }
    if has_extension(name, "json") {
        return QueueName::Ticket;
    }
    marker(name).map_or(QueueName::Unknown, QueueName::Marker)
}

fn marker(name: &str) -> Option<Marker> {
    if let Some(key) = name.strip_prefix(CONDENSED) {
        return is_h64(key).then_some(Marker::Condensed);
    }
    if let Some(rest) = name.strip_prefix(COMPLETED) {
        let id = rest.strip_suffix(DONE)?;
        return is_h64(id).then_some(Marker::Completed);
    }
    if let Some(stem) = name.strip_prefix(UNAVAILABLE) {
        let stem_shape = |b: u8| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-';
        let fits = !stem.is_empty() && stem.bytes().all(stem_shape);
        return fits.then_some(Marker::Unavailable);
    }
    has_extension(name, "error").then_some(Marker::Error)
}

fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|actual| actual == extension)
}

fn is_h64(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_the_queue_writes_classifies_as_its_own_kind() {
        let h64 = "ab".repeat(32);
        let cases = [
            (format!("basis-{h64}.json"), QueueName::Ticket),
            (condensed(&h64), QueueName::Marker(Marker::Condensed)),
            (completed(&h64), QueueName::Marker(Marker::Completed)),
            (
                unavailable(&format!("basis-{h64}")),
                QueueName::Marker(Marker::Unavailable),
            ),
            ("agent-x.error".to_owned(), QueueName::Marker(Marker::Error)),
            (".delivery-1.stage".to_owned(), QueueName::Stage),
            ("x.tmp".to_owned(), QueueName::Stage),
        ];
        for (name, class) in cases {
            assert_eq!(classify(&name), class, "{name}");
        }
    }

    #[test]
    fn a_name_the_queue_never_writes_is_unknown() {
        for name in [
            "readme.txt",
            "condensed-short",
            "completed-abc.done",
            "unavailable-",
            "unavailable-Basis",
            "x.JSON",
        ] {
            assert_eq!(classify(name), QueueName::Unknown, "{name}");
        }
    }
}
