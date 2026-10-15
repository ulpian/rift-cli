use super::*;

fn text<'a>(stdout: &'a str, stderr: &'a str) -> ResultText<'a> {
    ResultText { stdout, stderr }
}

fn out(start: u32, end: u32) -> LineRange {
    LineRange {
        stream: Stream::Stdout,
        start,
        end,
    }
}

fn err(start: u32, end: u32) -> LineRange {
    LineRange {
        stream: Stream::Stderr,
        start,
        end,
    }
}

/// A fold line as a rewriter might word one; the words are the test's own.
fn fold(n: u32) -> String {
    format!("[rift fold: {n} more lines like the one above]")
}

/// Each closed vocabulary round-trips through its words and admits nothing
/// else.
#[test]
fn every_closed_word_round_trips_and_nothing_else_parses() {
    for form in RewriteForm::ALL {
        assert_eq!(RewriteForm::from_word(form.word()), Some(form));
        let rewritten = Rewritten::Delivered(form);
        assert_eq!(Rewritten::from_word(rewritten.word()), Some(rewritten));
    }
    assert_eq!(
        Rewritten::from_word("spooled_not_delivered"),
        Some(Rewritten::SpooledNotDelivered)
    );
    assert_eq!(RewriteForm::from_word("outline"), None);
    assert_eq!(Rewritten::from_word("rewritten"), None);
    for stream in [Stream::Stdout, Stream::Stderr] {
        assert_eq!(Stream::from_word(stream.word()), Some(stream));
    }
    assert_eq!(Stream::from_word("stdin"), None);
    let range = err(1, 3);
    assert_eq!(LineRange::from_json(&range.to_json()), Some(range));
    assert_eq!(
        LineRange::from_json(&serde_json::json!({ "start": 1 })),
        None
    );
}

/// A fold keeps every other line verbatim and in order and puts one fold line
/// where each range was, per stream.
#[test]
fn a_fold_replaces_each_range_with_one_line_and_keeps_the_rest_verbatim() {
    let original = text("a\nb\nc\nd\ne\n", "w1\nw2\nw3");
    let (stdout, stderr) =
        render_folded(original, &[out(1, 3), out(4, 5), err(0, 2)], fold).unwrap();
    assert_eq!(stdout, format!("a\n{}\nd\n{}\n", fold(2), fold(1)));
    assert_eq!(stderr, format!("{}\nw3", fold(2)));
    assert_eq!(
        check_rendering(
            RewriteForm::FoldedOutput,
            original,
            text(&stdout, &stderr),
            &[out(1, 3), out(4, 5), err(0, 2)]
        ),
        RenderCheck::Matches
    );
    let edited = stdout.replace("d\n", "D\n");
    assert_eq!(
        check_rendering(
            RewriteForm::FoldedOutput,
            original,
            text(&edited, &stderr),
            &[out(1, 3), out(4, 5), err(0, 2)]
        ),
        RenderCheck::Mismatch
    );
}

/// Ranges must be present, non-empty, inside their stream and ascending
/// without overlap; a set that does not fit renders nothing.
#[test]
fn ranges_that_do_not_fit_their_original_are_refused() {
    let original = text("a\nb\nc", "");
    assert!(ranges_fit(original, &[out(0, 3)]));
    assert!(ranges_fit(original, &[out(0, 1), out(1, 2), err(0, 1)]));
    for bad in [
        vec![],
        vec![out(0, 4)],
        vec![out(1, 1)],
        vec![out(2, 1)],
        vec![out(1, 3), out(0, 1)],
        vec![out(0, 2), out(1, 3)],
        vec![err(0, 2)],
    ] {
        assert!(!ranges_fit(original, &bad), "{bad:?}");
        assert!(render_folded(original, &bad, fold).is_none());
    }
    let too_many: Vec<LineRange> = (0..=MAX_REPLACED_RANGES)
        .map(|i| out(u32::try_from(i).unwrap(), u32::try_from(i).unwrap() + 1))
        .collect();
    let long = "x\n".repeat(MAX_REPLACED_RANGES + 2);
    assert!(!ranges_fit(text(&long, ""), &too_many));
    assert_eq!(
        check_rendering(RewriteForm::FoldedOutput, original, original, &[]),
        RenderCheck::Mismatch
    );
}

