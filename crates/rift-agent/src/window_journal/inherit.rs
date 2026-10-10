//! What a context window holds because the window that started it held it:
//! the classes of that evidence, the rows that carry it, and the one place a
//! decision is allowed to lean on them.
//!
//! A module of its own because inheritance is the one source of coverage that
//! is not something this window was handed by a tool call. A `read` line is a
//! fact about this window; an `inherit` line is a claim made on its behalf by
//! the window that spawned it, and how far a decision may trust that claim is
//! a lever per class rather than a property of the row. Below `enforce` the
//! rows are inert to every question but one — what declining on them would
//! have been worth — so a class can be measured for as long as it takes
//! without any session losing a byte to it.
//!
//! The spawn and claim rows that link a subagent to the call that started it
//! live here too: they are the same act seen from its two ends, and the only
//! rows in a journal that speak about a window other than the one they are
//! written in.

use rift_core::Sha256Id;
use serde_json::{json, Map, Value};

use super::line::{hash_at, range_at, text_at, u32_at};
use super::{Decision, IntervalSet, Journal, Limits, Line, Mode, ResolvedRead};

/// What an inherited line rests on.
///
/// Ordered by precedence: when two classes would each make the same read a
/// would-be decline, it is counted under the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InheritClass {
    /// The window began from its spawner's own history, and so holds what
    /// that history held. No client this build knows signals such a start, so
    /// nothing writes this class yet; it is named so the lever and the
    /// accounting have a place for it when one does.
    Fork,
    /// The spawning prompt carried the lines themselves.
    Paste,
    /// The spawning prompt named the path, and the spawner held these lines
    /// of it: the window was pointed at bytes its spawner had, not handed
    /// them, which is why this class has a lever of its own.
    ByName,
}

/// How far a decision may lean on the lines of a fork.
pub const FORK_MODE: Mode = Mode::Observe;

/// How far a decision may lean on lines a spawning prompt carried.
pub const PASTE_MODE: Mode = Mode::Observe;

/// How far a decision may lean on each class of inherited line.
///
/// A field of [`Limits`] so the one pure decision answers for every class,
/// and a test can ask what enforcing a class would do without a second copy of
/// the decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InheritModes {
    /// Lines of a fork: [`FORK_MODE`].
    pub fork: Mode,
    /// Lines a spawning prompt carried: [`PASTE_MODE`].
    pub paste: Mode,
    /// Lines of a path a spawning prompt named: the repository's lever,
    /// never above the read lever it qualifies.
    pub by_name: Mode,
}

impl InheritClass {
    /// Every class, in precedence order.
    pub const ALL: [Self; 3] = [Self::Fork, Self::Paste, Self::ByName];

    /// The token this class is written as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fork => "fork",
            Self::Paste => "paste",
            Self::ByName => "by_name",
        }
    }

    /// The class a token names, or `None` for one this version does not know.
    #[must_use]
    pub fn from_str(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.as_str() == token)
    }

    /// The neutral tag an accounting row about this class carries.
    #[must_use]
    pub const fn audit_tag(self) -> &'static str {
        match self {
            Self::Fork => "inherited:fork",
            Self::Paste => "inherited:paste",
            Self::ByName => "inherited:by_name",
        }
    }
}

impl Default for InheritModes {
    fn default() -> Self {
        Self {
            fork: FORK_MODE,
            paste: PASTE_MODE,
            by_name: Mode::Observe,
        }
    }
}

impl InheritModes {
    /// The mode one class is at.
    #[must_use]
    pub const fn of(self, class: InheritClass) -> Mode {
        match class {
            InheritClass::Fork => self.fork,
            InheritClass::Paste => self.paste,
            InheritClass::ByName => self.by_name,
        }
    }
}

impl Journal {
    /// The lines of `(path, hash)` this window inherited and still holds, from
    /// the classes `admit` accepts.
    #[must_use]
    pub fn inherited_coverage(
        &self,
        path: &str,
        hash: Sha256Id,
        admit: impl Fn(InheritClass) -> bool,
    ) -> IntervalSet {
        let mut set = IntervalSet::new();
        for line in self.live() {
            if let Line::Inherit {
                class,
                path: p,
                hash: h,
                range,
                ..
            } = line
            {
                if admit(*class) && p == path && *h == hash {
                    set.insert(*range);
                }
            }
        }
        set
    }

    /// The claim the window `agent` has already made, if it made one: `Some`
    /// of the spawn it named, which is itself `None` where it could name none.
    #[must_use]
    pub fn claim_of(&self, agent: &str) -> Option<Option<&str>> {
        self.lines.iter().find_map(|line| match line {
            Line::Claim { agent: a, call, .. } if a == agent => Some(call.as_deref()),
            _ => None,
        })
    }

