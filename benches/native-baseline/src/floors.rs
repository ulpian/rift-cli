//! `benchmarks/floors.toml` — the declaration a published number is judged
//! against.
//!
//! # A floor is a publication convention, not a measurement
//!
//! `floor = min(per-run medians over >= 3 fresh runs at the shipped caps) x 0.5`,
//! rounded down to at most two significant figures. **The x0.5 is not a noise
//! budget** and nobody should read it as one: this project measured
//! run-to-run variance reaching +98% between two identical runs of the same
//! binary (`benchmarks/hook-latency.md` §9.8), which no factor of two absorbs.
//! The halving exists so that a *tracked artifact* can be checked for internal
//! consistency without the check becoming a live performance gate that reds on
//! machine weather. The guard this file feeds is an **artifact-consistency**
//! instrument: it answers "do the published numbers, the published floors and
//! the published provenance agree with each other", never "is the machine fast
//! today".
//!
//! # Why a hand-rolled parser
//!
//! The harness takes **no new dependencies** (it is its own workspace and its
//! lockfile is committed), so this reads the strict subset of TOML the file
//! actually uses: comments, top-level `key = value`, `[[floor]]` array-of-table
//! headers, and scalar or homogeneous-array values. Anything else is a parse
//! **error** rather than a silent skip — a floors file that half-parses is how
//! a guard ends up enforcing a bar nobody wrote.

use std::{collections::BTreeSet, path::Path};

/// The schema version [`Floors`] understands.
pub const FLOORS_SCHEMA: u32 = 1;

/// Where the tracked declaration lives, relative to the repository root.
pub const FLOORS_PATH: &str = "benchmarks/floors.toml";

/// What kind of bar a row states.
///
/// Kept apart because they fail in opposite directions, and the displacement
/// scenario is **never** a throughput floor: an
/// `enable` that does more work per second is not thereby better, and a commit
/// that takes longer is not thereby doing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorKind {
    /// A work-rate floor in the row's own unit: measured >= floor.
    Throughput,
    /// A wall-time ceiling in milliseconds: measured <= budget.
    Latency,
}

/// How much authority a row's number carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorStatus {
    /// A placeholder: the structure exists, the number does not yet. The
    /// derivation runs on a quiet machine with nothing else building or
    /// measuring, and until it does the guard reports the row as PENDING with
    /// the command that would fill it — never as a pass.
    Underived,
    /// Derived from recorded per-run medians by the convention above.
    Derived,
    /// A bar an owner set. Not derived from anything and not re-derivable: the
    /// 100 ms git-latency budget is the owner's, hard, and no measurement
    /// moves it.
    OwnerRatified,
}

/// One scenario's declared bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Floor {
    /// The scenario id, as [`crate::scenario::ScenarioId::as_str`] spells it.
    pub scenario: String,
    /// Throughput floor or latency ceiling.
    pub kind: FloorKind,
    /// Underived placeholder, derived number, or an owner's bar.
    pub status: FloorStatus,
    /// What one unit of this row's work is. Empty for a latency row.
    pub work_unit: String,
    /// The floor, in `work_unit` per second. `None` while underived.
    pub min_units_per_s: Option<f64>,
    /// The ceiling, milliseconds. `None` unless this is a latency row.
    pub budget_ms: Option<f64>,
    /// The per-run medians the floor was derived from, in the run order they
    /// were taken. At least three for a derived row, because one run cannot
    /// distinguish a slow machine from slow code.
    pub derivation_medians: Vec<f64>,
    /// The artifact stems those medians came from, positionally paired with
    /// them.
    ///
    /// **This is what makes "no artifact without provenance may derive a floor"
    /// mechanical rather than a promise.** Without it a floor is three bare
    /// numbers and nothing can tell whether they came from committed runs, from
    /// a dirty tree, or from somebody's memory. The guard resolves each stem,
    /// refuses one it cannot find, and refuses one that is a *record* — an
    /// artifact with no commit provenance, or taken over uncommitted edits.
    pub derivation_artifacts: Vec<String>,
    /// The machine the medians came from. An absolute here moves ~2x with load
    /// under load, so a floor without its machine class is unquotable.
    pub machine_class: String,
    /// The iteration protocol behind each median.
    pub protocol: String,
    /// The product caps in force when the medians were taken. A number taken
    /// under a different cap is about a different product.
    pub caps: String,
    /// Repository paths whose last change the subject commit must contain
    /// (the guard's staleness arm). These are the paths that decide what this
    /// scenario measures; a subject built before the newest of them is quoting
    /// a number about code it does not have.
    pub measured_paths: Vec<String>,
    /// The convention, restated in the file so a reader meets it beside the
    /// number rather than in a ledger they have not read.
    pub derivation: String,
    /// Anything else the row needs said. Carried, never interpreted.
    pub note: String,
}

