//! The canopy UI-state projection (`ui-state.db`): machine-global, append-only
//! dashboard facts. Its own module because these rows never touch capture or
//! recall data yet obey the same immutability discipline.

use super::{CanonicalTable, Column, NO_UNIQUE};

// --- Canopy UI-state -------------------------------------------------------
// Machine-global, append-only UI facts: which projects the dashboard watches and
// each project's feature-toggle intent. A SEPARATE registry + db file
// (`ui-state.db`) from the per-repo capture projection, so these never touch
// capture data — yet they still obey the immutability discipline: an
// `op`/`desired` change is a NEW event row (never an in-place UPDATE), and the
// current state is the fold of the latest-valid event per key. `op`/`feature` are
// plain `TEXT` validated at the Rust writer (the same "CHECK deferred" posture
// the capture schema takes), so
// the store stays decoupled from Canopy's enum spelling.

const CANOPY_PROJECT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("project_id"),      // stable per registered project
    Column::text("op"),              // 'added' | 'removed' | 'relabeled' | 'reordered'
    Column::text_null("repo_id"),    // set on 'added'
    Column::text_null("path"),       // set on 'added'
    Column::text_null("label"),      // set on 'added'/'relabeled'
    Column::int_null("order_key"),   // set on 'reordered'
    Column::text_null("supersedes"), // prior event_id this corrects
    Column::int("created_at"),       // transaction-time (epoch ms)
];

const CANOPY_TOGGLE_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("project_id"),      // FK: a canopy_project_event.project_id (comment-FK)
    Column::text("feature"), // 'fff' | 'codegraph' | 'codegraph-typescript' | … | 'rift-total-recall'
    Column::int("desired"),  // 1 = on, 0 = off (the user's intent at this time)
    Column::text_null("supersedes"), // prior event_id for this (project,feature)
    Column::int("created_at"), // transaction-time (epoch ms)
];

// Per-project dashboard arrangement: the panel editor's layout, saved
// per project_id as a superseding event. `layout` is the encoded LayoutModel;
// NULL is a reset to the default (a superseding event, never a delete).
const CANOPY_LAYOUT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("project_id"),      // FK: a canopy_project_event.project_id (comment-FK)
    Column::text_null("layout"),     // encoded LayoutModel; NULL = reset-to-default
    Column::text_null("supersedes"), // prior layout event_id for this project
    Column::int("created_at"),       // transaction-time (epoch ms)
];

// The profile plane: named, machine-global operator views. A profile
// scopes what the dashboard shows — its own membership over the global catalogue
// (`canopy_project_event` stays the registration source of truth), its own
// per-(profile, project) arrangements, and its own theme slots. All
// per-key, time-varying, superseding-event state like its siblings above.
const CANOPY_PROFILE_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("profile_id"),      // stable per profile (ULID)
    Column::text("op"),              // 'created' | 'renamed' | 'removed' | 'selected'
    Column::text_null("name"),       // set on 'created'/'renamed'
    Column::text_null("supersedes"), // prior event_id this corrects
    Column::int("created_at"),       // transaction-time (epoch ms)
];

const CANOPY_PROFILE_PROJECT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("profile_id"),      // FK: a canopy_profile_event.profile_id (comment-FK)
    Column::text("project_id"),      // FK: a canopy_project_event.project_id (comment-FK)
    Column::text("op"),              // 'added' | 'removed' | 'reordered'
    Column::int_null("order_key"),   // set on 'added'/'reordered'
    Column::text_null("supersedes"), // prior event_id for this (profile,project)
    Column::int("created_at"),       // transaction-time (epoch ms)
];

// Per-(profile, project) arrangement, superseding the per-project table for
// writes: the `LayoutModel` codec is reused unchanged;
// NULL is a reset to the default. `canopy_layout_event` is frozen for writes and
// stays readable as the default profile's fallback.
const CANOPY_PROFILE_LAYOUT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("profile_id"),      // FK: a canopy_profile_event.profile_id (comment-FK)
    Column::text("project_id"),      // FK: a canopy_project_event.project_id (comment-FK)
    Column::text_null("layout"),     // encoded LayoutModel; NULL = reset-to-default
    Column::text_null("supersedes"), // prior layout event_id for this (profile,project)
    Column::int("created_at"),       // transaction-time (epoch ms)
];

// Per-(profile, slot) surface-theme overrides: `colour` is `#RRGGBB`
// hex with NULL = reset-to-default; latest-wins per (profile, slot). Slots are
// the five roles (background/border/button/graph/text) validated at the writer.
const CANOPY_PROFILE_THEME_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("profile_id"),      // FK: a canopy_profile_event.profile_id (comment-FK)
    Column::text("slot"),            // 'background' | 'border' | 'button' | 'graph' | 'text'
    Column::text_null("colour"),     // '#RRGGBB'; NULL = reset-to-default
    Column::text_null("supersedes"), // prior event_id for this (profile,slot)
    Column::int("created_at"),       // transaction-time (epoch ms)
];

// The startup pin: a single latest-wins fold — the profile launch
// opens on, NULL = follow last-selected (today's behaviour). Launch only ever
// READS this table; pinning/unpinning are ordinary superseding events.
const CANOPY_PROFILE_STARTUP_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text_null("profile_id"), // FK comment-FK; NULL = follow last-selected
    Column::text_null("supersedes"), // prior startup event_id
    Column::int("created_at"),       // transaction-time (epoch ms)
];

const CANOPY_TABLES: &[CanonicalTable] = &[
    CanonicalTable {
        name: "canopy_project_event",
        columns: CANOPY_PROJECT_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_toggle_event",
        columns: CANOPY_TOGGLE_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_layout_event",
        columns: CANOPY_LAYOUT_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_event",
        columns: CANOPY_PROFILE_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_project_event",
        columns: CANOPY_PROFILE_PROJECT_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_layout_event",
        columns: CANOPY_PROFILE_LAYOUT_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_theme_event",
        columns: CANOPY_PROFILE_THEME_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_startup_event",
        columns: CANOPY_PROFILE_STARTUP_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
];

/// The registry of canopy UI-state (`ui-state.db`) canonical tables. Machine-
/// global and append-only, iterated by the immutability property test **in
/// addition to** [`super::canonical_tables`]/[`super::recall_canonical_tables`], so every
/// canopy table gets its append-only guard for free.
#[must_use]
pub fn canopy_canonical_tables() -> &'static [CanonicalTable] {
    CANOPY_TABLES
}

/// Look up a canopy canonical table by name.
#[must_use]
pub fn canopy_canonical_table(name: &str) -> Option<&'static CanonicalTable> {
    CANOPY_TABLES.iter().find(|t| t.name == name)
}
