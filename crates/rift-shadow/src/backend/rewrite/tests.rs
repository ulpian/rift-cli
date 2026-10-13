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
    let (stdout, stderr) = render_folded(original, &[out(1, 3), out(4, 5), err(0, 2)]).unwrap();
    assert_eq!(stdout, "a\n[rift fold: 2 lines]\nd\n[rift fold: 1 lines]\n");
    assert_eq!(stderr, "[rift fold: 2 lines]\nw3");
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
        assert!(render_folded(original, &bad).is_none());
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
