//! What the reads are made of: the machine-format census and the sizes of the
//! files a window saw for the first time.
//!
//! Neither measures this build. Both size a lever a later build may add — a
//! lens over machine-written files, and a first-contact floor for large ones —
//! so the pool each would work on is known before either is argued for. They
//! are a module of their own because their classes are decided by the path and
//! the file's size alone, never by the bytes a result carried, and keeping that
//! rule in one place is what makes two runs comparable.

use super::report::{Report, Tally};

/// A file shape a machine writes and a model rarely needs whole.
///
/// Decided by the path's shape only, so the class is deterministic and can
/// never depend on what a result happened to contain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MachineFormat {
    /// A package manager's resolved dependency list.
    Lockfile,
    /// A minified script or stylesheet.
    Minified,
    /// A notebook document.
    Notebook,
    /// A vector image.
    Svg,
    /// Comma- or tab-separated values.
    Tabular,
}

/// A machine-format class's calls: every repository-content read of it, and
/// the part of those that was a first sight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CensusRow {
    /// Every repository-content read call whose path has the shape.
    pub all: Tally,
    /// Of those, the calls in the first-sight bucket.
    pub first_sight: Tally,
}

/// The first-sight calls, by the size of the file they read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirstSightSizes {
    /// One tally per band of `LINE_BANDS`, in its order.
    pub by_lines: [Tally; 5],
    /// One tally per band of `BYTE_BANDS`, in its order.
    pub by_bytes: [Tally; 4],
    /// The calls meeting either first-contact bound.
    pub first_contact: Tally,
    /// Of those, the ones reading a file with an extension in
    /// `CODE_EXTENSIONS`.
    pub first_contact_code: Tally,
    /// Of those, the ones reading a Markdown file.
    pub first_contact_markdown: Tally,
    /// Of those, the ones reading a TOML file.
    pub first_contact_toml: Tally,
}

/// The file names a lockfile is spelled with, compared exactly.
const LOCKFILES: [&str; 12] = [
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Pipfile.lock",
    "Gemfile.lock",
    "composer.lock",
    "go.sum",
    "flake.lock",
    "uv.lock",
];

/// The bands the line histogram is cut at: a key, a label, and the smallest
/// line count in the band. Each band runs to the next one's lower bound.
pub(super) const LINE_BANDS: [(&str, &str, u64); 5] = [
    ("under_100", "< 100 lines", 0),
    ("100_to_399", "100–399 lines", 100),
    ("400_to_999", "400–999 lines", 400),
    ("1000_to_1499", "1,000–1,499 lines", 1_000),
    ("1500_and_over", "≥ 1,500 lines", 1_500),
];

/// The bands the byte histogram is cut at, in the same shape.
pub(super) const BYTE_BANDS: [(&str, &str, u64); 4] = [
    ("under_4_kib", "< 4 KiB", 0),
    ("4_to_16_kib", "4–16 KiB", 4 * 1024),
    ("16_to_64_kib", "16–64 KiB", 16 * 1024),
    ("64_kib_and_over", "≥ 64 KiB", 64 * 1024),
];

/// The first-contact floor, in lines: a first sight of a file this long or
/// longer meets it.
pub(super) const FIRST_CONTACT_LINES: u64 = 400;

/// The first-contact floor, in bytes: either bound is enough.
pub(super) const FIRST_CONTACT_BYTES: u64 = 16_384;

/// The extensions the census counts as code, compared without regard to case.
///
/// A file is code when a compiler or an interpreter reads it. Markdown and TOML
/// are left out on purpose and published on rows of their own, because whether
/// they join an outline is the question the either-bound line is there to
/// answer.
pub(super) const CODE_EXTENSIONS: [&str; 30] = [
    "bash", "c", "cc", "cjs", "cpp", "cs", "css", "go", "h", "hpp", "java", "js", "jsx", "kt",
    "lua", "m", "mjs", "php", "py", "rb", "rs", "scala", "scss", "sh", "sql", "swift", "ts", "tsx",
    "zig", "zsh",
];

impl MachineFormat {
    /// Every class, in the order the table prints them.
    pub const ALL: [Self; 5] = [
        Self::Lockfile,
        Self::Minified,
        Self::Notebook,
        Self::Svg,
        Self::Tabular,
    ];

    /// The key this class is published under, and the only name of it that
    /// reaches an output line.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Lockfile => "lockfile",
            Self::Minified => "minified",
            Self::Notebook => "notebook",
            Self::Svg => "svg",
            Self::Tabular => "tabular",
        }
    }

    /// The class a path's shape puts it in, if any.
    pub(super) fn of(path: &str) -> Option<Self> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if LOCKFILES.contains(&name) {
            return Some(Self::Lockfile);
        }
        let lower = name.to_ascii_lowercase();
        let ends = |suffixes: &[&str]| suffixes.iter().any(|suffix| lower.ends_with(suffix));
        if ends(&[".min.js", ".min.css", ".min.mjs"]) {
            return Some(Self::Minified);
        }
        if ends(&[".ipynb"]) {
            return Some(Self::Notebook);
        }
        if ends(&[".svg"]) {
            return Some(Self::Svg);
        }
        if ends(&[".csv", ".tsv"]) {
            return Some(Self::Tabular);
        }
        None
    }
}

