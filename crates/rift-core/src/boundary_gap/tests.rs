use super::*;

const VECTOR: &str = r#"{"batch":{"evidence_sha256":"2222222222222222222222222222222222222222222222222222222222222222","gap_id":"41ee68ff6bec15553e5a399a27b56a7cca839cd024ecd21a0469c341cd21d9b0","member_key":"1111111111111111111111111111111111111111111111111111111111111111","reason":"uncaptured-backlog","recorded_at":1500,"repo_id":"repo-1","schema_version":1,"selection_sha256":"7befeea2bbcd241de610e8f572200e84b5d1adb5cdffe828a529750e323e2f5a","ticket_count":1,"tickets":[{"attempts":2,"boundary":"end","captured_at":1000,"context_sha256":"4444444444444444444444444444444444444444444444444444444444444444","head_oid":"7777777777777777777777777777777777777777","queued_at":1200,"schema_version":1,"session_key":"6666666666666666666666666666666666666666666666666666666666666666","source_key":"3333333333333333333333333333333333333333333333333333333333333333","ticket_sha256":"5555555555555555555555555555555555555555555555555555555555555555","turn_ordinal":7}]},"carrier_session_key":"8888888888888888888888888888888888888888888888888888888888888888","schema_version":1}"#;

fn vector() -> GapEnvelope {
    decode_checked(format!("{VECTOR}\n").as_bytes())
        .unwrap()
        .remove(0)
}

#[test]
fn canonical_vectors_pin_identity_preimages_and_envelope_bytes() {
    let envelope = vector();
    assert_eq!(
        envelope.batch().selection_digest().to_hex(),
        "7befeea2bbcd241de610e8f572200e84b5d1adb5cdffe828a529750e323e2f5a"
    );
    assert_eq!(
        envelope.batch().gap_id().to_hex(),
        "41ee68ff6bec15553e5a399a27b56a7cca839cd024ecd21a0469c341cd21d9b0"
    );
    assert_eq!(
        hash(
            b"",
            &encode_canonical(std::slice::from_ref(&envelope)).unwrap()
        )
        .to_hex(),
        "b2bec9d9836cb4c3dec8d8670fa681a3cf02b1a9658452f7844d5c6c04ca3c7c"
    );
    assert_eq!(
        encode_canonical(&[envelope]).unwrap(),
        format!("{VECTOR}\n").as_bytes()
    );
}

#[test]
fn rejected_fields_never_escape_through_error_display_debug_or_source() {
    use std::error::Error as _;
    for input in [
        format!(
            "{{\"private-sentinel\":\"secret-sentinel\",{}\n",
            &VECTOR[1..]
        ),
        VECTOR.replacen(
            "\"source_key\":",
            "\"private-sentinel\":1,\"source_key\":",
            1,
        ) + "\n",
        VECTOR.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":1",
            1,
        ) + "\n",
    ] {
        let error = decode_checked(input.as_bytes()).unwrap_err();
        assert!(!error.to_string().contains("sentinel"));
        assert!(!format!("{error:?}").contains("sentinel"));
        assert!(error.source().is_none());
    }
}

#[test]
fn carrier_is_separate_from_batch_subjects_and_identity() {
    let a = vector();
    let b = GapEnvelope::new(
        SessionKey::parse_hex(&"99".repeat(32)).unwrap(),
        a.batch().clone(),
    );
    assert_eq!(a.batch(), b.batch());
    assert_ne!(a.carrier_session_key(), b.carrier_session_key());
    assert_ne!(
        encode_canonical(&[a]).unwrap(),
        encode_canonical(&[b]).unwrap()
    );
}

#[test]
fn absent_artifact_is_not_an_empty_present_receipt() {
    assert!(decode_checked(b"").is_err());
    assert!(encode_canonical(&[]).is_err());
    assert!(decode_checked(&vec![b' '; MAX_ARTIFACT_BYTES + 1]).is_err());
}

fn other_batch(ticket: GapTicket, evidence: &str) -> GapBatch {
    GapBatch::new(
        RepoId::new("repo-1").unwrap(),
        MemberKey::parse_hex(&"11".repeat(32)).unwrap(),
        EvidenceDigest::parse_hex(&evidence.repeat(32)).unwrap(),
        1500,
        vec![ticket],
    )
    .unwrap()
}

