//! The tests of the response module, one file per subject.
//!
//! The production half stays one file (`../mod.rs`) because the intercept
//! gate names it as the one composer of a hook response and reads it whole;
//! the tests it once carried in a single trailing module are split here by
//! what they are about: the capability table, the declined call, the amended
//! call, the injected text, and each family's shapes written out as bytes.

mod decline;
mod injection;
mod rewrite;
mod shapes;
mod table;
