use std::collections::BTreeMap;

use super::*;

const META: &str = r#"{"transcript_segments":2,"segment_raw_lines":[1,1]}"#;

fn store(entries: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.as_bytes().to_vec()))
        .collect()
}

fn read(map: &BTreeMap<String, Vec<u8>>, rel: &str) -> Option<Vec<u8>> {
    resolve_transcript(rel, |name| Ok(map.get(name).cloned())).unwrap()
}

#[test]
fn a_whole_transcript_and_every_other_name_pass_straight_through() {
    let map = store(&[
        ("metadata.json", "{}"),
        ("transcript.jsonl", "w\n"),
        ("full.jsonl", "f\n"),
        ("x_transcript.jsonl", "x\n"),
    ]);
    assert_eq!(read(&map, "transcript.jsonl").unwrap(), b"w\n");
    assert_eq!(read(&map, "full.jsonl").unwrap(), b"f\n");
    assert_eq!(read(&map, "x_transcript.jsonl").unwrap(), b"x\n");
    assert_eq!(read(&map, "tasks/t/transcript.jsonl"), None);
}

#[test]
fn declared_parts_read_back_as_their_concatenation_for_the_parent_and_a_task() {
    for dir in ["", "tasks/toolu_1/"] {
        let (meta, a, b) = (
            format!("{dir}metadata.json"),
            format!("{dir}transcript.segments/000000"),
            format!("{dir}transcript.segments/000001"),
        );
        let map = store(&[(&meta, META), (&a, "a\n"), (&b, "b\n")]);
        let rel = format!("{dir}transcript.jsonl");
        assert_eq!(read(&map, &rel).unwrap(), b"a\nb\n", "{dir}");
    }
}

#[test]
fn a_missing_part_a_bad_declaration_or_two_layouts_read_as_absent_never_partial() {
    let missing = store(&[
        ("metadata.json", META),
        ("transcript.segments/000000", "a\n"),
    ]);
    assert_eq!(read(&missing, "transcript.jsonl"), None, "missing part");

    let both = store(&[
        ("metadata.json", META),
        ("transcript.jsonl", "a\nb\n"),
        ("transcript.segments/000000", "a\n"),
        ("transcript.segments/000001", "b\n"),
    ]);
    assert_eq!(read(&both, "transcript.jsonl"), None, "two layouts");

    for bad in [
        r#"{"transcript_segments":2}"#,
        r#"{"transcript_segments":2,"segment_raw_lines":[1]}"#,
        r#"{"transcript_segments":0,"segment_raw_lines":[]}"#,
        r#"{"transcript_segments":"2","segment_raw_lines":[1,1]}"#,
    ] {
        let map = store(&[("metadata.json", bad), ("transcript.jsonl", "w\n")]);
        assert_eq!(read(&map, "transcript.jsonl"), None, "{bad}");
    }
}

#[test]
fn a_declaration_reads_as_its_line_counts() {
    let doc: Value = serde_json::from_str(META).unwrap();
    assert_eq!(declared_layout(&doc), Declared::Segments(vec![1, 1]));
    assert_eq!(declared_layout(&Value::Null), Declared::Whole);
    assert_eq!(segment_name(7), "transcript.segments/000007");
}
