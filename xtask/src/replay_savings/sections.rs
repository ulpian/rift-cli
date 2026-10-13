//! The sections that size the next levers rather than measure this build:
//! where the facts came from, the client's own declines, the sibling re-reads
//! by inheritance route, the machine-format census, the first-sight sizes and
//! the shapes of the shell reads the grammar refused.
//!
//! Kept apart from the table they extend so the table's existing lines — which
//! two runs are compared by — are visibly untouched by them. Aggregates only,
//! like every other surface of the command: class names and counts, never a
//! path or a prompt.

use rift_agent::window_journal::InheritClass;
use serde_json::{json, Map, Value};

use super::bucket::Bucket;
use super::census::{
    MachineFormat, BYTE_BANDS, CODE_EXTENSIONS, FIRST_CONTACT_BYTES, FIRST_CONTACT_LINES,
    LINE_BANDS,
};
use super::inheritance::Inheritance;
use super::render::{commas, row};
use super::report::{share_percent, Report, Tally};
use super::shell_shape::ranked;

/// The header line naming where the path facts came from.
pub(super) fn facts_header(report: &Report) -> String {
    let mode = if report.facts.history {
        "history at each window's commit"
    } else {
        "worktree"
    };
    format!("  facts from: {mode}\n")
}

/// The corpus lines only a history run has: what the commits answered, and
/// what the worktree had to.
pub(super) fn history_lines(report: &Report) -> String {
    let facts = report.facts;
    if !facts.history {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "paths answered at a window's commit / worktree",
        commas(facts.resolved_at_a_commit),
        commas(facts.fell_back_to_the_worktree)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "windows with no commit at or before them",
        commas(facts.windows_without_a_commit)
    ));
    out
}

/// The client's own declines, beside this build's.
pub(super) fn declined_by_the_client(report: &Report, denominator: u64) -> String {
    row(
        "declined by the client's own repeat guard",
        report.bucket(Bucket::ClientDedup),
        denominator,
    )
}

/// The unit the routes are counted in, under the pairs line it splits.
///
/// The pairs line counts distinct `(path, revision)` pairs; the routes count
/// one per later window's first read of such a pair, so a pair five windows
/// read is one pair and four reads. Without this line the routes would read
/// as a split of a number smaller than their sum.
pub(super) fn later_first_reads(report: &Report, denominator: u64) -> String {
    row(
        "  later windows' first reads of them",
        report.cross_window,
        denominator,
    )
}

/// The sibling re-reads, one line per inheritance route, and across the
/// routes the part the section a window opens with would have named.
pub(super) fn sibling_classes(report: &Report, denominator: u64) -> String {
    let mut out = String::new();
    for class in Inheritance::ALL {
        let tally = inheritance_tally(report, class);
        out.push_str(&row(class.label(), tally, denominator));
    }
    out.push_str(&row(
        "    named at the spawn by the opening section",
        report.sibling_named_at_spawn,
        denominator,
    ));
    out
}

/// What the opening windows inherited and what each class would have
/// declined, which ran because no class is enforced, and how the spawns were
/// linked to the windows they opened.
pub(super) fn inherited_classes(report: &Report, denominator: u64) -> String {
    let mut out = String::new();
    for (at, class) in InheritClass::ALL.into_iter().enumerate() {
        let lines = report.inherited_lines.get(at).copied().unwrap_or_default();
        out.push_str(&format!(
            "  {:<48}{:>9}\n",
            format!("  inherited by {}: lines at the spawn", class.as_str()),
            commas(lines)
        ));
        out.push_str(&row(
            "    reads it alone would decline (observed)",
            report
                .inherited_observed
                .get(at)
                .copied()
                .unwrap_or_default(),
            denominator,
        ));
    }
    let links = report.spawn_links;
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "  spawns linked by prompt id and type, exactly",
        format!("{} of {}", commas(links.exact), commas(links.openings))
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "    linked to the wrong spawn",
        commas(links.wrong)
    ));
    out
}

/// The machine-format census: every class, and its first-sight part.
pub(super) fn machine_formats(report: &Report, denominator: u64) -> String {
    let mut out = String::new();
    out.push_str("\n  machine-format reads, by path shape");
    out.push_str(&format!(
        "\n  {:<48}{:>9}{:>15}{:>13}\n",
        "", "calls", "tokens", "of content"
    ));
    for format in MachineFormat::ALL {
        let census = report
            .machine_formats
            .get(format.key())
            .copied()
            .unwrap_or_default();
        out.push_str(&row(format.key(), census.all, denominator));
        out.push_str(&row(
            "  of it, first sight",
            census.first_sight,
            denominator,
        ));
    }
    out
}

