//! The grammar of one spooled original: the record a hook writes before it
//! delivers a rewrite, and the archive reads back to seal.
//!
//! One file per rewritten call. Line one is a header naming the call, the
//! window and family that rewrote it, the form, the replaced ranges, and the
//! SHA-256 and size of the body; the rest of the file is the body — the
//! original's canonical record ([`super::rewrite::original_record_json`])
//! already through the redaction gate. One grammar for the writer and the
//! reader, here, so the two cannot drift; the writer takes the body as
//! [`RedactedBytes`], so raw tool output cannot be spooled.

use rift_core::RedactedBytes;

use super::rewrite::{LineRange, RewriteForm};

/// The header's version.
pub const SPOOL_RECORD_VERSION: u64 = 1;

/// A spool record's header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpoolHeader {
    /// The rewritten call.
    pub tool_use_id: String,
    /// The subagent whose window rewrote it, `None` for the session's own.
    pub agent_id: Option<String>,
    /// The window journal key of the window that rewrote it.
    pub window_key: String,
    /// The repository family the session belongs to.
    pub family_key: String,
    /// The rewrite's form.
    pub form: RewriteForm,
    /// The original's lines the rewrite replaced.
    pub replaced: Vec<LineRange>,
    /// The SHA-256 (lowercase hex) of the body.
    pub sha256: String,
    /// The body's length in bytes.
    pub bytes: u64,
}

impl SpoolHeader {
    /// The whole record: this header, a line break, then the body.
    #[must_use]
    pub fn record(&self, body: &RedactedBytes) -> Vec<u8> {
        let mut out = serde_json::to_vec(&self.to_json()).unwrap_or_default();
        out.push(b'\n');
        out.extend_from_slice(body.as_slice());
        out
    }

    fn to_json(&self) -> serde_json::Value {
        let ranges: Vec<serde_json::Value> = self.replaced.iter().map(|r| r.to_json()).collect();
        let mut obj = serde_json::Map::new();
        obj.insert("v".into(), SPOOL_RECORD_VERSION.into());
        obj.insert("tool_use_id".into(), self.tool_use_id.clone().into());
        if let Some(agent) = &self.agent_id {
            obj.insert("agent_id".into(), agent.clone().into());
        }
        obj.insert("window_key".into(), self.window_key.clone().into());
        obj.insert("family_key".into(), self.family_key.clone().into());
        obj.insert("form".into(), self.form.word().into());
        obj.insert("replaced".into(), ranges.into());
        obj.insert("sha256".into(), self.sha256.clone().into());
        obj.insert("bytes".into(), self.bytes.into());
        serde_json::Value::Object(obj)
    }

    fn from_json(v: &serde_json::Value) -> Option<Self> {
        if v.get("v")?.as_u64()? != SPOOL_RECORD_VERSION {
            return None;
        }
        let text = |k: &str| {
            v.get(k)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let replaced = v
            .get("replaced")?
            .as_array()?
            .iter()
            .map(LineRange::from_json)
            .collect::<Option<Vec<_>>>()?;
        let agent_id = match v.get("agent_id") {
            None => None,
            Some(agent) => Some(agent.as_str()?.to_owned()),
        };
        Some(Self {
            tool_use_id: text("tool_use_id")?,
            agent_id,
            window_key: text("window_key")?,
            family_key: text("family_key")?,
            form: RewriteForm::from_word(v.get("form")?.as_str()?)?,
            replaced,
            sha256: text("sha256")?,
            bytes: v.get("bytes")?.as_u64()?,
        })
    }
}

/// A record split into its header, when the first line is one in this
/// grammar, and its body — everything after the first line break, or the whole
/// file when there is none.
#[must_use]
pub fn parse_spool_record(record: &[u8]) -> (Option<SpoolHeader>, &[u8]) {
    let Some(at) = record.iter().position(|b| *b == b'\n') else {
        return (None, record);
    };
    let (line, rest) = record.split_at(at);
    let body = rest.get(1..).unwrap_or_default();
    let header = serde_json::from_slice::<serde_json::Value>(line)
        .ok()
        .and_then(|v| SpoolHeader::from_json(&v));
    (header, body)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::redacted_of;
    use super::super::rewrite::Stream;
    use super::*;

    fn header() -> SpoolHeader {
        SpoolHeader {
            tool_use_id: "toolu_a".into(),
            agent_id: Some("a1".into()),
            window_key: "k".repeat(64),
            family_key: "fam".into(),
            form: RewriteForm::FoldedOutput,
            replaced: vec![LineRange {
                stream: Stream::Stderr,
                start: 1,
                end: 4,
            }],
            sha256: "s".into(),
            bytes: 2,
        }
    }

    /// A record reads back as the header and body it was written from, and a
    /// first line outside the grammar leaves the body readable.
    #[test]
    fn a_record_reads_back_as_written_and_a_bad_header_keeps_its_body() {
        let body = redacted_of(b"{}\n");
        let record = header().record(&body);
        let (parsed, read) = parse_spool_record(&record);
        assert_eq!(parsed, Some(header()));
        assert_eq!(read, b"{}\n");
        let own = SpoolHeader {
            agent_id: None,
            ..header()
        };
        assert_eq!(parse_spool_record(&own.record(&body)).0, Some(own));
        for bad in [
            &b"{\"v\":2}\nbody"[..],
            b"not json\nbody",
            b"{\"v\":1,\"agent_id\":3}\nbody",
        ] {
            assert_eq!(parse_spool_record(bad), (None, &b"body"[..]));
        }
        assert_eq!(parse_spool_record(b"no line"), (None, &b"no line"[..]));
    }
}
