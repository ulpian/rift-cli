//! The sealed originals of rewritten results, read back and held to the text
//! the client recorded.
//!
//! Three checks, after the shard's root has bound every body as a leaf:
//!
//! * **Digest.** Each body the `originals` manifest beside a transcript claims
//!   must be in the shard with the digest and size recorded, its form and
//!   state words in their vocabularies — one that is not is named by path.
//! * **Rendering.** The delivered text on the transcript line the entry names
//!   must be the form's rendering of the original
//!   ([`rift_shadow::backend::rewrite::check_rendering`]), or the original
//!   itself when the entry says `spooled_not_delivered`. A fold is held to its
//!   shape, not its words: every fold line passes
//!   [`rift_shadow::backend::rewrite::parse_fold_line`], its count equals the
//!   length of the range it stands for, and every line outside the ranges is
//!   verbatim. A held substitution's
//!   replaced lines need the held revision, which this pass does not read:
//!   each such range is counted `held_unchecked`, reported, never passed.
//! * **The reverse.** A transcript line whose delivered text carries a Rift
//!   marker line with no manifest entry for that line is `original_missing`:
//!   a rewrite whose original was never sealed. A structure finding, not
//!   tamper: the shard is what its writer wrote, and what it lacks is a capture.

use rift_core::Result;
use rift_recall_substrate::SubstrateReader;
use rift_shadow::backend::rewrite::{
    check_rendering, delivered_result, is_rift_marker_line, parse_original_record, LineRange,
    RenderCheck, ResultText, RewriteForm, Rewritten,
};
use rift_shadow::backend::{
    OriginalLeafBytes, ORIGINALS_DIR, ORIGINAL_SUFFIX, TRANSCRIPT_FULL_NAME,
};

use super::artifacts::NativeArtifacts;
use super::verdict::Verdict;

/// The state words a manifest entry may carry.
const STATE_WORDS: [&str; 3] = [
    "original_line_absent",
    "original_spool_corrupt",
    "original_unsealed",
];

/// One manifest entry.
#[derive(Clone)]
pub(super) struct OriginalClaim {
    pub(super) tool_use_id: String,
    sha256: Option<String>,
    bytes: Option<u64>,
    form: Option<String>,
    replaced: Option<Vec<LineRange>>,
    line: Option<usize>,
    rewritten: Option<String>,
    state: Option<String>,
}

impl OriginalClaim {
    /// Whether the entry claims a sealed body.
    fn claims_body(&self) -> bool {
        self.sha256.is_some() || self.bytes.is_some()
    }
}

/// One manifest entry with its body as read back out of the shard.
pub(super) struct OriginalBytes {
    /// The task subtree it sits in, `None` for the session's own transcript.
    pub(super) task: Option<String>,
    pub(super) claim: OriginalClaim,
    /// The stored body; `None` when none is claimed or the file is absent.
    pub(super) body: Option<Vec<u8>>,
}

impl OriginalBytes {
    /// The shard-relative path the body is sealed at.
    fn path(&self) -> String {
        format!(
            "{}{ORIGINALS_DIR}/{}{ORIGINAL_SUFFIX}",
            task_dir(self.task.as_deref()),
            self.claim.tool_use_id
        )
    }
}

/// `tasks/<task>/` for a subagent's transcript, nothing for the session's.
fn task_dir(task: Option<&str>) -> String {
    task.map_or_else(String::new, |t| format!("tasks/{t}/"))
}

/// Every entry of a metadata document's `originals` manifest that names a
/// call.
pub(super) fn claims(doc: Option<&serde_json::Value>) -> Vec<OriginalClaim> {
    doc.and_then(|d| d.get("originals"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let text = |k: &str| {
                entry
                    .get(k)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            let replaced = entry
                .get("replaced")
                .and_then(serde_json::Value::as_array)
                .map(|rs| {
                    rs.iter()
                        .map(LineRange::from_json)
                        .collect::<Option<Vec<_>>>()
                });
            Some(OriginalClaim {
                tool_use_id: text("tool_use_id")?,
                sha256: text("sha256"),
                bytes: entry.get("bytes").and_then(serde_json::Value::as_u64),
                form: text("form"),
                replaced: replaced.flatten(),
                line: entry
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|l| usize::try_from(l).ok()),
                rewritten: text("rewritten"),
                state: text("original"),
            })
        })
        .collect()
}