    /// The one spawn line no window has claimed yet that was made under the
    /// user prompt `prompt` for a subagent of type `agent_type`, or `None`
    /// when there is not exactly one.
    ///
    /// Exactness is the whole contract. A window's own spawn line is always
    /// written before the window opens, and a window only ever claims a spawn
    /// it found alone, so a sole unclaimed match is always the caller's own.
    /// Two candidates are two siblings started under one prompt with one type,
    /// and guessing between them could hand one sibling the other's paths, so
    /// the answer is neither.
    #[must_use]
    pub fn sole_spawn(&self, prompt: &str, agent_type: &str) -> Option<&Line> {
        let mut found = self.lines.iter().filter(|line| match line {
            Line::Spawn {
                call,
                prompt: p,
                agent_type: t,
                ..
            } => p == prompt && t == agent_type && !self.spawn_is_claimed(call),
            _ => false,
        });
        let sole = found.next()?;
        found.next().is_none().then_some(sole)
    }

    /// Whether some window has already claimed the spawn `call`.
    fn spawn_is_claimed(&self, call: &str) -> bool {
        self.lines
            .iter()
            .any(|line| matches!(line, Line::Claim { call: Some(c), .. } if c == call))
    }

    /// This journal with every inherited line `admit` accepts standing as
    /// the read it inherits, or `None` when it accepts none — so a window with
    /// nothing inherited is decided over its own lines without a copy.
    fn admitting(&self, admit: impl Fn(InheritClass) -> bool) -> Option<Self> {
        let admitted = |line: &Line| matches!(line, Line::Inherit { class, .. } if admit(*class));
        if !self.lines.iter().any(admitted) {
            return None;
        }
        let lines = self
            .lines
            .iter()
            .map(|line| match line {
                Line::Inherit {
                    turn,
                    path,
                    hash,
                    range,
                    total_lines,
                    at_ms,
                    ..
                } if admitted(line) => Line::Read {
                    turn: *turn,
                    path: path.clone(),
                    hash: *hash,
                    range: *range,
                    total_lines: *total_lines,
                    at_ms: *at_ms,
                },
                other => other.clone(),
            })
            .collect();
        Some(Self::from_lines(lines))
    }
}

/// The journal a decision is taken over: this window's own lines, with the
/// inherited lines of every class at `enforce` standing as reads, and without
/// a copy when there are none.
pub(super) fn enforced_view(journal: &Journal, limits: Limits) -> Option<Journal> {
    journal.admitting(|class| limits.inherit.of(class) == Mode::Enforce)
}

/// What a read this window's own lines allow would have been, had the first
/// observed class of inherited line that makes it a decline been enforced —
/// or `None` when none does.
///
/// `decide` is the shipped decision, asked again over a journal in which that
/// class's lines stand as reads. Narrowing is switched off for the question:
/// a partly-held read is not a re-read of inherited lines, and the frame that
/// answers it asks for lines the window does not hold either way.
pub(super) fn inherited_observation(
    journal: &Journal,
    limits: Limits,
    decide: impl Fn(&Journal, Limits) -> Decision,
) -> Option<Decision> {
    let asked = Limits {
        narrow_mode: Mode::Off,
        ..limits
    };
    InheritClass::ALL
        .into_iter()
        .filter(|class| limits.inherit.of(*class) == Mode::Observe)
        .find_map(|class| {
            let view =
                journal.admitting(|c| c == class || limits.inherit.of(c) == Mode::Enforce)?;
            match decide(&view, asked) {
                Decision::Deny(shape) | Decision::Observe(shape) => {
                    Some(Decision::Inherited { shape, class })
                }
                Decision::Allow | Decision::Inherited { .. } => None,
            }
        })
}

/// The observation an inherited decision leaves beside the read it records,
/// marked with its class, or `None` for every other decision.
///
/// Two lines for one call because the call did two things: it ran, so the
/// window now holds what it delivered — the `read` [`record`](super::record)
/// writes — and it was a read an inherited line would have declined, which is
/// what this line sizes.
#[must_use]
pub fn record_inheritance(
    read: &ResolvedRead,
    decision: &Decision,
    turn: u32,
    at_ms: u64,
) -> Option<Line> {
    let Decision::Inherited { class, .. } = decision else {
        return None;
    };
    Some(Line::Observe {
        turn,
        path: read.path.clone(),
        hash: read.hash,
        range: read.range,
        at_ms,
        via: Some(*class),
    })
}

/// An optional member that is present but not in its shape: the row is
/// dropped rather than read as if the member were absent.
pub(super) struct Malformed;

/// The optional `c` member of an observation: `Ok(None)` when absent, and
/// [`Malformed`] when present and not a class this version knows, which drops
/// the row rather than reading a class it cannot name as none at all.
pub(super) fn class_at(object: &Map<String, Value>) -> Result<Option<InheritClass>, Malformed> {
    match object.get("c") {
        None => Ok(None),
        Some(_) => text_at(object, "c")
            .and_then(InheritClass::from_str)
            .map(Some)
            .ok_or(Malformed),
    }
}

