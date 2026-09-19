//! The tests of the inclusion module, one file per subject.
//!
//! The production half stays one file (`../mod.rs`) because the source screens
//! in `screens` read it whole; the tests it once carried in a single trailing
//! module are split here by what they are about, with the fixtures they share
//! in `fixtures`.

mod fixtures;

mod budgets;
mod capture;
mod doors;
mod keying;
#[cfg(unix)]
mod links;
mod properties;
mod screens;
mod spelling;