/// The refused shell reads by shape, ranked by the tokens of the calls naming
/// a work-tree file, each beside its share of that pool and its count on the
/// loose bound. Shape names only: a command line never reaches the table.
pub(super) fn refused_shapes(report: &Report) -> String {
    let pool = report.invisible_shell_named_file.tokens;
    let mut out = format!(
        "    {:<46}{:>9}{:>15}{:>13}{:>9}{:>15}\n",
        "refused, by shape (naming a file / loose)",
        "calls",
        "tokens",
        "of named",
        "calls",
        "tokens"
    );
    for (shape, row) in ranked(&report.invisible_shell_shapes) {
        out.push_str(&format!(
            "      {:<44}{:>9}{:>15}{:>11.2} %{:>9}{:>15}\n",
            shape.key(),
            commas(row.named.calls),
            commas(row.named.tokens),
            share_percent(row.named.tokens, pool),
            commas(row.all.calls),
            commas(row.all.tokens)
        ));
    }
    out
}

/// The first-sight calls by file size, and the first-contact floor.
pub(super) fn first_sight_sizes(report: &Report, denominator: u64) -> String {
    let sizes = &report.first_sight_sizes;
    let mut out = String::new();
    out.push_str("\n  first sight, by the size of the file read\n");
    for ((_, label, _), tally) in LINE_BANDS.iter().zip(sizes.by_lines) {
        out.push_str(&row(label, tally, denominator));
    }
    for ((_, label, _), tally) in BYTE_BANDS.iter().zip(sizes.by_bytes) {
        out.push_str(&row(label, tally, denominator));
    }
    let floor = format!(
        "≥ {} lines or ≥ {} bytes",
        commas(FIRST_CONTACT_LINES),
        commas(FIRST_CONTACT_BYTES)
    );
    out.push_str(&row(&floor, sizes.first_contact, denominator));
    out.push_str(&row("  of it, code", sizes.first_contact_code, denominator));
    out.push_str(&row(
        "  of it, Markdown",
        sizes.first_contact_markdown,
        denominator,
    ));
    out.push_str(&row("  of it, TOML", sizes.first_contact_toml, denominator));
    out
}

/// Add the new keys to the machine-readable document. Every one is new, so a
/// reader of the previous shape reads this one unchanged.
pub(super) fn extend_document(document: &mut Value, report: &Report) {
    let Some(map) = document.as_object_mut() else {
        return;
    };
    if let Some(declined) = map.get_mut("declined").and_then(Value::as_object_mut) {
        declined.insert(
            "by_the_client".to_owned(),
            tally_json(report.bucket(Bucket::ClientDedup)),
        );
    }
    if let Some(cross) = map
        .get_mut("cross_window_repeats")
        .and_then(Value::as_object_mut)
    {
        cross.insert("by_inheritance".to_owned(), inheritance_json(report));
        cross.insert(
            "named_at_spawn".to_owned(),
            tally_json(report.sibling_named_at_spawn),
        );
        cross.insert("reads".to_owned(), report.cross_window.calls.into());
    }
    map.insert("inheritance".to_owned(), inherited_json(report));
    map.insert("facts".to_owned(), facts_json(report));
    map.insert(
        "machine_format_reads".to_owned(),
        machine_formats_json(report),
    );
    map.insert(
        "first_sight_sizes".to_owned(),
        first_sight_sizes_json(report),
    );
    map.insert(
        "invisible_shell_read_shapes".to_owned(),
        refused_shapes_json(report),
    );
    map.insert(
        "first_contact".to_owned(),
        super::first_contact::first_contact_json(report),
    );
    map.insert(
        "family_held".to_owned(),
        super::family_held::family_held_json(report),
    );
    map.insert(
        "first_sight_split".to_owned(),
        super::first_sight_split::first_sight_split_json(report),
    );
    map.insert(
        "by_name_split".to_owned(),
        super::by_name_split::by_name_json(report),
    );
    map.insert(
        "post_compaction".to_owned(),
        super::post_compaction::post_compaction_json(report),
    );
    map.insert(
        "pull_answers".to_owned(),
        super::pull_answers::pull_json(report),
    );
    map.insert(
        "no_line_count_split".to_owned(),
        super::no_line_count_split::no_line_count_json(report),
    );
}

