//! Streaming JSONL helpers for `full.jsonl` / `tool_calls.jsonl`.
//!
//! JSONL is one JSON value per line. We read lazily so large transcripts never
//! have to be fully buffered as parsed values.

use std::io::{BufRead, BufReader, Read};

/// An error encountered while reading a JSONL stream.
#[derive(Debug, thiserror::Error)]
pub enum JsonlError {
    #[error("io error reading JSONL: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON on line {line}: {source}")]
    Parse {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// Iterate over the parsed JSON values in a JSONL reader.
///
/// Blank lines are skipped. The first malformed line yields an `Err` and ends
/// iteration; callers that want best-effort parsing can simply `.flatten()`.
pub fn read_values<R: Read>(
    reader: R,
) -> impl Iterator<Item = Result<serde_json::Value, JsonlError>> {
    BufReader::new(reader)
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line = match line {
                Ok(l) => l,
                Err(e) => return Some(Err(JsonlError::Io(e))),
            };
            if line.trim().is_empty() {
                return None;
            }
            Some(
                serde_json::from_str(&line).map_err(|source| JsonlError::Parse {
                    line: idx + 1,
                    source,
                }),
            )
        })
}

/// Count the number of JSON records in a JSONL stream (skipping blank lines).
pub fn count_records<R: Read>(reader: R) -> Result<usize, JsonlError> {
    let mut count = 0;
    for value in read_values(reader) {
        value?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_line_as_a_value() {
        let data = "{\"a\":1}\n\n{\"b\":2}\n";
        let values: Vec<_> = read_values(data.as_bytes()).map(Result::unwrap).collect();
        assert_eq!(values.len(), 2);
        assert_eq!(values[1]["b"], 2);
    }

    #[test]
    fn counts_records_ignoring_blanks() {
        let data = "{}\n{}\n\n{}\n";
        assert_eq!(count_records(data.as_bytes()).unwrap(), 3);
    }

    #[test]
    fn surfaces_parse_errors_with_line_numbers() {
        let data = "{}\nnot json\n";
        let err = read_values(data.as_bytes()).nth(1).unwrap().unwrap_err();
        assert!(matches!(err, JsonlError::Parse { line: 2, .. }));
    }
}