impl Floor {
    /// Whether this row's number binds a measured value today.
    #[must_use]
    pub fn binds(&self) -> bool {
        matches!(
            self.status,
            FloorStatus::Derived | FloorStatus::OwnerRatified
        )
    }

    /// The floor the recorded medians imply under the convention.
    ///
    /// The guard recomputes this and compares it with what the file says, so a
    /// hand-typed floor that does not follow from its own medians is caught. It
    /// costs nothing and it closes the obvious way a "derived" number stops
    /// being derived.
    #[must_use]
    pub fn implied_floor(&self) -> Option<f64> {
        let min = self
            .derivation_medians
            .iter()
            .copied()
            .filter(|m| m.is_finite() && *m > 0.0)
            .fold(f64::INFINITY, f64::min);
        min.is_finite().then(|| two_sig_figs_down(min * 0.5))
    }

    /// Why this row cannot be accepted as it stands, if it cannot.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        match self.status {
            FloorStatus::Underived => {
                if self.min_units_per_s.is_some() || self.budget_ms.is_some() {
                    return Some(format!(
                        "`{}` is marked underived but carries a number: a placeholder that \
                         states a bar is a bar nobody derived",
                        self.scenario
                    ));
                }
                None
            }
            FloorStatus::OwnerRatified => {
                if self.kind == FloorKind::Latency && self.budget_ms.is_none() {
                    return Some(format!(
                        "`{}` is an owner-ratified latency row with no `budget_ms`",
                        self.scenario
                    ));
                }
                None
            }
            FloorStatus::Derived => {
                // A latency bar is DECIDED, never derived: halving a median would
                // manufacture a ceiling stricter than anything measured, which
                // is the opposite of what the convention does for a rate.
                if self.kind == FloorKind::Latency {
                    return Some(format!(
                        "`{}` is a latency row marked derived: a latency bar is an owner's \
                         decision, not an arithmetic consequence of medians",
                        self.scenario
                    ));
                }
                if self.derivation_medians.len() < 3 {
                    return Some(format!(
                        "`{}` is derived from {} run median(s); at least 3 are required",
                        self.scenario,
                        self.derivation_medians.len()
                    ));
                }
                if self.derivation_artifacts.len() != self.derivation_medians.len() {
                    return Some(format!(
                        "`{}` states {} median(s) and names {} artifact(s): a median with no \
                         artifact behind it is a number nobody can re-derive",
                        self.scenario,
                        self.derivation_medians.len(),
                        self.derivation_artifacts.len()
                    ));
                }
                let (Some(stated), Some(implied)) = (self.min_units_per_s, self.implied_floor())
                else {
                    return Some(format!(
                        "`{}` is derived but states no `min_units_per_s`",
                        self.scenario
                    ));
                };
                // Exact equality would make the file un-hand-editable over a
                // float; a part in a million is far tighter than the two
                // significant figures the convention rounds to, and still
                // survives a decimal round trip.
                if (stated - implied).abs() > implied.abs() * 1e-6 + 1e-9 {
                    return Some(format!(
                        "`{}` states {stated} but its own medians imply {implied} under the \
                         convention (min x 0.5, rounded down to <= 2 significant figures)",
                        self.scenario
                    ));
                }
                None
            }
        }
    }
}