#[test]
fn descending_wire_source_order_is_refused() {
    let first = vector();
    let mut second = first.batch().tickets()[0].clone();
    second.source_key = SourceKey::parse_hex(&"99".repeat(32)).unwrap();
    let batch = GapBatch::new(
        first.batch().repo_id().clone(),
        first.batch().member_key(),
        first.batch().evidence_digest(),
        1500,
        vec![second, first.batch().tickets()[0].clone()],
    )
    .unwrap();
    let bytes = encode_canonical(&[GapEnvelope::new(first.carrier_session_key(), batch)]).unwrap();
    let mut wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    wire.pointer_mut("/batch/tickets")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .reverse();
    assert!(decode_checked(format!("{wire}\n").as_bytes()).is_err());
}

#[test]
fn descending_wire_envelope_order_is_refused() {
    let first = vector();
    let mut ticket = first.batch().tickets()[0].clone();
    ticket.source_key = SourceKey::parse_hex(&"99".repeat(32)).unwrap();
    let second = GapEnvelope::new(first.carrier_session_key(), other_batch(ticket, "aa"));
    let encoded = String::from_utf8(encode_canonical(&[first, second]).unwrap()).unwrap();
    let reversed = encoded.lines().rev().collect::<Vec<_>>().join("\n") + "\n";
    assert!(decode_checked(reversed.as_bytes()).is_err());
}

#[test]
fn overlapping_physical_sources_in_one_artifact_are_refused() {
    let first = vector();
    let second = GapEnvelope::new(
        first.carrier_session_key(),
        other_batch(first.batch().tickets()[0].clone(), "aa"),
    );
    assert!(encode_canonical(&[first, second]).is_err());
}

#[test]
fn decoder_refuses_overlapping_independently_encoded_batches() {
    let first = vector();
    let second = GapEnvelope::new(
        first.carrier_session_key(),
        other_batch(first.batch().tickets()[0].clone(), "aa"),
    );
    let mut batches = [first, second];
    batches.sort_by_key(|envelope| envelope.batch().gap_id());
    let bytes: Vec<_> = batches
        .into_iter()
        .flat_map(|envelope| encode_canonical(&[envelope]).unwrap())
        .collect();
    assert!(decode_checked(&bytes).is_err());
}

