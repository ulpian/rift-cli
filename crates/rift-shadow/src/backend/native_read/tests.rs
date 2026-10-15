use super::*;
use rift_recall_substrate::MemorySubstrate;

#[test]
fn metadata_presence_and_parse_success_remain_distinct() {
    let absent = shard_header(&MemorySubstrate::local(), "fixture").unwrap();
    assert!(absent.metadata_bytes.is_none());
    assert!(absent.doc.is_none());
    let malformed = shard_header(
        &MemorySubstrate::local().with_artifact("fixture", "metadata.json", b"{"),
        "fixture",
    )
    .unwrap();
    assert_eq!(malformed.metadata_bytes.as_deref(), Some(b"{".as_slice()));
    assert!(malformed.doc.is_none());
    let null = shard_header(
        &MemorySubstrate::local().with_artifact("fixture", "metadata.json", b"null"),
        "fixture",
    )
    .unwrap();
    assert_eq!(null.doc, Some(serde_json::Value::Null));
}