/// Round **down** to at most two significant figures.
///
/// Down, never to-nearest: a floor that rounds up is a bar nothing measured
/// ever cleared.
#[must_use]
pub fn two_sig_figs_down(x: f64) -> f64 {
    if !x.is_finite() || x <= 0.0 {
        return 0.0;
    }
    let exp = x.log10().floor();
    let scale = 10f64.powf(exp - 1.0);
    (x / scale).floor() * scale
}

/// The whole file.
#[derive(Debug, Clone, PartialEq)]
pub struct Floors {
    /// Declaration schema.
    pub schema_version: u32,
    /// One row per scenario that is owed a bar.
    pub floors: Vec<Floor>,
}

impl Floors {
    /// Read and parse the tracked declaration under `repo_root`.
    ///
    /// # Errors
    /// The file could not be read, or does not parse.
    pub fn load(repo_root: &Path) -> std::result::Result<Self, String> {
        let path = repo_root.join(FLOORS_PATH);
        let body = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        Self::parse(&body)
    }

    /// The row for `scenario`, if the file declares one.
    #[must_use]
    pub fn get(&self, scenario: &str) -> Option<&Floor> {
        self.floors.iter().find(|f| f.scenario == scenario)
    }

    /// Parse the strict TOML subset.
    ///
    /// # Errors
    /// Any line that is not a comment, a blank, a `[[floor]]` header or a
    /// `key = value` the subset understands.
    pub fn parse(body: &str) -> std::result::Result<Self, String> {
        let mut schema_version: Option<u32> = None;
        let mut rows: Vec<Vec<(String, Value)>> = Vec::new();
        let mut current: Option<Vec<(String, Value)>> = None;

        for (n, raw) in body.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let at = || format!("line {}", n + 1);
            if line == "[[floor]]" {
                if let Some(row) = current.take() {
                    rows.push(row);
                }
                current = Some(Vec::new());
                continue;
            }
            if line.starts_with('[') {
                return Err(format!("{}: only `[[floor]]` tables are understood", at()));
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("{}: not a `key = value` line", at()))?;
            let key = key.trim().to_owned();
            let value = parse_value(value.trim()).map_err(|e| format!("{}: {e}", at()))?;
            match &mut current {
                Some(row) => {
                    if row.iter().any(|(k, _)| *k == key) {
                        return Err(format!("{}: `{key}` is declared twice in one row", at()));
                    }
                    row.push((key, value));
                }
                None if key == "schema_version" => {
                    schema_version = Some(
                        value
                            .as_int()
                            .ok_or_else(|| format!("{}: schema_version must be an integer", at()))?
                            as u32,
                    );
                }
                None => return Err(format!("{}: `{key}` sits outside any [[floor]]", at())),
            }
        }
        if let Some(row) = current.take() {
            rows.push(row);
        }

        let schema_version =
            schema_version.ok_or_else(|| "no top-level schema_version".to_owned())?;
        if schema_version != FLOORS_SCHEMA {
            return Err(format!(
                "floors schema {schema_version}, this build understands {FLOORS_SCHEMA}"
            ));
        }
        let floors = rows
            .iter()
            .map(|row| floor_from(row))
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut seen = BTreeSet::new();
        for f in &floors {
            if !seen.insert(f.scenario.clone()) {
                return Err(format!("`{}` is declared twice", f.scenario));
            }
        }
        Ok(Self {
            schema_version,
            floors,
        })
    }
}

/// One parsed scalar or array.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Strings(Vec<String>),
    Floats(Vec<f64>),
    EmptyArray,
}

impl Value {
    fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            Self::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    fn as_strings(&self) -> Option<Vec<String>> {
        match self {
            Self::Strings(v) => Some(v.clone()),
            Self::EmptyArray => Some(Vec::new()),
            _ => None,
        }
    }

