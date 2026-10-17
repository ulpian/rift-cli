//! The rewrite forms a hook may deliver in place of a tool's result, and the
//! rendering contract the rewriter, the archive and `rift verify` share.
//!
//! A hook that rewrites a Bash result (Q72) hands the model a text that is not
//! the one the tool produced. The original is spooled before the rewrite is
//! delivered and sealed beside the transcript at archive
//! ([`super::ArchivedOriginal`]); what binds the two is this module: a closed
//! set of forms, line ranges that name the stream they cut, and one check per
//! form of the text the model was given against the original
//! ([`check_rendering`]). The rewriter tests its output against the same
//! predicates verify holds the delivered text to, so the two cannot hold it to
//! two different contracts.
//!
//! **The rendering contract.** A stream's lines are its text split on `\n`
//! (a trailing newline leaves a last, empty line, and joining on `\n` gives the
//! text back byte for byte). A range is 0-based and end-exclusive over those
//! lines. Every line outside the ranges is delivered verbatim and in order;
//! each range is delivered as exactly one marker line:
//!
//! * `folded_output` — a fold line ([`parse_fold_line`]): one whole line
//!   that opens `[rift fold: `, whose first token is `<n>`, the range's
//!   length, and whose last character is `]`. The words between the count and
//!   the closer are the rewriter's, written where the sentences a model reads
//!   are written; this contract fixes only the shape and the count, so
//!   rift-shadow renders no fold prose of its own.
//! * `held_output_substitution` — a held frame, `[rift held: …]`, whose words
//!   name the revision the replaced lines are held as. The frame's words are
//!   the rewriter's; this contract fixes only that it is one line in that
//!   shape. Whether the replaced lines equal the held revision needs the
//!   revision, so verify counts each such range `held_unchecked` rather than
//!   passing it silently.
//!
//! A marker line is recognised only as a whole line, so a result that merely
//! prints the words (a `cat` of this file) is not mistaken for a rewrite.

/// The longest list of ranges one rewrite may name.
pub const MAX_REPLACED_RANGES: usize = 1024;

/// What opens a fold line.
pub const FOLD_OPEN: &str = "[rift fold: ";

/// What closes a fold line: its last character, so the rewriter's words may
/// themselves hold a `]`.
const FOLD_CLOSE: char = ']';

/// What opens a held frame.
pub const HELD_FRAME_OPEN: &str = "[rift held: ";

/// The two rewrite forms Q72 grants. Closed: a rewrite in any other form is
/// not one a hook may deliver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewriteForm {
    /// Replaced lines delivered as a held frame naming their revision.
    HeldOutputSubstitution,
    /// Replaced lines delivered as one fold line each range.
    FoldedOutput,
}

impl RewriteForm {
    /// Every form, in vocabulary order.
    pub const ALL: [Self; 2] = [Self::HeldOutputSubstitution, Self::FoldedOutput];

    /// The form's word in the spool and the manifest.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::HeldOutputSubstitution => "held_output_substitution",
            Self::FoldedOutput => "folded_output",
        }
    }

    /// The form a word names, `None` outside the vocabulary.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.word() == word)
    }
}

/// The stream of a Bash result a range cuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl Stream {
    /// The stream's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }

    /// The stream a word names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::Stdout, Self::Stderr]
            .into_iter()
            .find(|s| s.word() == word)
    }
}

/// One run of an original's lines a rewrite replaced: 0-based, end-exclusive,
/// over the lines of the named stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineRange {
    /// The stream it cuts.
    pub stream: Stream,
    /// The first replaced line.
    pub start: u32,
    /// One past the last replaced line.
    pub end: u32,
}

impl LineRange {
    /// The range as the spool and the manifest record it.
    #[must_use]
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({ "end": self.end, "start": self.start, "stream": self.stream.word() })
    }

    /// The range a recorded value names, `None` when it is not one.
    #[must_use]
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        let num = |k: &str| u32::try_from(v.get(k)?.as_u64()?).ok();
        Some(Self {
            stream: Stream::from_word(v.get("stream")?.as_str()?)?,
            start: num("start")?,
            end: num("end")?,
        })
    }
}

/// What the client recorded for a spooled original, judged from the
/// transcript and never from the spool. Closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rewritten {
    /// The client recorded a text other than the original: the rewrite in
    /// this form was delivered, and verify holds it to the form's rendering.
    Delivered(RewriteForm),
    /// The client recorded the original itself: the hook spooled it and no
    /// rewrite reached the model.
    SpooledNotDelivered,
}

impl Rewritten {
    /// The word the manifest records.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Delivered(form) => form.word(),
            Self::SpooledNotDelivered => "spooled_not_delivered",
        }
    }

    /// The value a word names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        if word == "spooled_not_delivered" {
            return Some(Self::SpooledNotDelivered);
        }
        RewriteForm::from_word(word).map(Self::Delivered)
    }

    /// What the client recorded, given the original and the text the
    /// transcript holds: the original itself means nothing was delivered.
    #[must_use]
    pub fn judge(form: RewriteForm, original: ResultText<'_>, delivered: ResultText<'_>) -> Self {
        if original == delivered {
            Self::SpooledNotDelivered
        } else {
            Self::Delivered(form)
        }
    }
}