/// Read every claimed body beside one transcript.
pub(super) fn read(
    reader: &impl SubstrateReader,
    id: &str,
    task: Option<&str>,
    claims: Vec<OriginalClaim>,
) -> Result<Vec<OriginalBytes>> {
    let mut out = Vec::with_capacity(claims.len());
    for claim in claims {
        let mut original = OriginalBytes {
            task: task.map(str::to_owned),
            claim,
            body: None,
        };
        if original.claim.claims_body() {
            original.body = reader.read_artifact(id, &original.path())?;
        }
        out.push(original);
    }
    Ok(out)
}

/// The leaf views of the bodies claimed. A claimed body that is not in the
/// shard reads as empty bytes, which cannot reproduce the writer's leaf.
pub(super) fn leaves(originals: &[OriginalBytes]) -> Vec<OriginalLeafBytes<'_>> {
    originals
        .iter()
        .filter(|o| o.claim.claims_body())
        .map(|o| OriginalLeafBytes {
            task: o.task.as_deref(),
            tool_use_id: &o.claim.tool_use_id,
            body: o.body.as_deref().unwrap_or_default(),
        })
        .collect()
}

/// What the three checks found: findings, and the held ranges left unchecked.
pub(super) struct OriginalFindings {
    pub(super) verdicts: Vec<Verdict>,
    pub(super) held_unchecked: usize,
}

/// All three checks over one shard's originals.
pub(super) fn original_passes(id: &str, native: &NativeArtifacts) -> OriginalFindings {
    let mut found = OriginalFindings {
        verdicts: Vec::new(),
        held_unchecked: 0,
    };
    for original in &native.originals {
        if let Some(v) = digest_finding(id, original) {
            found.verdicts.push(v);
            continue;
        }
        let transcript = transcript_of(native, original.task.as_deref());
        match rendering_finding(id, original, transcript) {
            Ok(held) => found.held_unchecked += held,
            Err(v) => found.verdicts.push(v),
        }
    }
    let mut transcripts = vec![(None, native.transcript_full.as_deref())];
    transcripts.extend(
        native
            .tasks
            .iter()
            .map(|t| (Some(t.tool_use_id.as_str()), t.transcript_full.as_deref())),
    );
    for (task, transcript) in transcripts {
        found
            .verdicts
            .extend(missing_originals(id, native, task, transcript));
    }
    found
}

/// The sealed full transcript of the session (`None`) or of a task.
fn transcript_of<'a>(native: &'a NativeArtifacts, task: Option<&str>) -> Option<&'a [u8]> {
    match task {
        None => native.transcript_full.as_deref(),
        Some(task) => native
            .tasks
            .iter()
            .find(|t| t.tool_use_id == task)
            .and_then(|t| t.transcript_full.as_deref()),
    }
}

/// A claimed body absent, unequal to its digest or size, or an entry whose
/// words are outside their vocabularies.
fn digest_finding(id: &str, o: &OriginalBytes) -> Option<Verdict> {
    let tamper = |why: &str| Some(Verdict::Tamper(format!("{id}: {} {why}", o.path())));
    let form_ok = o
        .claim
        .form
        .as_deref()
        .is_none_or(|f| RewriteForm::from_word(f).is_some());
    let state_ok = o
        .claim
        .state
        .as_deref()
        .is_none_or(|s| STATE_WORDS.contains(&s));
    let rewritten_ok = o
        .claim
        .rewritten
        .as_deref()
        .is_none_or(|r| Rewritten::from_word(r).is_some());
    if !(form_ok && state_ok && rewritten_ok) {
        return tamper("records a word outside its vocabulary");
    }
    if !o.claim.claims_body() {
        return None;
    }
    let Some(body) = &o.body else {
        return tamper("does not match its recorded sha256");
    };
    let digest = rift_shadow::sha256_hex(&[body]);
    let matches = o.claim.sha256.as_deref() == Some(digest.as_str())
        && o.claim.bytes == u64::try_from(body.len()).ok();
    (!matches).then(|| {
        Verdict::Tamper(format!(
            "{id}: {} does not match its recorded sha256",
            o.path()
        ))
    })
}