    fn as_floats(&self) -> Option<Vec<f64>> {
        match self {
            Self::Floats(v) => Some(v.clone()),
            Self::EmptyArray => Some(Vec::new()),
            _ => None,
        }
    }
}

fn strip_comment(line: &str) -> &str {
    // A `#` inside a quoted string is content. The subset has no escapes, so
    // counting quotes ahead of the `#` is exact.
    let bytes = line.as_bytes();
    let mut quoted = false;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'"' => quoted = !quoted,
            b'#' if !quoted => return line.get(..i).unwrap_or(""),
            _ => {}
        }
    }
    line
}

fn parse_value(text: &str) -> std::result::Result<Value, String> {
    if let Some(rest) = text.strip_prefix('[') {
        let inner = rest.strip_suffix(']').ok_or_else(|| {
            "unterminated array (one line per value, no continuations)".to_owned()
        })?;
        let items: Vec<&str> = inner
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if items.is_empty() {
            return Ok(Value::EmptyArray);
        }
        if items.iter().all(|i| i.starts_with('"')) {
            let mut out = Vec::new();
            for item in items {
                out.push(unquote(item)?.to_owned());
            }
            return Ok(Value::Strings(out));
        }
        let mut out = Vec::new();
        for item in items {
            out.push(
                item.parse::<f64>()
                    .map_err(|_| format!("`{item}` is not a number"))?,
            );
        }
        return Ok(Value::Floats(out));
    }
    if text.starts_with('"') {
        return Ok(Value::Str(unquote(text)?.to_owned()));
    }
    match text {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    if let Ok(i) = text.parse::<i64>() {
        return Ok(Value::Int(i));
    }
    text.parse::<f64>()
        .map(Value::Float)
        .map_err(|_| format!("`{text}` is not a value this subset understands"))
}

fn unquote(text: &str) -> std::result::Result<&str, String> {
    text.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .ok_or_else(|| format!("`{text}` is not a quoted string"))
}

fn floor_from(row: &[(String, Value)]) -> std::result::Result<Floor, String> {
    let get = |key: &str| row.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let required_str = |key: &str| -> std::result::Result<String, String> {
        get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("a [[floor]] row is missing the string `{key}`"))
    };
    let optional_str = |key: &str| get(key).and_then(Value::as_str).unwrap_or("").to_owned();

    let scenario = required_str("scenario")?;
    let kind = match required_str("kind")?.as_str() {
        "throughput" => FloorKind::Throughput,
        "latency" => FloorKind::Latency,
        other => return Err(format!("`{scenario}`: unknown kind `{other}`")),
    };
    let status = match required_str("status")?.as_str() {
        "underived" => FloorStatus::Underived,
        "derived" => FloorStatus::Derived,
        "owner-ratified" => FloorStatus::OwnerRatified,
        other => return Err(format!("`{scenario}`: unknown status `{other}`")),
    };
    let measured_paths = get("measured_paths")
        .and_then(Value::as_strings)
        .ok_or_else(|| format!("`{scenario}`: measured_paths must be an array of strings"))?;
    if measured_paths.is_empty() {
        return Err(format!(
            "`{scenario}`: measured_paths is empty, so the staleness arm has nothing to read — \
             a scenario that declares no measured paths can never be found stale"
        ));
    }
    Ok(Floor {
        scenario,
        kind,
        status,
        work_unit: optional_str("work_unit"),
        min_units_per_s: get("min_units_per_s").and_then(Value::as_f64),
        budget_ms: get("budget_ms").and_then(Value::as_f64),
        derivation_medians: get("derivation_medians")
            .and_then(Value::as_floats)
            .unwrap_or_default(),
        derivation_artifacts: get("derivation_artifacts")
            .and_then(Value::as_strings)
            .unwrap_or_default(),
        machine_class: required_str("machine_class")?,
        protocol: required_str("protocol")?,
        caps: required_str("caps")?,
        measured_paths,
        derivation: required_str("derivation")?,
        note: optional_str("note"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
schema_version = 1

[[floor]]
scenario = "verify-throughput"
kind = "throughput"
status = "underived"
work_unit = "checkpoints"
machine_class = "i5-8279U / macOS 24.6.0 / 8 logical CPUs"
protocol = "1 warmup + 3 measured iterations"
caps = "MAX_TOTAL_BYTES=8192"
measured_paths = ["bins/rift/src/verify.rs"]
derivation_medians = []
derivation_artifacts = []
derivation = "min x 0.5, rounded down"
"#;

    #[test]
    fn the_tracked_file_parses_and_declares_every_owed_scenario() {
        let root = crate::leg::default_repo_root();
        let floors = Floors::load(&root).expect("benchmarks/floors.toml must parse");
        assert_eq!(floors.schema_version, FLOORS_SCHEMA);
        // All four rows are owner-ratified today. The three throughput bars were
        // derived by the halving convention, but the history holding their
        // subject commits was collapsed to a fresh root and the medians stopped
        // being re-derivable by anyone, so the owner ratified the same numbers
        // as set bars and the derivation lists were emptied. Re-deriving a row
        // means three fresh runs, the medians and stems written back, and
        // `derived` again — which this table would then be updated to expect.
        for (scenario, expected_status) in [
            ("condense", FloorStatus::OwnerRatified),
            ("verify-throughput", FloorStatus::OwnerRatified),
            ("import-throughput", FloorStatus::OwnerRatified),
            ("enable-displacement", FloorStatus::OwnerRatified),
        ] {
            let row = floors
                .get(scenario)
                .unwrap_or_else(|| panic!("`{scenario}` is owed a floor row"));
            assert_eq!(row.refusal(), None, "{scenario}");
            assert_eq!(
                row.status, expected_status,
                "{scenario} must carry the authority its row claims"
            );
            // Every declared path must exist, or the staleness arm reads a
            // history that never mentions it and silently answers "fresh".
            for path in &row.measured_paths {
                assert!(
                    root.join(path).exists(),
                    "`{scenario}` declares `{path}`, which is not in the tree"
                );
            }
            match row.kind {
                // The convention travels with the number it produced.
                FloorKind::Throughput => assert!(
                    row.derivation.contains("0.5") && row.derivation.contains("NOT a noise budget"),
                    "{scenario}"
                ),
                // And a latency row states, in the same field, that it is not
                // one — this scenario must never be read as a rate.
                FloorKind::Latency => assert!(
                    row.derivation.contains("NOT a derived floor")
                        && row.derivation.contains("NEVER a throughput floor"),
                    "{scenario}"
                ),
            }
        }
        assert_eq!(
            floors.floors.len(),
            4,
            "exactly the owed set, no decoration"
        );
    }

    #[test]
    fn a_minimal_row_round_trips_through_the_subset() {
        let floors = Floors::parse(MINIMAL).unwrap();
        assert_eq!(floors.floors.len(), 1);
        let row = &floors.floors[0];
        assert_eq!(row.scenario, "verify-throughput");
        assert_eq!(row.kind, FloorKind::Throughput);
        assert_eq!(row.status, FloorStatus::Underived);
        assert_eq!(row.min_units_per_s, None);
        assert!(!row.binds(), "a placeholder binds nothing");
        assert_eq!(row.refusal(), None);
    }

    /// The convention, applied to its own inputs. Down, never to nearest: a
    /// floor that rounds up is a bar nothing measured ever cleared.
    #[test]
    fn the_floor_is_half_the_smallest_median_rounded_down_to_two_figures() {
        assert!((two_sig_figs_down(1234.0) - 1200.0).abs() < 1e-9);
        assert!((two_sig_figs_down(87.6) - 87.0).abs() < 1e-9);
        assert!((two_sig_figs_down(8.76) - 8.7).abs() < 1e-9);
        assert!((two_sig_figs_down(0.0376) - 0.037).abs() < 1e-9);
        assert!((two_sig_figs_down(99.999) - 99.0).abs() < 1e-9);
        // Degenerate inputs collapse to a floor nothing can violate rather
        // than to a NaN that every comparison silently passes.
        for x in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            assert!(two_sig_figs_down(x).abs() < f64::EPSILON, "{x}");
        }

        let mut row = Floors::parse(MINIMAL).unwrap().floors.remove(0);
        row.derivation_medians = vec![41.0, 38.0, 44.0];
        // min 38.0, halved 19.0, already two figures.
        assert!((row.implied_floor().unwrap() - 19.0).abs() < 1e-9);
    }

    #[test]
    fn a_derived_row_must_follow_from_its_own_medians_and_from_three_of_them() {
        let mut row = Floors::parse(MINIMAL).unwrap().floors.remove(0);
        row.status = FloorStatus::Derived;
        row.derivation_medians = vec![41.0, 38.0];
        row.derivation_artifacts = vec!["a".to_owned(), "b".to_owned()];
        row.min_units_per_s = Some(19.0);
        assert!(row.refusal().unwrap().contains("at least 3"));

        row.derivation_medians = vec![41.0, 38.0, 44.0];
        // A median with no artifact behind it is a number nobody can re-derive.
        assert!(row.refusal().unwrap().contains("names 2 artifact(s)"));
        row.derivation_artifacts.push("c".to_owned());
        assert_eq!(row.refusal(), None, "38 x 0.5 = 19");
        assert!(row.binds());

        // The hand-typed floor that stopped following from its inputs.
        row.min_units_per_s = Some(30.0);
        let refusal = row.refusal().unwrap();
        assert!(refusal.contains("imply 19"), "{refusal}");

        row.min_units_per_s = None;
        assert!(row.refusal().unwrap().contains("states no"));
    }

    #[test]
    fn a_placeholder_that_states_a_bar_is_refused() {
        let mut row = Floors::parse(MINIMAL).unwrap().floors.remove(0);
        row.min_units_per_s = Some(19.0);
        let refusal = row.refusal().unwrap();
        assert!(
            refusal.contains("marked underived but carries a number"),
            "{refusal}"
        );
    }

    #[test]
    fn the_parser_refuses_rather_than_half_reading() {
        for (body, needle) in [
            ("schema_version = 1\n[other]\n", "only `[[floor]]`"),
            ("schema_version = 2\n", "this build understands"),
            ("scenario = \"x\"\n", "outside any [[floor]]"),
            ("schema_version = 1\n[[floor]]\nnot a pair\n", "key = value"),
            (
                "schema_version = 1\n[[floor]]\nscenario = \"x\"\nscenario = \"y\"\n",
                "declared twice in one row",
            ),
            ("[[floor]]\n", "no top-level schema_version"),
        ] {
            let err = Floors::parse(body).unwrap_err();
            assert!(err.contains(needle), "{body:?} -> {err}");
        }
    }

    /// A row with no measured paths can never be found stale, which would make
    /// the staleness arm silently vacuous for exactly that scenario.
    #[test]
    fn a_row_with_no_measured_paths_is_refused_at_parse_time() {
        let body = MINIMAL.replace(
            "measured_paths = [\"bins/rift/src/verify.rs\"]",
            "measured_paths = []",
        );
        let err = Floors::parse(&body).unwrap_err();
        assert!(err.contains("staleness arm has nothing to read"), "{err}");
    }

    #[test]
    fn comments_and_hashes_inside_strings_are_told_apart() {
        let body = MINIMAL.replace(
            "caps = \"MAX_TOTAL_BYTES=8192\"",
            "caps = \"MAX_TOTAL_BYTES=8192 # not a comment\" # a comment",
        );
        let floors = Floors::parse(&body).unwrap();
        assert_eq!(
            floors.floors[0].caps,
            "MAX_TOTAL_BYTES=8192 # not a comment"
        );
    }

    #[test]
    fn two_rows_for_one_scenario_are_refused() {
        let body = format!("{MINIMAL}{}", MINIMAL.replace("schema_version = 1", ""));
        let err = Floors::parse(&body).unwrap_err();
        assert!(err.contains("declared twice"), "{err}");
    }
}
