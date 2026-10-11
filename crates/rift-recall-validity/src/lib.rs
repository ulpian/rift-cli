//! Hash-bound summary store + validity engine + deterministic extractors + the
//! four-state envelope builder. This is the plane that kills the false-memory
//! bug: "file unchanged" never silently means "contents in context".
//!
//! * **Hash-bound key** ([`summary_key`]): the key encodes source + extractor +
//!   policy + redaction versions, so a stale summary has a *different* key and is
//!   structurally unlookupable.
//! * **Validity** ([`resolve`]): re-checks the bound content hash against the
//!   current worktree; only a [`ValidSummary`] (a sealed type no external code can
//!   construct) is returnable — a changed source can never exit as `Valid`.
//! * **Deterministic extractors** ([`Extractor`]): digests scrub/count/outline
//!   (failure/log/diff/file-shape, the last a code outline, [`FileOutline`]);
//!   they never paraphrase, so injected directives are neutralized, and the
//!   output is reproducible (binding-sound).
//! * **Envelope** ([`envelope_for`]): the single way to construct a served
//!   artifact; `known_unchanged` requires same-hash-seen and a source that is
//!   not known-changed.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod trust;

mod envelope;
mod extract;
mod handle;
mod outline;
mod summary;

pub use envelope::{envelope_for, manifest_envelope, Budget, SeenStatus};
pub use extract::{
    extract_and_bind, outline_versions, DiffExtractor, Extractor, ExtractorKind, FailureExtractor,
    FileShapeExtractor, LogExtractor, OUTLINE_REDACTION_VERSION,
};
pub use handle::{resolve_handle, BlobResolver, Resolved};
pub use outline::{
    is_code_path, is_markdown_path, is_outlinable_path, line_span, outline_definitions,
    outline_shape, unnamed_outline, Definition, DefinitionKind, FileOutline, OutlineShape,
};
pub use summary::{
    classify, resolve, summary_key, DigestStore, MemoryDigestStore, SummaryBinding, SummaryStore,
    SummaryVersions, ValidSummary, Validity, WorktreeSnapshot,
};