/// The two streams of one Bash result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResultText<'a> {
    /// Standard output.
    pub stdout: &'a str,
    /// Standard error.
    pub stderr: &'a str,
}

impl<'a> ResultText<'a> {
    fn stream(self, stream: Stream) -> &'a str {
        match stream {
            Stream::Stdout => self.stdout,
            Stream::Stderr => self.stderr,
        }
    }
}

/// How a delivered text compares with the rendering of its original.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderCheck {
    /// The delivered text is the form's rendering of the original.
    Matches,
    /// Every line outside the ranges is verbatim and each range is a held
    /// frame; the frames' revisions were not read, so this many ranges are
    /// unchecked rather than passed.
    HeldUnchecked(usize),
    /// The delivered text is not the form's rendering of the original.
    Mismatch,
}

/// The count a whole line folds, `None` when the line is not a fold line.
///
/// The one predicate both sides call: the rewriter tests the fold text it
/// writes against it, and verify holds a delivered fold to it. The shape is
/// [`FOLD_OPEN`], then the count — a positive decimal without a leading zero
/// or sign, ended by a space or by the closer — then the rewriter's words,
/// then `]` as the line's last character. A line holding a newline is not one
/// line, so it is never a fold line.
#[must_use]
pub fn parse_fold_line(line: &str) -> Option<u32> {
    if line.contains('\n') {
        return None;
    }
    let inner = line.strip_prefix(FOLD_OPEN)?.strip_suffix(FOLD_CLOSE)?;
    let count = inner.split(' ').next()?;
    let n = count.parse::<u32>().ok()?;
    (n > 0 && n.to_string() == count).then_some(n)
}

/// Whether a whole line is a held frame.
#[must_use]
pub fn is_held_frame(line: &str) -> bool {
    line.strip_prefix(HELD_FRAME_OPEN)
        .is_some_and(|rest| rest.len() > 1 && rest.ends_with(']'))
}

/// Whether a whole line is a Rift rewrite marker of either form.
#[must_use]
pub fn is_rift_marker_line(line: &str) -> bool {
    parse_fold_line(line).is_some() || is_held_frame(line)
}

/// Whether `ranges` are a replacement set for `original`: at least one and at
/// most [`MAX_REPLACED_RANGES`], each non-empty and inside its stream, and a
/// stream's ranges ascending without overlap.
#[must_use]
pub fn ranges_fit(original: ResultText<'_>, ranges: &[LineRange]) -> bool {
    if ranges.is_empty() || ranges.len() > MAX_REPLACED_RANGES {
        return false;
    }
    [Stream::Stdout, Stream::Stderr].into_iter().all(|stream| {
        let lines = original.stream(stream).split('\n').count();
        let mut floor = 0usize;
        ranges.iter().filter(|r| r.stream == stream).all(|r| {
            let (start, end) = (r.start as usize, r.end as usize);
            let fits = floor <= start && start < end && end <= lines;
            floor = end;
            fits
        })
    })
}

/// The original as a `folded_output` rewrite delivers it, each range replaced
/// by the line `fold_line` writes for its length; `None` when the ranges do
/// not fit it, or when a fold line does not read back as its own count. The
/// fold line's words are the caller's, so the second refusal is what holds
/// them to the shape the checker reads: a bad frames text fails here and the
/// original goes through unchanged, rather than becoming a fold the record
/// would misread. Each marker is generated once, checked, and retained for
/// emission, including when the callback carries interior state.
#[must_use]
pub fn render_folded(
    original: ResultText<'_>,
    ranges: &[LineRange],
    fold_line: impl Fn(u32) -> String,
) -> Option<(String, String)> {
    if !ranges_fit(original, ranges) {
        return None;
    }
    // A Fn can carry interior state: retain the exact bytes we validate,
    // rather than invoking it again when emitting a stream.
    let markers = ranges
        .iter()
        .map(|r| {
            let n = r.end - r.start;
            let marker = fold_line(n);
            (parse_fold_line(&marker) == Some(n)).then_some((*r, marker))
        })
        .collect::<Option<Vec<_>>>()?;
    let fold = |stream| fold_stream(original.stream(stream), &markers, stream);
    Some((fold(Stream::Stdout), fold(Stream::Stderr)))
}