/// A fold line whose words do not read back as its own count is refused at
/// render, so a bad frames text cannot become a fold the checker would then
/// misread; the original goes through unchanged instead.
#[test]
fn a_fold_line_that_does_not_read_back_its_count_is_refused() {
    let original = text("a\nb\nc\nd", "");
    let ranges = [out(1, 3)];
    assert!(render_folded(original, &ranges, fold).is_some());
    let bad_lines: [fn(u32) -> String; 4] = [
        |_| "[rift fold: 9 more lines like the one above]".to_owned(),
        |_| "[rift fold: some lines]".to_owned(),
        |n| format!("folded {n} lines"),
        |n| format!("[rift fold: {n} more lines]\nsecond"),
    ];
    for bad in bad_lines {
        assert!(render_folded(original, &ranges, bad).is_none());
    }
}

/// A held substitution keeps the rest verbatim and puts one held frame per
/// range; its ranges are counted unchecked, never passed.
#[test]
fn a_held_substitution_is_counted_unchecked_and_a_wrong_line_mismatches() {
    let original = text("a\nb\nc\nd", "e");
    let delivered = text("a\n[rift held: src/lib.rs@1a2b lines 2-3]\nd", "e");
    let ranges = [out(1, 3)];
    assert_eq!(
        check_rendering(
            RewriteForm::HeldOutputSubstitution,
            original,
            delivered,
            &ranges
        ),
        RenderCheck::HeldUnchecked(1)
    );
    for wrong in [
        "a\nb\nd",
        "a\n[rift held: x]\nd\nextra",
        "A\n[rift held: x]\nd",
        "a\n[rift held: x]",
    ] {
        assert_eq!(
            check_rendering(
                RewriteForm::HeldOutputSubstitution,
                original,
                text(wrong, "e"),
                &ranges
            ),
            RenderCheck::Mismatch,
            "{wrong}"
        );
    }
}

/// A fold verifies by its shape and its count, whatever its words; a wrong
/// count, a broken opener or closer, or an altered kept line does not.
#[test]
fn a_fold_is_held_to_its_shape_and_count_not_its_words() {
    let original = text("a\nb\nc\nd", "");
    let ranges = [out(1, 3)];
    let check = |delivered: &str| {
        check_rendering(
            RewriteForm::FoldedOutput,
            original,
            text(delivered, ""),
            &ranges,
        )
    };
    for honest in [
        "[rift fold: 2]",
        "[rift fold: 2 lines]",
        "[rift fold: 2 more lines like the one above]",
        "[rift fold: 2 lines [b, c] folded]",
    ] {
        assert_eq!(
            check(&format!("a\n{honest}\nd")),
            RenderCheck::Matches,
            "{honest}"
        );
    }
    for forged in [
        "a\n[rift fold: 3 lines]\nd",
        "a\n[rift fold: 1 lines]\nd",
        "a\n[rift fold:2 lines]\nd",
        "a\n{rift fold: 2 lines]\nd",
        "a\n[rift fold: 2 lines] ok\nd",
        "a\n[rift fold: 2 lines\nd",
        "a\n[rift fold: 2lines]\nd",
        "a\n[rift fold: 2 lines]\nD",
        "A\n[rift fold: 2 lines]\nd",
        "a\n[rift fold: 2 lines]\nd\n",
        "a\nb\nc\nd",
    ] {
        assert_eq!(check(forged), RenderCheck::Mismatch, "{forged}");
    }
}

/// The predicate reads the count out of a whole fold line, and the closer is
/// the line's last character, so a `]` inside the words is the words'.
#[test]
fn the_fold_predicate_reads_the_count_and_only_a_whole_line() {
    assert_eq!(parse_fold_line("[rift fold: 12]"), Some(12));
    assert_eq!(parse_fold_line("[rift fold: 12 lines]"), Some(12));
    assert_eq!(parse_fold_line("[rift fold: 3 lines [x] cut]"), Some(3));
    assert_eq!(parse_fold_line("[rift fold: 3 ]"), Some(3));
    for not in [
        "[rift fold: 3 lines] cut",
        "[rift fold: 3 lines]\n",
        "[rift fold: 3\nlines]",
        "[rift fold: ]",
        "[rift fold:  3 lines]",
        "[rift fold: +3 lines]",
        "[rift fold: 4294967296 lines]",
        "[Rift fold: 3 lines]",
    ] {
        assert_eq!(parse_fold_line(not), None, "{not:?}");
    }
}

proptest::proptest! {
    /// Any words without a newline round-trip through the predicate with the
    /// count preserved.
    #[test]
    fn any_words_without_a_newline_keep_their_count(n in 1u32.., words in "[^\n]*") {
        let line = format!("{FOLD_OPEN}{n} {words}]");
        proptest::prop_assert_eq!(parse_fold_line(&line), Some(n));
    }
}