/// Hold the delivered text on the entry's line to the entry's word: the
/// form's rendering of the original, or the original itself. `Ok` carries the
/// held ranges left unchecked. An entry that names no line, or no word, or is
/// not whole, owes nothing here.
fn rendering_finding(
    id: &str,
    o: &OriginalBytes,
    transcript: Option<&[u8]>,
) -> Result<usize, Verdict> {
    let (Some(line), Some(word), None, Some(body)) = (
        o.claim.line,
        o.claim.rewritten.as_deref(),
        &o.claim.state,
        &o.body,
    ) else {
        return Ok(0);
    };
    let place = format!(
        "{}{TRANSCRIPT_FULL_NAME} line {line}",
        task_dir(o.task.as_deref())
    );
    let tamper = |why: &str| Verdict::Tamper(format!("{id}: {} {why} ({place})", o.path()));
    let delivered = transcript
        .and_then(|t| t.split(|b| *b == b'\n').nth(line))
        .and_then(delivered_result)
        .ok_or_else(|| tamper("names a line that records no result"))?;
    let (stdout, stderr, _) =
        parse_original_record(body).ok_or_else(|| tamper("is not an original record"))?;
    let original = ResultText {
        stdout: &stdout,
        stderr: &stderr,
    };
    let delivered = ResultText {
        stdout: &delivered.0,
        stderr: &delivered.1,
    };
    let form = o.claim.form.as_deref().and_then(RewriteForm::from_word);
    match (Rewritten::from_word(word), form) {
        (Some(Rewritten::SpooledNotDelivered), _) if original == delivered => Ok(0),
        (Some(Rewritten::Delivered(said)), Some(form)) if said == form => {
            let ranges = o.claim.replaced.as_deref().unwrap_or_default();
            match check_rendering(form, original, delivered, ranges) {
                RenderCheck::Matches => Ok(0),
                RenderCheck::HeldUnchecked(n) => Ok(n),
                RenderCheck::Mismatch => Err(tamper("is not what the delivered text renders")),
            }
        }
        _ => Err(tamper("is not what the delivered text renders")),
    }
}

/// Every line of one transcript that delivers a Rift marker without a
/// manifest entry naming that line.
fn missing_originals(
    id: &str,
    native: &NativeArtifacts,
    task: Option<&str>,
    transcript: Option<&[u8]>,
) -> Vec<Verdict> {
    let Some(transcript) = transcript else {
        return Vec::new();
    };
    let named: Vec<usize> = native
        .originals
        .iter()
        .filter(|o| o.task.as_deref() == task)
        .filter_map(|o| o.claim.line)
        .collect();
    transcript
        .split(|b| *b == b'\n')
        .enumerate()
        .filter(|(i, line)| !named.contains(i) && line.windows(6).any(|w| w == b"[rift "))
        .filter(|(_, line)| {
            delivered_result(line).is_some_and(|(out, err)| {
                out.split('\n')
                    .chain(err.split('\n'))
                    .any(is_rift_marker_line)
            })
        })
        .map(|(i, _)| {
            Verdict::Structure(format!(
                "{id}: {}{TRANSCRIPT_FULL_NAME} line {i} delivers a Rift rewrite with no sealed \
                 original (original_missing)",
                task_dir(task)
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests;
