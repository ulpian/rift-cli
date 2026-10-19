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

const COMPACT_META: &str = r#"{"transcript_compact_segments":2}"#;

#[test]
fn a_record_re_emitted_by_a_later_compact_part_supersedes_at_its_first_position() {
    let first = br#"{"id":"m1","t":"a"}
{"role":"user"}
"#;
    let second = br#"{"id":"m1","t":"ab"}
{"id":"m2","t":"c"}
"#;
    let joined = join_compact_segments(&[first.to_vec(), second.to_vec()]);
    let expected = br#"{"id":"m1","t":"ab"}
{"role":"user"}
{"id":"m2","t":"c"}
"#;
    assert_eq!(joined, expected.to_vec());
}

#[test]
fn declared_compact_parts_read_back_as_their_join_for_the_parent_and_a_task() {
    for dir in ["", "tasks/toolu_1/"] {
        let (meta, a, b) = (
            format!("{dir}metadata.json"),
            format!("{dir}transcript.compact.segments/000000"),
            format!("{dir}transcript.compact.segments/000001"),
        );
        let map = store(&[
            (&meta, COMPACT_META),
            (&a, "{\"id\":\"m\",\"t\":1}\n"),
            (&b, "{\"id\":\"m\",\"t\":2}\n"),
        ]);
        let rel = format!("{dir}transcript.compact.jsonl");
        assert_eq!(
            read(&map, &rel).unwrap(),
            b"{\"id\":\"m\",\"t\":2}\n",
            "{dir}"
        );
    }
}

#[test]
fn a_compact_part_missing_or_beside_a_whole_compact_reads_as_absent() {
    let missing = store(&[
        ("metadata.json", COMPACT_META),
        ("transcript.compact.segments/000000", "a\n"),
    ]);
    assert_eq!(read(&missing, "transcript.compact.jsonl"), None);
    let both = store(&[
        ("metadata.json", r#"{"transcript_compact_segments":1}"#),
        ("transcript.compact.segments/000000", "a\n"),
        ("transcript.compact.jsonl", "w\n"),
    ]);
    assert_eq!(read(&both, "transcript.compact.jsonl"), None);
    let bad = store(&[
        ("metadata.json", r#"{"transcript_compact_segments":0}"#),
        ("transcript.compact.jsonl", "w\n"),
    ]);
    assert_eq!(read(&bad, "transcript.compact.jsonl"), None);
    let whole = store(&[("metadata.json", "{}"), ("transcript.compact.jsonl", "w\n")]);
    assert_eq!(read(&whole, "transcript.compact.jsonl").unwrap(), b"w\n");
}