/// One inheritance route's tally.
fn inheritance_tally(report: &Report, class: Inheritance) -> Tally {
    report
        .sibling_by_inheritance
        .get(class.index())
        .copied()
        .unwrap_or_default()
}

/// One tally as the document spells it.
fn tally_json(tally: Tally) -> Value {
    json!({ "calls": tally.calls, "tokens": tally.tokens })
}

/// The sibling re-reads by route.
fn inheritance_json(report: &Report) -> Value {
    let mut map = Map::new();
    for class in Inheritance::ALL {
        map.insert(
            class.key().to_owned(),
            tally_json(inheritance_tally(report, class)),
        );
    }
    Value::Object(map)
}

/// Each inherited class's lines and observed pool, and the spawn links.
fn inherited_json(report: &Report) -> Value {
    let mut classes = Map::new();
    for (at, class) in InheritClass::ALL.into_iter().enumerate() {
        let lines = report.inherited_lines.get(at).copied().unwrap_or_default();
        let observed = report
            .inherited_observed
            .get(at)
            .copied()
            .unwrap_or_default();
        classes.insert(
            class.as_str().to_owned(),
            json!({ "lines": lines, "observed": tally_json(observed) }),
        );
    }
    let links = report.spawn_links;
    json!({
        "classes": classes,
        "spawn_links": {
            "spawns": links.spawns,
            "openings": links.openings,
            "exact": links.exact,
            "wrong": links.wrong,
            "unnamed": links.unnamed,
        },
    })
}

/// Where the path facts came from.
fn facts_json(report: &Report) -> Value {
    let facts = report.facts;
    json!({
        "fell_back_to_the_worktree": facts.fell_back_to_the_worktree,
        "mode": if facts.history { "history" } else { "worktree" },
        "resolved_at_a_commit": facts.resolved_at_a_commit,
        "windows_without_a_commit": facts.windows_without_a_commit,
    })
}

/// The machine-format census, every class published whether or not a call
/// landed in it.
fn machine_formats_json(report: &Report) -> Value {
    let mut map = Map::new();
    for format in MachineFormat::ALL {
        let census = report
            .machine_formats
            .get(format.key())
            .copied()
            .unwrap_or_default();
        map.insert(
            format.key().to_owned(),
            json!({
                "all": tally_json(census.all),
                "first_sight": tally_json(census.first_sight),
            }),
        );
    }
    Value::Object(map)
}

/// The first-sight size histograms and the first-contact floor.
fn first_sight_sizes_json(report: &Report) -> Value {
    let sizes = &report.first_sight_sizes;
    let bands = |bands: &[(&str, &str, u64)], tallies: &[Tally]| {
        let mut map = Map::new();
        for ((key, _, _), tally) in bands.iter().zip(tallies) {
            map.insert((*key).to_owned(), tally_json(*tally));
        }
        Value::Object(map)
    };
    json!({
        "by_bytes": bands(&BYTE_BANDS, &sizes.by_bytes),
        "by_lines": bands(&LINE_BANDS, &sizes.by_lines),
        "first_contact_floor": {
            "bytes": FIRST_CONTACT_BYTES,
            "code": tally_json(sizes.first_contact_code),
            "code_extensions": CODE_EXTENSIONS,
            "either_bound": tally_json(sizes.first_contact),
            "lines": FIRST_CONTACT_LINES,
            "markdown": tally_json(sizes.first_contact_markdown),
            "toml": tally_json(sizes.first_contact_toml),
        },
    })
}

/// The refused shell reads by shape, in the table's ranked order.
fn refused_shapes_json(report: &Report) -> Value {
    let pool = report.invisible_shell_named_file.tokens;
    let rows: Vec<Value> = ranked(&report.invisible_shell_shapes)
        .into_iter()
        .map(|(shape, row)| {
            json!({
                "loose_bound": tally_json(row.all),
                "naming_a_work_tree_file": tally_json(row.named),
                "shape": shape.key(),
                "share_of_naming_a_work_tree_file_percent": share_percent(row.named.tokens, pool),
            })
        })
        .collect();
    Value::Array(rows)
}
