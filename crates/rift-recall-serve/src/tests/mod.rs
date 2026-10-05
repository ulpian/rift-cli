//! Offline dispatcher tests: the handlers run against an in-memory recall.db +
//! a `MemorySubstrate`, so the whole bounded surface is exercised without a
//! live process on either surviving channel (hook injection or CLI skill).
//!
//! One module per verb, or per concern the verbs share, beside a `fixtures`
//! module holding the seeded projection and the request builders they share.

mod budget;
mod claim;
mod credit;
mod current;
mod doc;
mod exact;
mod fixtures;
mod manifest;
mod pack;
mod pricing;
mod pull;
mod screens;
mod search;
mod surface;
