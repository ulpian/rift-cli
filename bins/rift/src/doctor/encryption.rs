//! At-rest encryption: the family's column key, and the summaries still
//! stored in plaintext because they were bound before there was one.
//!
//! Both legs are read-only. The key leg judges the key file without minting
//! one (a doctor run must not create the key it reports on) and never reads
//! the key's bytes. The summary leg counts plain-framed rows; it cannot seal
//! them, because `digest_blob` is a canonical, append-only table whose rows
//! are addressed by their plaintext digest — a sealed copy could neither
//! replace the row nor sit beside it under the same address, and a
//! superseding row would leave the plaintext in the file anyway. What removes
//! them is the projection's rebuild, which sets the old file aside and binds
//! every summary afresh, sealed.

use std::path::Path;

use rift_store::KeyFileState;

use crate::family;

/// The family's column key file: missing (fine — minted on first use),
/// usable, or a state every seal and open refuses, each with the sentence
/// that fixes it. The JSON carries tags and numbers, never the path.
pub(super) fn doctor_column_key(root: &Path) -> (String, serde_json::Value) {
    let Some(family) = family::try_at(root) else {
        return (String::new(), serde_json::json!({ "surveyed": false }));
    };
    match crate::column_key::key_file_of(&family) {
        Some(path) => key_file_report(&path, rift_store::inspect_key_file(&path)),
        None => (
            format!(
                "\n  column key: {} — recall summaries can be neither sealed nor read",
                rift_registry::CONFIG_ROOT_UNSET
            ),
            serde_json::json!({ "surveyed": true, "state": "config_root_unset" }),
        ),
    }
}

/// The leg's line and object for the key file at `path` in `state`.
fn key_file_report(path: &Path, state: KeyFileState) -> (String, serde_json::Value) {
    let shown = path.display();
    let human = match state {
        KeyFileState::Missing => {
            "\n  column key: not minted yet — the first summary sealed mints it".to_owned()
        }
        KeyFileState::Usable => "\n  column key: present, owner-only".to_owned(),
        KeyFileState::Malformed { len } => format!(
            "\n  column key: {shown} is {len} byte(s), not a 32-byte key, so nothing is \
             sealed or opened — move it aside, then run `rift-memd rebuild` to seal \
             under a fresh key"
        ),
        KeyFileState::OpenToOthers { mode } => format!(
            "\n  column key: {shown} is open to other users (mode {mode:o}) and is refused \
             until it is not — run `chmod 600 {shown}`"
        ),
        KeyFileState::Unreadable => format!(
            "\n  column key: {shown} cannot be read by this user — check its owner and \
             that it is a regular file"
        ),
    };
    let detail = match state {
        KeyFileState::Malformed { len } => Some(("bytes", len)),
        KeyFileState::OpenToOthers { mode } => Some(("mode", u64::from(mode))),
        _ => None,
    };
    let mut json = serde_json::Map::new();
    json.insert("surveyed".to_owned(), serde_json::json!(true));
    json.insert("state".to_owned(), serde_json::json!(state.as_str()));
    if let Some((key, value)) = detail {
        json.insert(key.to_owned(), serde_json::json!(value));
    }
    (human, serde_json::Value::Object(json))
}

/// How many summary and outline bodies the family's recall projection still
/// holds in plaintext, and what seals them.
pub(super) fn doctor_plaintext_summaries(root: &Path) -> (String, serde_json::Value) {
    let Some(db) = family::recall_db(root).ok().filter(|db| db.exists()) else {
        return (String::new(), serde_json::json!({ "surveyed": false }));
    };
    match count_plaintext(&db) {
        Some(count) => plaintext_report(count),
        None => (
            String::new(),
            serde_json::json!({ "surveyed": false, "unreadable": true }),
        ),
    }
}

/// The plain-framed rows of `digest_blob` in the projection at `db`, read
/// through a read-only handle; `None` when the store cannot be read.
fn count_plaintext(db: &Path) -> Option<i64> {
    let store = rift_store::SqliteStore::open_read_only(db).ok()?;
    let rt = crate::projection::runtime().ok()?;
    let rows = rt
        .block_on(rift_store::CanonicalStore::query(
            &store,
            "SELECT COUNT(*) FROM digest_blob WHERE substr(bytes, 1, 1) = x'00'",
            &[],
        ))
        .ok()?;
    rows.first()?.get_i64(0).ok()
}

