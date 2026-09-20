//! The canopy UI-state projection's migration set (`ui-state.db`): five
//! versions, each a frozen, name-selected subset of the canopy registry.
//!
//! A separate module because its selection is by table name rather than by
//! registry slice, and because `ui-state.db` is machine-global — it upgrades
//! in place on every Canopy start, so every applied checksum here is locked.

use super::Migration;

/// The canopy UI-state projection's migration set (applied to a fresh or an
/// existing `ui-state.db`). v1 is the frozen project + toggle core; v2
/// adds the per-project layout table; v3 adds the profile plane; v4 adds the
/// per-(profile, slot) theme table; v5 adds
/// the startup-pin table — each a new version so an existing store
/// upgrades additively and the applied checksums stay locked.
#[must_use]
pub fn canopy_migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            name: "canopy_core",
            sql: canopy_schema_sql_for(&["canopy_project_event", "canopy_toggle_event"]),
        },
        Migration {
            version: 2,
            name: "canopy_layout",
            sql: canopy_schema_sql_for(&["canopy_layout_event"]),
        },
        Migration {
            version: 3,
            name: "canopy_profiles",
            sql: canopy_schema_sql_for(&[
                "canopy_profile_event",
                "canopy_profile_project_event",
                "canopy_profile_layout_event",
            ]),
        },
        Migration {
            version: 4,
            name: "canopy_profile_theme",
            sql: canopy_schema_sql_for(&["canopy_profile_theme_event"]),
        },
        Migration {
            version: 5,
            name: "canopy_profile_startup",
            sql: canopy_schema_sql_for(&["canopy_profile_startup_event"]),
        },
    ]
}

/// The full canopy UI-state schema SQL (every table, all versions) — for the
/// additive-migration check and the DDL snapshot. Not a single migration (the
/// layout table arrived in v2); see [`canopy_migrations`].
#[must_use]
pub fn canopy_canonical_schema_sql() -> String {
    use crate::schema::canopy_canonical_tables;
    let all: Vec<&str> = canopy_canonical_tables().iter().map(|t| t.name).collect();
    canopy_schema_sql_for(&all)
}

/// Build the canopy schema SQL for the registry tables named in `names` (in
/// registry order): create + append-only guard per table, then a
/// `_schema_version` row per table. Splitting by name lets each migration
/// **version** own a frozen, checksum-stable subset (a new table is a new
/// version, never an edit to an applied one — the frozen-prefix rule).
fn canopy_schema_sql_for(names: &[&str]) -> String {
    use crate::schema::canopy_canonical_tables;
    let selected = || {
        canopy_canonical_tables()
            .iter()
            .filter(|t| names.contains(&t.name))
    };
    let mut sql = String::new();
    for table in selected() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in selected() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::fixtures::store;
    use crate::migrate::{apply_migrations, is_additive};

    /// The canopy UI-state migrations create the append-only tables (v1 core +
    /// v2 layout + v3 profiles) and re-run idempotently (a fresh `ui-state.db`
    /// upgrade).
    #[test]
    fn canopy_migrations_apply_and_rerun_cleanly() {
        let s = store();
        let out = apply_migrations(&s, &canopy_migrations()).unwrap();
        assert_eq!(
            out.applied, 5,
            "v1 core + v2 layout + v3 profiles + v4 theme + v5 startup"
        );
        assert_eq!(out.high_water, 5);
        let v1 = &canopy_migrations()[0].sql;
        assert!(v1.contains("CREATE TABLE IF NOT EXISTS canopy_project_event"));
        assert!(v1.contains("CREATE TABLE IF NOT EXISTS canopy_toggle_event"));
        assert!(v1.contains("canopy_project_event_no_update"));
        assert!(v1.contains("canopy_toggle_event_no_delete"));
        // v1 stays frozen (no layout table — the checksum lock holds for existing
        // stores); v2 adds the layout table with its append-only guards.
        assert!(!v1.contains("canopy_layout_event"), "v1 is the frozen core");
        let v2 = &canopy_migrations()[1].sql;
        assert!(v2.contains("CREATE TABLE IF NOT EXISTS canopy_layout_event"));
        assert!(v2.contains("canopy_layout_event_no_update"));
        assert!(v2.contains("canopy_layout_event_no_delete"));
        // v2 stays frozen too (no profile tables); v3 adds the profile plane
        // with its append-only guards.
        assert!(
            !v2.contains("canopy_profile"),
            "v2 is the frozen layout slice"
        );
        let v3 = &canopy_migrations()[2].sql;
        assert!(v3.contains("CREATE TABLE IF NOT EXISTS canopy_profile_event"));
        assert!(v3.contains("CREATE TABLE IF NOT EXISTS canopy_profile_project_event"));
        assert!(v3.contains("CREATE TABLE IF NOT EXISTS canopy_profile_layout_event"));
        assert!(v3.contains("canopy_profile_event_no_update"));
        assert!(v3.contains("canopy_profile_project_event_no_delete"));
        assert!(v3.contains("canopy_profile_layout_event_no_update"));
        // v3 stays frozen (no theme table); v4 adds it with guards.
        assert!(!v3.contains("canopy_profile_theme_event"), "v3 is frozen");
        let v4 = &canopy_migrations()[3].sql;
        assert!(v4.contains("CREATE TABLE IF NOT EXISTS canopy_profile_theme_event"));
        assert!(v4.contains("canopy_profile_theme_event_no_update"));
        assert!(v4.contains("canopy_profile_theme_event_no_delete"));
        // v4 stays frozen (no startup table); v5 adds it with guards.
        assert!(!v4.contains("canopy_profile_startup_event"), "v4 is frozen");
        let v5 = &canopy_migrations()[4].sql;
        assert!(v5.contains("CREATE TABLE IF NOT EXISTS canopy_profile_startup_event"));
        assert!(v5.contains("canopy_profile_startup_event_no_update"));
        assert!(v5.contains("canopy_profile_startup_event_no_delete"));
        let again = apply_migrations(&s, &canopy_migrations()).unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.skipped, 5);
    }

    /// Every canopy migration is additive (the destructive-DDL CI gate).
    #[test]
    fn every_canopy_migration_is_additive() {
        for m in canopy_migrations() {
            is_additive(&m.sql).unwrap();
        }
    }
}
