//! The read gate's tests, one file per subject. `fixtures` holds the hooks,
//! checkouts, levers and readers every subject shares; the rest are about one
//! thing each — what a call names, what a result delivered, what a decline
//! says, what a window stops holding, and what each decision leaves the
//! detached worker.

mod audit;
mod basis;
mod deliveries;
mod denies;
#[cfg(feature = "doc")]
mod documents;
mod evictions;
mod fixtures;
mod paths;
mod prevented;
mod search_index;
mod shell;
mod turns;