/// The leg's line and object for `count` plaintext rows.
fn plaintext_report(count: i64) -> (String, serde_json::Value) {
    let human = if count == 0 {
        "\n  recall summaries at rest: all sealed".to_owned()
    } else {
        format!(
            "\n  recall summaries at rest: {count} stored before encryption, in plaintext — \
             run `rift-memd rebuild` to seal them (their rows are append-only, so only a \
             rebuild replaces them), then delete the set-aside file it names once \
             `rift backfill --recall-from` has read it back"
        )
    };
    (
        human,
        serde_json::json!({ "surveyed": true, "plaintext_rows": count }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_key_state_reads_as_one_actionable_line_and_a_path_free_object() {
        let path = Path::new("/cfg/rift/keys/fam.key");
        let (missing, json) = key_file_report(path, KeyFileState::Missing);
        assert!(missing.contains("not minted yet"), "{missing}");
        assert_eq!(json["state"], "missing");
        let (usable, _) = key_file_report(path, KeyFileState::Usable);
        assert!(usable.contains("present, owner-only"), "{usable}");
        let (short, json) = key_file_report(path, KeyFileState::Malformed { len: 5 });
        assert!(short.contains("5 byte(s)") && short.contains("rift-memd rebuild"));
        assert_eq!(json["bytes"], 5);
        let (open, json) = key_file_report(path, KeyFileState::OpenToOthers { mode: 0o644 });
        assert!(open.contains("mode 644") && open.contains("chmod 600 /cfg/rift/keys/fam.key"));
        assert_eq!(json["mode"], 0o644);
        let (unreadable, json) = key_file_report(path, KeyFileState::Unreadable);
        assert!(unreadable.contains("cannot be read"), "{unreadable}");
        assert_eq!(json["state"], "unreadable");
        for state in [
            KeyFileState::Missing,
            KeyFileState::Malformed { len: 5 },
            KeyFileState::OpenToOthers { mode: 0o644 },
        ] {
            let (_, json) = key_file_report(path, state);
            assert!(!json.to_string().contains("/cfg"), "{json}");
        }
    }

    #[test]
    fn plaintext_rows_are_counted_and_the_rebuild_is_named() {
        assert_eq!(
            plaintext_report(0).0,
            "\n  recall summaries at rest: all sealed"
        );
        let (human, json) = plaintext_report(3);
        assert!(human.contains("3 stored before encryption"), "{human}");
        assert!(human.contains("rift-memd rebuild"), "{human}");
        assert_eq!(json["plaintext_rows"], 3);
    }

    /// A projection holding one plain-framed and one sealed body counts one.
    #[test]
    fn only_plain_framed_bodies_are_counted() {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let fam = family::resolve_at(tmp.path()).unwrap();
        let db = family::recall_db_of(&fam);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = rift_store::open_recall_projection(&db).unwrap();
        let rt = crate::projection::runtime().unwrap();
        let versions =
            rift_recall_serve::outline_versions(rift_recall_policy::SERVE_POLICY_VERSION);
        let put = |cipher: &dyn rift_store::ColumnCipher, source: &[u8], digest: &[u8]| {
            rt.block_on(rift_recall_ledger::validity_store::put_summary(
                &store, cipher, "a.rs", source, versions, digest,
            ))
            .unwrap();
        };
        put(&rift_store::PlainCipher, b"one", b"plain digest");
        put(
            &crate::column_key::column_cipher(&fam),
            b"two",
            b"sealed digest",
        );
        drop(store);
        assert_eq!(count_plaintext(&db), Some(1));
        let (human, _) = doctor_plaintext_summaries(tmp.path());
        assert!(human.contains("1 stored before encryption"), "{human}");
        let (key, json) = doctor_column_key(tmp.path());
        assert!(key.contains("present, owner-only"), "{key}");
        assert_eq!(json["state"], "usable");
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }
}