/// Only a whole line in the grammar is a marker.
#[test]
fn only_a_whole_marker_line_is_a_marker() {
    assert!(is_rift_marker_line("[rift fold: 12 lines]"));
    assert!(is_rift_marker_line("[rift held: a@b]"));
    for not in [
        "[rift fold: 0 lines]",
        "[rift fold: 012 lines]",
        "[rift fold: x lines]",
        " [rift fold: 2 lines]",
        "\"[rift fold: 2 lines]\"",
        "[rift held: ]",
        "[rift held: x",
        "plain",
    ] {
        assert!(!is_rift_marker_line(not), "{not}");
    }
}

/// The delivered text is read from the client's streams first, else from the
/// one result block's text; the judge says spooled_not_delivered only when the
/// client recorded the original itself.
#[test]
fn the_delivered_text_is_read_from_the_line_and_judged_against_the_original() {
    let line = br#"{"toolUseResult":{"stdout":"o","stderr":"e","interrupted":false},"message":{"content":[]}}"#;
    assert_eq!(delivered_result(line), Some(("o".into(), "e".into())));
    let block = br#"{"message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"t"}]}]}}"#;
    assert_eq!(delivered_result(block), Some(("t".into(), String::new())));
    let two = br#"{"message":{"content":[{"type":"tool_result","content":"a"},{"type":"tool_result","content":"b"}]}}"#;
    assert_eq!(delivered_result(two), None);
    assert_eq!(delivered_result(b"not json"), None);
    let original = text("a", "");
    assert_eq!(
        Rewritten::judge(RewriteForm::FoldedOutput, original, original),
        Rewritten::SpooledNotDelivered
    );
    assert_eq!(
        Rewritten::judge(RewriteForm::FoldedOutput, original, text("b", "")),
        Rewritten::Delivered(RewriteForm::FoldedOutput)
    );
}

/// The original's canonical bytes have sorted keys and parse back; anything
/// else is not an original.
#[test]
fn an_original_record_is_canonical_and_parses_back() {
    let body = original_record_json("out\n", "err", true);
    assert_eq!(
        body,
        br#"{"interrupted":true,"stderr":"err","stdout":"out\n"}"#
    );
    assert_eq!(
        parse_original_record(&body),
        Some(("out\n".into(), "err".into(), true))
    );
    assert_eq!(parse_original_record(br#"{"stdout":"a"}"#), None);
    assert_eq!(
        parse_original_record(br#"{"interrupted":true,"stderr":"","stdout":"","x":1}"#),
        None
    );
}

#[test]
fn a_stateful_fold_callback_cannot_emit_unchecked_text() {
    let original = text("a\nb\nc\nd", "");
    let ranges = [out(1, 3)];
    let calls = std::cell::Cell::new(0);
    let (stdout, stderr) = render_folded(original, &ranges, |n| {
        let call = calls.get();
        calls.set(call + 1);
        if call == 0 {
            fold(n)
        } else {
            "unchecked output\nsecond line".to_owned()
        }
    })
    .unwrap();
    assert_eq!(
        check_rendering(
            RewriteForm::FoldedOutput,
            original,
            text(&stdout, &stderr),
            &ranges
        ),
        RenderCheck::Matches,
        "the emitted fold must be the bytes that passed validation: {stdout}"
    );
    assert_eq!(calls.get(), ranges.len());
}

#[test]
fn validated_markers_keep_range_order_across_both_streams() {
    let original = text("a\nb\nc\n", "w\nx\ny\n");
    let ranges = [err(1, 2), out(0, 2), err(2, 3)];
    let calls = std::cell::Cell::new(0);
    let (stdout, stderr) = render_folded(original, &ranges, |n| {
        calls.set(calls.get() + 1);
        format!("[rift fold: {n} marker {}]", calls.get())
    })
    .unwrap();
    assert_eq!(calls.get(), ranges.len());
    assert_eq!(stdout, "[rift fold: 2 marker 2]\nc\n");
    assert_eq!(
        stderr,
        "w\n[rift fold: 1 marker 1]\n[rift fold: 1 marker 3]\n"
    );
    assert_eq!(
        check_rendering(
            RewriteForm::FoldedOutput,
            original,
            text(&stdout, &stderr),
            &ranges
        ),
        RenderCheck::Matches
    );
}
