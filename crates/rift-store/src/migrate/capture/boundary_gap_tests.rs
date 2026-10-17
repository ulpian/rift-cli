use super::*;
use crate::{CanonicalStore, SqliteStore};

// Captured from compiled a7619bfc builders BEFORE gap schema changes.
const FROZEN: &[(u32, &str, &str)] = &[
    (
        1,
        "init_meta",
        "c8eab2f89a2c627c063adafdf413056db47906a78ebda36a95515cfc300b8148",
    ),
    (
        2,
        "canonical_core",
        "1dc6a86bd030cda355dd6998fd97b84a12b0f2b8d5eafde03d36bee8618ccc09",
    ),
    (
        3,
        "capture_identity",
        "189e0ec139713ae08f93fa6a8224375d60105354b14536432dbb173b7514ff33",
    ),
    (
        4,
        "capture_turn_boundary",
        "ac8b1ac35ba0a943b5a4bc7fcbf285e4d321214d091cce10b91ea7b1a886d243",
    ),
    (
        5,
        "capture_projection_meta",
        "f3c6ef11848a92d8bbc0d360773987373a3ff82de0c338a1cf86dc9cca7ad00c",
    ),
    (
        6,
        "capture_read_tokens",
        "163491396d1e8e0cd0948b61c5e02f0cee51f97efe2667df8053bd204ecb6115",
    ),
    (
        7,
        "capture_system_truth",
        "08039e8ad9744c0413cc82f32f19ae5bfbe06ebcd93025b9cde04a056b150747",
    ),
    (
        8,
        "capture_price_seed_current",
        "2c91d63805fc51027a9495038cd7a46077e9b7b51259257bdc59a6ab3cf2b708",
    ),
    (
        9,
        "capture_model_observation",
        "4207ef726e0039d3902f757dad71bf74fbb2725a814669a0495eb895f360534b",
    ),
    (
        10,
        "capture_hook_event_identity_index",
        "224725ead453934d3148f500d5e42f7cf901a1e9b8068a695e9498a63920786e",
    ),
    (
        11,
        "capture_checkpoint_commit",
        "931230d3a851c889b9a928a8d5002595aaf00da97a8f5da7f28e8299a6749c8e",
    ),
    (
        12,
        "capture_verification_witness",
        "56984a7b11fac42819ca0c757982c3728c975ebdbb8984eaacdac4d7c15964e7",
    ),
    (
        13,
        "capture_imported_checkpoint",
        "6894b4be5e4c5819ca7466ef0ee19d7427398e3f2ebb41bc6cf00eb33011a61c",
    ),
    (
        14,
        "capture_worktree_provenance",
        "6b77adb2e63a92746871fdf1a9b362605b1859e649c1837e9555f824eaf4421d",
    ),
    (
        15,
        "capture_tool_result_tokens",
        "b5c9899faeda319f768b8a8c3ed0deb52d1c65d07e645f57f05bbd98605582bc",
    ),
    (
        16,
        "capture_price_seed_observed",
        "30fd2335439b59009c54ff1cf33f5f4b61f7e6ed7c639f5536388051cdffd85f",
    ),
    (
        17,
        "capture_worktree_cd",
        "467cbbe65e05aaec5fa8a6382a2d502c8e5b0469cfa0cae1da5245db03c8d367",
    ),
    (
        18,
        "capture_price_seed_opus_5_5",
        "92a4289729def2a86e95d2c41431dd00f87a25ce329424c0c573d2f0a064c88d",
    ),
    (
        19,
        "capture_boundary_file_kind",
        "0c6bd126662e017fd2c4cdb89c2a14f497f139f07b8420ab7fc4c576ccab35e3",
    ),
    (
        20,
        "capture_boundary_observation",
        "2ba4c2cf50d418d635ce9fb193dc5fc417f2cb602007bbed42404c38da77034f",
    ),
];

#[test]
fn capture_gap_migration_preserves_the_frozen_twenty_sql_bodies() {
    let migrations = default_migrations();
    assert_eq!(migrations.len(), 22);
    let saved = std::env::var_os("RIFT_GAP_MIGRATION_BASELINE").map(std::path::PathBuf::from);
    for (actual, (version, name, checksum)) in migrations.iter().zip(FROZEN) {
        assert_eq!((actual.version, actual.name), (*version, *name));
        assert_eq!(&crate::migrate::checksum_hex(&actual.sql), checksum);
        if let Some(dir) = &saved {
            let bytes = std::fs::read(dir.join(format!("migration-{version:02}.sql"))).unwrap();
            assert_eq!(actual.sql.as_bytes(), bytes, "compiled SQL v{version}");
        }
    }
    if saved.is_some() {
        println!("verified exact saved compiled migration SQL bodies: 20");
    }
}

#[test]
fn boundary_gap_relations_are_registered() {
    for name in ["turn_boundary_gap_batch", "turn_boundary_gap_ticket"] {
        assert!(crate::schema::canonical_table(name).is_some(), "{name}");
    }
}

#[test]
fn boundary_gap_relations_exist_in_a_fresh_capture_projection() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt
        .block_on(store.query(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN \
         ('turn_boundary_gap_batch', 'turn_boundary_gap_ticket')",
            &[],
        ))
        .unwrap();
    assert_eq!(rows[0].get_i64(0).unwrap(), 2);
}

#[test]
fn a_v20_projection_gains_only_gap_schema_and_preserves_old_rows_and_checksums() {
    use crate::migrate::apply_migrations;
    let store = SqliteStore::open_in_memory().unwrap();
    let all = default_migrations();
    let historical = &all[..21];
    apply_migrations(&store, &historical[..20]).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let snapshot = crate::TurnBoundarySnapshotRow {
        snapshot_id: "11".repeat(32),
        session_id: "22".repeat(32),
        turn_ordinal: 7,
        boundary: "end".into(),
        actor: "agent".into(),
        worktree_hash: "33".repeat(32),
        captured_at: 1000,
        schema_version: 1,
    };
    assert!(rt
        .block_on(crate::record_turn_boundary_snapshot(&store, &snapshot))
        .unwrap());
    let before = rt
        .block_on(store.query(
            "SELECT version, name, checksum FROM _migrations ORDER BY version",
            &[],
        ))
        .unwrap();
    let old_rows = rt
        .block_on(store.query("SELECT * FROM turn_boundary_snapshot", &[]))
        .unwrap();
    let outcome = apply_migrations(&store, historical).unwrap();
    assert_eq!(
        (outcome.applied, outcome.skipped, outcome.high_water),
        (1, 20, 21)
    );
    assert_eq!(
        rt.block_on(store.query(
            "SELECT version, name, checksum FROM _migrations WHERE version <= 20 ORDER BY version",
            &[]
        ))
        .unwrap(),
        before
    );
    assert_eq!(
        rt.block_on(store.query("SELECT * FROM turn_boundary_snapshot", &[]))
            .unwrap(),
        old_rows
    );
    for name in ["turn_boundary_gap_batch", "turn_boundary_gap_ticket"] {
        assert_eq!(rt.block_on(store.count(name)).unwrap(), 0);
    }
    assert_eq!(apply_migrations(&store, historical).unwrap().applied, 0);
}
