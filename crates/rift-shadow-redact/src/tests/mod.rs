//! The crate's tests, one file per subject. `fixtures` holds the helpers the
//! others share; `census` is the acceptance bar's own instrument and keeps its
//! test's name, which the bar invokes by filter.

mod census;
mod corpus;
mod detectors;
mod entropy;
mod fast_paths;
mod fixtures;
mod marks;
mod mint_guard;
mod packs;
mod placeholders;
mod props;
mod reference;
mod shapes;
