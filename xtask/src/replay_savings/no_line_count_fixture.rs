//! The "no name or line count" split, pinned over a corpus of files no
//! commit and no checkout holds, so every read of them lands in the bucket.

use serde_json::{json, Value};

use super::bucket::Bucket;
use super::no_line_count_split::{delivered, Delivered, Standing, WorkingTree};
use super::render::{render_json, render_table};
use super::replay::replay;
use super::tests::fixture;
use super::transcript::Call;

fn tool_use(id: &str, name: &str, input: &Value) -> Value {
    json!({"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": id, "name": name, "input": input}
    ]}})
}

fn tool_result(id: &str, content: &str) -> Value {
    json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "content": content}
    ]}})
}

/// The lines of a scratch file, with `third` standing in for line 3.
fn body(lines: usize, third: &str) -> Vec<String> {
    (1..=lines)
        .map(|i| match i {
            3 => third.to_owned(),
            _ => format!("line {i} of plain prose text"),
        })
        .collect()
}

/// The Read tool's numbered rendering of lines `from..` of a body.
fn numbered(body: &[String], from: usize) -> String {
    body.iter()
        .enumerate()
        .skip(from - 1)
        .map(|(at, line)| format!("{:>6}\u{2192}{line}", at + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A read and its result, as two records.
fn read(id: &str, input: &Value, result: &str) -> [Value; 2] {
    [tool_use(id, "Read", input), tool_result(id, result)]
}

/// The walk: a whole file read, re-read, edited, re-read twice, and moved by
/// another hand; a range of a second file read twice; a third file written,
/// edited and read.
fn records() -> Vec<Value> {
    let a = json!({"file_path": "/repo/scratch/a.md"});
    let b = json!({"file_path": "/repo/scratch/b.md", "offset": 5, "limit": 5});
    let c = "/repo/scratch/c.md";
    let plain = body(30, "line 3 of plain prose text");
    let edited = body(30, "line three of plain prose text");
    let moved = body(30, "line 3, moved by another hand");
    let mut out = vec![json!({"type": "user", "message": {"role": "user", "content": "go"}})];
    out.extend(read("r1", &a, &numbered(&plain, 1)));
    out.extend(read("r2", &a, &numbered(&plain, 1)));
    out.push(tool_use(
        "e1",
        "Edit",
        &json!({"file_path": "/repo/scratch/a.md",
                "old_string": "line 3 of plain prose text",
                "new_string": "line three of plain prose text"}),
    ));
    out.push(tool_result("e1", "The file has been updated."));
    out.extend(read("r3", &a, &numbered(&edited, 1)));
    out.extend(read("r4", &a, &numbered(&edited, 1)));
    out.extend(read("r5", &a, &numbered(&moved, 1)));
    out.extend(read("r6", &b, &numbered(&plain[..9], 5)));
    out.extend(read("r7", &b, &numbered(&plain[..9], 5)));
    let written = body(10, "line 3 of plain prose text").join("\n") + "\n";
    out.push(tool_use(
        "w1",
        "Write",
        &json!({"file_path": c, "content": written}),
    ));
    out.push(tool_result("w1", "File created."));
    out.push(tool_use(
        "e2",
        "Edit",
        &json!({"file_path": c, "old_string": "line 2 of", "new_string": "line two of"}),
    ));
    out.push(tool_result("e2", "The file has been updated."));
    out.extend(read(
        "r8",
        &json!({"file_path": c}),
        &numbered(&body(10, "line 3 of plain prose text"), 1),
    ));
    out
}

/// Write one transcript of `records` under `dir`, stamped a second apart.
fn write_transcript(dir: &std::path::Path, records: &[Value]) {
    std::fs::create_dir_all(dir).unwrap();
    let mut out = String::new();
    for (second, record) in records.iter().enumerate() {
        let mut record = record.clone();
        record["cwd"] = json!("/repo");
        record["timestamp"] = json!(format!("2026-03-01T00:00:{second:02}.000Z"));
        out.push_str(&record.to_string());
        out.push('\n');
    }
    std::fs::write(dir.join("sess-scratch.jsonl"), out).unwrap();
}

/// ★ Every call of the bucket lands in exactly one class: the re-read of
/// bytes a read delivered and nothing changed is declinable; one after the
/// window's own edit, or after another hand moved the file, is a change; a
/// range never joined to a whole copy has no bytes; and the first read of
/// each path is a first sight.
#[test]
fn the_bucket_is_split_by_what_the_transcript_proves() {
    let tmp = tempfile::tempdir().unwrap();
    let transcripts = tmp.path().join("transcripts");
    write_transcript(&transcripts, &records());
    let mut opts = fixture(tmp.path());
    opts.transcripts = transcripts;
    let report = replay(&opts).unwrap();

    let doc: Value = serde_json::from_str(&render_json(&report)).unwrap();
    let split = &doc["no_line_count_split"];
    let calls = |class: &str| split[class]["calls"].as_u64().unwrap();
    assert_eq!(calls("known_unchanged"), 2, "r2 and r4: {split}");
    assert_eq!(calls("known_changed"), 3, "r3, r5 and r8: {split}");
    assert_eq!(calls("unknown_reread"), 1, "r7: {split}");
    assert_eq!(calls("unknown_first_sight"), 2, "r1 and r6: {split}");

    let bucket = report.bucket(Bucket::NoLineCount);
    let classes = [
        "known_unchanged",
        "known_changed",
        "unknown_reread",
        "unknown_first_sight",
    ];
    let sum = |field: &str| -> u64 {
        classes
            .iter()
            .map(|class| split[*class][field].as_u64().unwrap())
            .sum()
    };
    assert_eq!(
        sum("calls"),
        bucket.calls,
        "the classes add up to the bucket"
    );
    assert_eq!(sum("tokens"), bucket.tokens, "token for token");
    assert_eq!(
        split["known_unchanged"]["bands"]["le_16_kib"]["calls"], 2,
        "small files, in the smallest band: {split}"
    );
    let table = render_table(&report);
    assert!(
        table.contains("known, unchanged at re-read (declinable)"),
        "{table}"
    );
}

fn call(name: &str, input: &Value, result: &str) -> Call {
    Call {
        name: name.to_owned(),
        id: "c".to_owned(),
        input: input.clone(),
        cwd: Some("/repo".to_owned()),
        result: Some(result.to_owned()),
        response: None,
        prompt_id: None,
    }
}

/// A shell read is matched line by line, a `cat -n` number set aside, and a
/// line the known bytes do not hold is a change.
#[test]
fn a_shell_read_agrees_line_by_line_with_its_numbers_set_aside() {
    let mut tree = WorkingTree::default();
    let text = "alpha line\nbravo line\ncharlie line\n";
    let write = call(
        "Write",
        &json!({"file_path": "/repo/x.txt", "content": text}),
        "",
    );
    tree.note_change(&write, "x.txt");
    let shell = |out: &str| call("Bash", &json!({"command": "cat -n x.txt"}), out);
    let numbered = "     1\talpha line\n     2\tbravo line\n";
    let same = shell(numbered);
    let delivered_same = delivered(&same, numbered);
    assert_eq!(
        tree.standing(&same, "x.txt", &delivered_same, false).0,
        Standing::Unchanged
    );
    let other = shell("alpha line\ndelta line\n");
    let delivered_other = delivered(&other, "alpha line\ndelta line\n");
    assert_eq!(
        tree.standing(&other, "x.txt", &delivered_other, false),
        (Standing::Changed, Some(u64::try_from(text.len()).unwrap()))
    );
}

/// A numbered range joins known bytes in place; one that starts past their
/// end leaves the path seen with no bytes; an edit whose old text is absent
/// does the same.
#[test]
fn a_range_joins_known_bytes_and_an_edit_that_misses_forgets_them() {
    let mut tree = WorkingTree::default();
    let read = |input: Value, out: &str| call("Read", &input, out);
    let whole = read(
        json!({"file_path": "/repo/y.md"}),
        "     1\u{2192}one\n     2\u{2192}two",
    );
    tree.note_delivery(
        &whole,
        "y.md",
        delivered(&whole, "     1\u{2192}one\n     2\u{2192}two"),
        true,
    );
    let range_text = "     2\u{2192}TWO\n     3\u{2192}three";
    let range = read(json!({"file_path": "/repo/y.md", "offset": 2}), range_text);
    tree.note_delivery(&range, "y.md", delivered(&range, range_text), false);
    let again = "     1\u{2192}one\n     2\u{2192}TWO\n     3\u{2192}three";
    let check = read(json!({"file_path": "/repo/y.md"}), again);
    assert_eq!(
        tree.standing(&check, "y.md", &delivered(&check, again), true)
            .0,
        Standing::Unchanged,
        "the joined bytes are the file"
    );
    let past = "     9\u{2192}nine";
    let far = read(json!({"file_path": "/repo/y.md", "offset": 9}), past);
    tree.note_delivery(&far, "y.md", delivered(&far, past), false);
    assert_eq!(
        tree.standing(&check, "y.md", &delivered(&check, again), true)
            .0,
        Standing::Reread
    );
    let Delivered::Numbered(lines) = delivered(&check, again) else {
        panic!("a Read result is numbered");
    };
    assert_eq!(lines.len(), 3);
    tree.note_delivery(&check, "y.md", delivered(&check, again), true);
    let missed = call(
        "Edit",
        &json!({"file_path": "/repo/y.md", "old_string": "absent", "new_string": "x"}),
        "ok",
    );
    tree.note_change(&missed, "y.md");
    assert_eq!(
        tree.standing(&check, "y.md", &delivered(&check, again), true)
            .0,
        Standing::Reread
    );
}

/// Replay one transcript of `records` and read the document back.
fn replayed(records: &[Value]) -> (super::report::Report, Value) {
    let tmp = tempfile::tempdir().unwrap();
    let transcripts = tmp.path().join("transcripts");
    write_transcript(&transcripts, records);
    let mut opts = fixture(tmp.path());
    opts.transcripts = transcripts;
    let report = replay(&opts).unwrap();
    let doc: Value = serde_json::from_str(&render_json(&report)).unwrap();
    (report, doc)
}

/// A prompt, then each file read in order, whole.
fn whole_reads(file: &str, bodies: &[&[String]]) -> Vec<Value> {
    let mut out = vec![json!({"type": "user", "message": {"role": "user", "content": "go"}})];
    for (at, body) in bodies.iter().enumerate() {
        out.extend(read(
            &format!("r{at}"),
            &json!({"file_path": file}),
            &numbered(body, 1),
        ));
    }
    out
}

/// The credit the gate model gave, and each reason it withheld the rest.
fn uncommitted(doc: &Value, key: &str) -> u64 {
    doc["no_line_count_split"]["uncommitted_declines"][key]["calls"]
        .as_u64()
        .unwrap()
}

/// ★ An unchanged re-read of a file no commit holds is declined, as the gate
/// declines it, and pays its reason frame.
#[test]
fn an_unchanged_uncommitted_reread_is_declined() {
    let plain = body(30, "line 3 of plain prose text");
    let (report, doc) = replayed(&whole_reads("/repo/scratch/a.md", &[&plain, &plain]));
    assert_eq!(report.declined.calls, 1, "the second read: {doc}");
    assert_eq!(report.declinable.calls, 1);
    assert_eq!(uncommitted(&doc, "credited"), 1);
    assert!(report.frame_tokens > 0, "the decline pays its frame");
    assert_eq!(
        report.bucket(Bucket::NoLineCount).calls,
        2,
        "the bucket and its split are as they were"
    );
}

/// A re-read after the bytes moved is a delta, not a held read: no credit.
#[test]
fn a_changed_uncommitted_reread_is_not_declined() {
    let plain = body(30, "line 3 of plain prose text");
    let moved = body(30, "line 3, moved by another hand");
    let (report, doc) = replayed(&whole_reads("/repo/scratch/a.md", &[&plain, &moved]));
    assert_eq!(report.declined.calls, 0, "{doc}");
    assert_eq!(uncommitted(&doc, "credited"), 0);
}

/// A re-read of a range never joined to a whole copy has no bytes to compare
/// against: no credit.
#[test]
fn a_reread_with_no_bytes_in_the_transcript_is_not_declined() {
    let plain = body(40, "line 3 of plain prose text");
    let input = json!({"file_path": "/repo/scratch/a.md", "offset": 1, "limit": 35});
    let mut records = vec![json!({"type": "user", "message": {"role": "user", "content": "go"}})];
    records.extend(read("r0", &input, &numbered(&plain[..35], 1)));
    records.extend(read("r1", &input, &numbered(&plain[..35], 1)));
    let (report, doc) = replayed(&records);
    assert_eq!(report.declined.calls, 0, "{doc}");
    assert_eq!(doc["no_line_count_split"]["unknown_reread"]["calls"], 1);
}

/// The gate declines a read once per key: the third identical read runs, and
/// is not counted as a second decline.
#[test]
fn a_second_decline_on_the_same_key_is_not_counted() {
    let plain = body(30, "line 3 of plain prose text");
    let (report, doc) = replayed(&whole_reads(
        "/repo/scratch/a.md",
        &[&plain, &plain, &plain],
    ));
    assert_eq!(report.declined.calls, 1, "{doc}");
    assert_eq!(uncommitted(&doc, "credited"), 1);
    assert_eq!(uncommitted(&doc, "key_already_declined"), 1);
}

/// A file past the inline-hash cap is never hashed by the gate, so it is
/// never held.
#[test]
fn a_file_over_the_hashing_cap_is_not_credited() {
    let wide: Vec<String> = (1..=1990)
        .map(|i| format!("{i} {}", "x".repeat(600)))
        .collect();
    let (report, doc) = replayed(&whole_reads("/repo/scratch/wide.md", &[&wide, &wide]));
    assert_eq!(report.declined.calls, 0, "{doc}");
    assert_eq!(uncommitted(&doc, "over_hash_cap"), 1);
}

/// What a compaction emptied is no longer held, though the bytes are known.
#[test]
fn a_reread_after_a_compaction_is_not_held() {
    let plain = body(30, "line 3 of plain prose text");
    let mut records = whole_reads("/repo/scratch/a.md", &[&plain]);
    records.push(json!({"type": "system", "subtype": "compact_boundary"}));
    records.extend(read(
        "r9",
        &json!({"file_path": "/repo/scratch/a.md"}),
        &numbered(&plain, 1),
    ));
    let (report, doc) = replayed(&records);
    assert_eq!(report.declined.calls, 0, "{doc}");
    assert_eq!(uncommitted(&doc, "emptied_by_compaction"), 1);
}