/// Count one repository-content read call in the class of the first of its
/// paths that has a machine-format shape, if one does.
///
/// One class per call, like one bucket per call, so the rows add up to no more
/// than the read channels.
pub(super) fn count_machine_format<'a>(
    report: &mut Report,
    paths: impl IntoIterator<Item = &'a str>,
    tokens: u64,
) -> Option<MachineFormat> {
    let format = paths.into_iter().find_map(MachineFormat::of)?;
    report
        .machine_formats
        .entry(format.key())
        .or_default()
        .all
        .add(tokens);
    Some(format)
}

/// Count a first-sight call in its machine-format class.
pub(super) fn count_machine_format_first_sight(
    report: &mut Report,
    format: MachineFormat,
    tokens: u64,
) {
    report
        .machine_formats
        .entry(format.key())
        .or_default()
        .first_sight
        .add(tokens);
}

/// Count one first-sight call by the size of the file it read.
pub(super) fn count_first_sight_size(
    sizes: &mut FirstSightSizes,
    path: &str,
    lines: u64,
    bytes: u64,
    tokens: u64,
) {
    if let Some(tally) = sizes.by_lines.get_mut(band_of(&LINE_BANDS, lines)) {
        tally.add(tokens);
    }
    if let Some(tally) = sizes.by_bytes.get_mut(band_of(&BYTE_BANDS, bytes)) {
        tally.add(tokens);
    }
    if lines < FIRST_CONTACT_LINES && bytes < FIRST_CONTACT_BYTES {
        return;
    }
    sizes.first_contact.add(tokens);
    let extension = extension_of(path);
    if CODE_EXTENSIONS.contains(&extension.as_str()) {
        sizes.first_contact_code.add(tokens);
    }
    match extension.as_str() {
        "md" => sizes.first_contact_markdown.add(tokens),
        "toml" => sizes.first_contact_toml.add(tokens),
        _ => (),
    }
}

/// Which band a value falls in: the last whose lower bound it reaches.
fn band_of(bands: &[(&str, &str, u64)], value: u64) -> usize {
    bands
        .iter()
        .rposition(|(_, _, lower)| value >= *lower)
        .unwrap_or_default()
}

/// A path's extension, lowercased, or empty when it has none.
fn extension_of(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_ascii_lowercase(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{band_of, count_first_sight_size, FirstSightSizes, MachineFormat, LINE_BANDS};

    #[test]
    fn a_machine_format_is_decided_by_the_path_shape_alone() {
        let of = |path: &str| MachineFormat::of(path).map(MachineFormat::key);
        assert_eq!(of("a/Cargo.lock"), Some("lockfile"));
        assert_eq!(of("go.sum"), Some("lockfile"));
        assert_eq!(of("web/app.MIN.js"), Some("minified"));
        assert_eq!(of("nb/a.ipynb"), Some("notebook"));
        assert_eq!(of("logo.svg"), Some("svg"));
        assert_eq!(of("t.tsv"), Some("tabular"));
        assert_eq!(of("src/lib.rs"), None);
        assert_eq!(of("cargo.lock.md"), None);
    }

    #[test]
    fn a_band_runs_to_the_next_ones_lower_bound() {
        assert_eq!(band_of(&LINE_BANDS, 0), 0);
        assert_eq!(band_of(&LINE_BANDS, 99), 0);
        assert_eq!(band_of(&LINE_BANDS, 100), 1);
        assert_eq!(band_of(&LINE_BANDS, 1_499), 3);
        assert_eq!(band_of(&LINE_BANDS, 90_000), 4);
    }

    #[test]
    fn either_bound_meets_the_first_contact_floor() {
        let mut sizes = FirstSightSizes::default();
        count_first_sight_size(&mut sizes, "a.md", 400, 10, 1);
        count_first_sight_size(&mut sizes, "b.toml", 10, 16_384, 1);
        count_first_sight_size(&mut sizes, "c.rs", 399, 16_383, 1);
        count_first_sight_size(&mut sizes, ".rs", 900, 10, 1);
        assert_eq!(sizes.first_contact.calls, 3);
        assert_eq!(sizes.first_contact_markdown.calls, 1);
        assert_eq!(sizes.first_contact_toml.calls, 1);
        assert_eq!(
            sizes.first_contact_code.calls, 0,
            "a dotfile has no extension"
        );
    }
}