/// One stream with each of its ranges folded to one line.
fn fold_stream(text: &str, markers: &[(LineRange, String)], stream: Stream) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut at = 0usize;
    for (r, marker) in markers.iter().filter(|(r, _)| r.stream == stream) {
        let (start, end) = (r.start as usize, r.end as usize);
        out.extend(
            lines
                .get(at..start)
                .unwrap_or_default()
                .iter()
                .map(|l| (*l).to_owned()),
        );
        out.push(marker.clone());
        at = end;
    }
    out.extend(
        lines
            .get(at..)
            .unwrap_or_default()
            .iter()
            .map(|l| (*l).to_owned()),
    );
    out.join("\n")
}

/// Compare a delivered text with the form's rendering of its original.
#[must_use]
pub fn check_rendering(
    form: RewriteForm,
    original: ResultText<'_>,
    delivered: ResultText<'_>,
    ranges: &[LineRange],
) -> RenderCheck {
    if !ranges_fit(original, ranges) {
        return RenderCheck::Mismatch;
    }
    let marker_fits = |line: &str, r: &LineRange| match form {
        RewriteForm::FoldedOutput => parse_fold_line(line) == Some(r.end - r.start),
        RewriteForm::HeldOutputSubstitution => is_held_frame(line),
    };
    let fits = [Stream::Stdout, Stream::Stderr].into_iter().all(|stream| {
        stream_matches(
            StreamPair {
                original: original.stream(stream),
                delivered: delivered.stream(stream),
                stream,
            },
            ranges,
            marker_fits,
        )
    });
    match (fits, form) {
        (false, _) => RenderCheck::Mismatch,
        (true, RewriteForm::FoldedOutput) => RenderCheck::Matches,
        (true, RewriteForm::HeldOutputSubstitution) => RenderCheck::HeldUnchecked(ranges.len()),
    }
}

/// One stream's original and delivered text.
#[derive(Clone, Copy)]
struct StreamPair<'a> {
    original: &'a str,
    delivered: &'a str,
    stream: Stream,
}

/// Whether one delivered stream is its original with each range replaced by
/// exactly one marker line `marker_fits` accepts for that range, and every
/// other line verbatim and in order.
fn stream_matches(
    StreamPair {
        original,
        delivered,
        stream,
    }: StreamPair<'_>,
    ranges: &[LineRange],
    marker_fits: impl Fn(&str, &LineRange) -> bool,
) -> bool {
    let original: Vec<&str> = original.split('\n').collect();
    let mut delivered = delivered.split('\n');
    let mut at = 0usize;
    for r in ranges.iter().filter(|r| r.stream == stream) {
        let kept = original.get(at..r.start as usize).unwrap_or_default();
        if !kept.iter().all(|line| delivered.next() == Some(*line)) {
            return false;
        }
        if !delivered.next().is_some_and(|line| marker_fits(line, r)) {
            return false;
        }
        at = r.end as usize;
    }
    let tail = original.get(at..).unwrap_or_default();
    tail.iter().all(|line| delivered.next() == Some(*line)) && delivered.next().is_none()
}

/// The result a transcript line records for its tool call: the client's
/// `toolUseResult` streams when it carries them, else the text of the line's
/// one `tool_result` block as standard output. `None` when the line records
/// no result this way.
#[must_use]
pub fn delivered_result(line: &[u8]) -> Option<(String, String)> {
    let record: serde_json::Value = serde_json::from_slice(line).ok()?;
    let text =
        |v: Option<&serde_json::Value>| v.and_then(serde_json::Value::as_str).map(str::to_owned);
    if let Some(stdout) = text(record.get("toolUseResult").and_then(|r| r.get("stdout"))) {
        let stderr = text(record.get("toolUseResult").and_then(|r| r.get("stderr")));
        return Some((stdout, stderr.unwrap_or_default()));
    }
    let blocks = record.get("message")?.get("content")?.as_array()?;
    let mut results = blocks
        .iter()
        .filter(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("tool_result"));
    let block = results.next()?;
    if results.next().is_some() {
        return None;
    }
    let content = match block.get("content")? {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => text(parts.first()?.get("text"))?,
        _ => return None,
    };
    Some((content, String::new()))
}

/// The canonical bytes of one original result — the JSON object
/// `{"interrupted","stderr","stdout"}` with its keys sorted, on one line. The
/// spool's body and the sealed artifact are this, redacted.
#[must_use]
pub fn original_record_json(stdout: &str, stderr: &str, interrupted: bool) -> Vec<u8> {
    let value =
        serde_json::json!({ "interrupted": interrupted, "stderr": stderr, "stdout": stdout });
    serde_json::to_vec(&value).unwrap_or_default()
}

/// The streams and the interrupted flag of an original's (redacted) bytes,
/// `None` when they are not that object.
#[must_use]
pub fn parse_original_record(body: &[u8]) -> Option<(String, String, bool)> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    if object.len() != 3 {
        return None;
    }
    Some((
        object.get("stdout")?.as_str()?.to_owned(),
        object.get("stderr")?.as_str()?.to_owned(),
        object.get("interrupted")?.as_bool()?,
    ))
}

#[cfg(test)]
mod tests;