/// The members past the shared header of the three kinds this module owns.
pub(super) fn insert_inherited_fields(line: &Line, object: &mut Map<String, Value>) {
    let mut put = |member: &str, value: Value| {
        object.insert(member.to_owned(), value);
    };
    match line {
        Line::Inherit {
            class,
            path,
            hash,
            range,
            total_lines,
            at_ms,
            ..
        } => {
            put("c", json!(class.as_str()));
            put("p", json!(path));
            put("h", json!(hash.to_hex()));
            put("lo", json!(range.lo));
            put("hi", json!(range.hi));
            put("n", json!(total_lines));
            put("at", json!(at_ms));
        }
        Line::Spawn {
            call,
            prompt,
            agent_type,
            spawner,
            names,
            at_ms,
            ..
        } => {
            put("call", json!(call));
            put("prompt", json!(prompt));
            put("type", json!(agent_type));
            if let Some(spawner) = spawner {
                put("by", json!(spawner));
            }
            put("names", json!(names));
            put("at", json!(at_ms));
        }
        Line::Claim {
            agent, call, at_ms, ..
        } => {
            put("agent", json!(agent));
            if let Some(call) = call {
                put("call", json!(call));
            }
            put("at", json!(at_ms));
        }
        _ => {}
    }
}

/// The line one of this module's kinds states, or `None` for a row that is
/// not one this version wrote — including a kind no module knows.
pub(super) fn inherited_line_from(
    kind: &str,
    object: &Map<String, Value>,
    turn: u32,
    at_ms: u64,
) -> Option<Line> {
    match kind {
        "inherit" => Some(Line::Inherit {
            turn,
            class: InheritClass::from_str(text_at(object, "c")?)?,
            path: text_at(object, "p")?.to_owned(),
            hash: hash_at(object, "h")?,
            range: range_at(object)?,
            total_lines: u32_at(object, "n")?,
            at_ms,
        }),
        "spawn" => Some(Line::Spawn {
            turn,
            call: ident_at(object, "call")?.to_owned(),
            prompt: ident_at(object, "prompt")?.to_owned(),
            agent_type: ident_at(object, "type")?.to_owned(),
            spawner: optional_ident_at(object, "by").ok()?,
            names: names_at(object)?,
            at_ms,
        }),
        "claim" => Some(Line::Claim {
            turn,
            agent: ident_at(object, "agent")?.to_owned(),
            call: optional_ident_at(object, "call").ok()?,
            at_ms,
        }),
        _ => None,
    }
}

/// The longest identifier a spawn or claim row carries.
const MAX_IDENT_CHARS: usize = 128;

/// The most paths one spawn line names.
pub const MAX_SPAWN_NAMES: usize = 32;

/// The longest path a spawn line names, in bytes.
const MAX_NAME_BYTES: usize = 1_024;

/// An identifier member: printable ASCII with no space, and short. A client's
/// call, prompt, agent and type ids all have that shape, and one that does not
/// is a row nobody here wrote.
fn ident_at<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    let text = text_at(object, key)?;
    is_ident(text).then_some(text)
}

/// Whether `text` has the one shape an identifier member may have.
#[must_use]
pub fn is_ident(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_IDENT_CHARS && text.bytes().all(|b| b.is_ascii_graphic())
}

/// An optional identifier member: `Ok(None)` when absent, [`Malformed`]
/// when present and not an identifier's shape.
fn optional_ident_at(object: &Map<String, Value>, key: &str) -> Result<Option<String>, Malformed> {
    match object.get(key) {
        None => Ok(None),
        Some(_) => ident_at(object, key)
            .map(|text| Some(text.to_owned()))
            .ok_or(Malformed),
    }
}

/// The `names` member of a spawn line: a bounded list of non-empty paths with
/// no control character in them. One malformed entry drops the line.
fn names_at(object: &Map<String, Value>) -> Option<Vec<String>> {
    let entries = object.get("names")?.as_array()?;
    if entries.len() > MAX_SPAWN_NAMES {
        return None;
    }
    entries
        .iter()
        .map(|entry| {
            let name = entry.as_str()?;
            let fits = !name.is_empty() && name.len() <= MAX_NAME_BYTES;
            (fits && !name.chars().any(char::is_control)).then(|| name.to_owned())
        })
        .collect()
}

/// The inherited line a copied read becomes, or `None` for any other line.
#[must_use]
pub fn inherited_from_read(
    line: &Line,
    class: InheritClass,
    turn: u32,
    at_ms: u64,
) -> Option<Line> {
    let Line::Read {
        path,
        hash,
        range,
        total_lines,
        ..
    } = line
    else {
        return None;
    };
    Some(Line::Inherit {
        turn,
        class,
        path: path.clone(),
        hash: *hash,
        range: *range,
        total_lines: *total_lines,
        at_ms,
    })
}

#[cfg(test)]
mod tests;