#[test]
fn malformed_schema_fields_bounds_and_commitments_are_refused() {
    use serde_json::json;
    let original: serde_json::Value = serde_json::from_str(VECTOR).unwrap();
    for (pointer, replacement) in [
        ("/schema_version", json!(2)),
        ("/batch/schema_version", json!(0)),
        ("/batch/tickets/0/schema_version", json!(2)),
        ("/batch/recorded_at", json!(-1)),
        ("/batch/tickets/0/captured_at", json!(-1)),
        ("/batch/tickets/0/queued_at", json!(-1)),
        ("/batch/tickets/0/attempts", json!(u64::from(u32::MAX) + 1)),
        ("/batch/tickets/0/turn_ordinal", json!(-1)),
        ("/batch/ticket_count", json!(0)),
        ("/batch/ticket_count", json!(2)),
        ("/batch/tickets/0/head_oid", json!("AB".repeat(20))),
        ("/batch/tickets/0/head_oid", json!("aa")),
        ("/batch/tickets/0/boundary", json!("middle")),
        ("/batch/reason", json!("collected")),
        ("/batch/repo_id", json!("../secret")),
        ("/batch/gap_id", json!("aa".repeat(32))),
        ("/batch/selection_sha256", json!("aa".repeat(32))),
        ("/batch/member_key", json!("AA".repeat(32))),
        ("/carrier_session_key", json!("AA".repeat(32))),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert!(
            decode_checked(format!("{changed}\n").as_bytes()).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn unknown_and_duplicate_keys_at_each_structural_level_are_refused() {
    for field in ["carrier_session_key", "gap_id", "source_key"] {
        let needle = format!("\"{field}\":");
        let unknown = VECTOR.replacen(
            &needle,
            &format!("\"private-sentinel\":\"value-sentinel\",{needle}"),
            1,
        );
        assert!(decode_checked(format!("{unknown}\n").as_bytes()).is_err());
        let duplicate = VECTOR.replacen(&needle, &format!("{needle}null,{needle}"), 1);
        assert!(decode_checked(format!("{duplicate}\n").as_bytes()).is_err());
        let original: serde_json::Value = serde_json::from_str(VECTOR).unwrap();
        let pointer = match field {
            "carrier_session_key" => "/carrier_session_key",
            "gap_id" => "/batch/gap_id",
            _ => "/batch/tickets/0/source_key",
        };
        let value = original.pointer(pointer).unwrap();
        let both_valid = VECTOR.replacen(&needle, &format!("{needle}{value},{needle}"), 1);
        assert!(decode_checked(format!("{both_valid}\n").as_bytes()).is_err());
    }
    for bad in [
        format!("{VECTOR} {{}}\n"),
        format!("{VECTOR}\n\n"),
        VECTOR.to_owned(),
    ] {
        assert!(decode_checked(bad.as_bytes()).is_err());
    }
}

#[test]
fn constructor_sorts_but_wire_requires_unique_sorted_physical_members() {
    let first = vector();
    let a = first.batch().tickets()[0].clone();
    let mut b = a.clone();
    b.source_key = SourceKey::parse_hex(&"99".repeat(32)).unwrap();
    let build = |tickets| {
        GapBatch::new(
            first.batch().repo_id().clone(),
            first.batch().member_key(),
            first.batch().evidence_digest(),
            1500,
            tickets,
        )
    };
    assert_eq!(
        build(vec![a.clone(), b.clone()]).unwrap(),
        build(vec![b, a.clone()]).unwrap()
    );
    assert!(build(vec![a.clone(), a]).is_err());
}

#[test]
fn recorded_time_is_not_identity_but_is_part_of_exact_fact_equality() {
    let first = vector();
    let batch = first.batch();
    let changed = GapBatch::new(
        batch.repo_id().clone(),
        batch.member_key(),
        batch.evidence_digest(),
        batch.recorded_at() + 1,
        batch.tickets().to_vec(),
    )
    .unwrap();
    assert_eq!(batch.gap_id(), changed.gap_id());
    assert_eq!(batch.selection_digest(), changed.selection_digest());
    assert_ne!(*batch, changed);
}

#[test]
fn missing_nullable_head_is_not_silently_filled_from_absence() {
    let mut wire: serde_json::Value = serde_json::from_str(VECTOR).unwrap();
    wire.pointer_mut("/batch/tickets/0")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("head_oid");
    assert!(decode_checked(format!("{wire}\n").as_bytes()).is_err());
}

#[test]
fn explicit_null_head_and_checked_numeric_extremes_roundtrip() {
    let first = vector();
    let ticket = first.batch().tickets()[0].clone();
    let changed = GapTicket::new(GapTicketFields {
        source_key: ticket.source_key(),
        context_digest: ticket.context_digest(),
        ticket_digest: ticket.ticket_digest(),
        session_key: ticket.session_key(),
        turn_ordinal: u32::MAX,
        boundary: BoundaryEdge::Start,
        head_oid: None,
        captured_at: i64::MAX,
        queued_at: 0,
        attempts: u32::MAX,
    })
    .unwrap();
    let batch = other_batch(changed, "aa");
    let envelope = GapEnvelope::new(first.carrier_session_key(), batch);
    let encoded = encode_canonical(std::slice::from_ref(&envelope)).unwrap();
    assert!(String::from_utf8_lossy(&encoded).contains("\"head_oid\":null"));
    assert_eq!(decode_checked(&encoded).unwrap(), [envelope]);
}

#[test]
fn exact_ticket_budget_roundtrips_and_the_next_member_refuses() {
    let first = vector();
    let make = |count| {
        (0..count)
            .map(|source| {
                let mut ticket = first.batch().tickets()[0].clone();
                ticket.source_key = SourceKey::parse_hex(&format!("{source:064x}")).unwrap();
                ticket
            })
            .collect()
    };
    let original = first.batch();
    let batch = GapBatch::new(
        original.repo_id().clone(),
        original.member_key(),
        original.evidence_digest(),
        1500,
        make(MAX_TICKETS),
    )
    .unwrap();
    let envelope = GapEnvelope::new(first.carrier_session_key(), batch);
    let bytes = encode_canonical(std::slice::from_ref(&envelope)).unwrap();
    assert!(bytes.len() <= MAX_ARTIFACT_BYTES);
    assert_eq!(decode_checked(&bytes).unwrap(), [envelope]);
    assert!(GapBatch::new(
        original.repo_id().clone(),
        original.member_key(),
        original.evidence_digest(),
        1500,
        make(MAX_TICKETS + 1)
    )
    .is_err());
}

#[test]
fn every_ticket_field_contributes_to_selection_and_gap_identity() {
    let first = vector();
    let original = first.batch();
    for field in 0..10 {
        let mut ticket = original.tickets()[0].clone();
        match field {
            0 => ticket.source_key = SourceKey::parse_hex(&"aa".repeat(32)).unwrap(),
            1 => ticket.context_digest = ContextDigest::parse_hex(&"aa".repeat(32)).unwrap(),
            2 => ticket.ticket_digest = TicketDigest::parse_hex(&"aa".repeat(32)).unwrap(),
            3 => ticket.session_key = SessionKey::parse_hex(&"aa".repeat(32)).unwrap(),
            4 => ticket.turn_ordinal += 1,
            5 => ticket.boundary = BoundaryEdge::Start,
            6 => ticket.head_oid = None,
            7 => ticket.captured_at += 1,
            8 => ticket.queued_at += 1,
            _ => ticket.attempts += 1,
        }
        let changed = GapBatch::new(
            original.repo_id().clone(),
            original.member_key(),
            original.evidence_digest(),
            1500,
            vec![ticket],
        )
        .unwrap();
        assert_ne!(
            changed.selection_digest(),
            original.selection_digest(),
            "field {field}"
        );
        assert_ne!(changed.gap_id(), original.gap_id(), "field {field}");
    }
    for (repo, member, evidence) in [
        (
            RepoId::new("repo-2").unwrap(),
            original.member_key(),
            original.evidence_digest(),
        ),
        (
            original.repo_id().clone(),
            MemberKey::parse_hex(&"aa".repeat(32)).unwrap(),
            original.evidence_digest(),
        ),
        (
            original.repo_id().clone(),
            original.member_key(),
            EvidenceDigest::parse_hex(&"aa".repeat(32)).unwrap(),
        ),
    ] {
        let changed =
            GapBatch::new(repo, member, evidence, 1500, original.tickets().to_vec()).unwrap();
        assert_eq!(changed.selection_digest(), original.selection_digest());
        assert_ne!(changed.gap_id(), original.gap_id());
    }
}

#[test]
fn approved_exact_preimages_pin_domain_framing_and_field_order() {
    let envelope = vector();
    let selection_json = r#"[{"attempts":2,"boundary":"end","captured_at":1000,"context_sha256":"4444444444444444444444444444444444444444444444444444444444444444","head_oid":"7777777777777777777777777777777777777777","queued_at":1200,"schema_version":1,"session_key":"6666666666666666666666666666666666666666666666666666666666666666","source_key":"3333333333333333333333333333333333333333333333333333333333333333","ticket_sha256":"5555555555555555555555555555555555555555555555555555555555555555","turn_ordinal":7}]"#;
    let identity_json = r#"{"evidence_sha256":"2222222222222222222222222222222222222222222222222222222222222222","member_key":"1111111111111111111111111111111111111111111111111111111111111111","reason":"uncaptured-backlog","repo_id":"repo-1","schema_version":1,"selection_sha256":"7befeea2bbcd241de610e8f572200e84b5d1adb5cdffe828a529750e323e2f5a"}"#;
    assert_eq!(
        hash(
            b"rift-boundary-gap-selection@1\0",
            selection_json.as_bytes()
        ),
        envelope.batch().selection_digest().0
    );
    assert_eq!(
        hash(b"rift-boundary-gap@1\0", identity_json.as_bytes()),
        envelope.batch().gap_id().0
    );
}
