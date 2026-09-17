//! Every client family Rift installs into must have a declared turn dialect.
//!
//! The injected-manifest guard (`rift_recall_validity::trust`) refuses spans
//! that read as a turn boundary. Which spellings *are* turn boundaries is the
//! one axis of that guard which cannot be closed from inside the crate: the
//! pushed alphabet is finite and declared there, but the set of words a model
//! reads as a role is decided by whichever client the manifest is injected
//! into. Three rounds of remediation kept the word list by hand and the fourth
//! found `human:` missing — the turn label of the very client family this
//! repository is captured by.
//!
//! So the obligation is bound to the list that changes when Rift's world does:
//! [`ClientFamily::ALL`], the families the installer writes hooks for. A family
//! added without a dialect row fails here, and a dialect row naming a family
//! that does not exist fails here too — the correspondence is checked in both
//! directions, because only one of them catches a stale row.
//!
//! This lives in `bins/rift` rather than in either crate because it is the one
//! crate that already depends on both, so the check costs no new crate edge.
//! It cannot check that the *words* are right — no in-tree oracle knows a
//! vendor's role names — it checks that the decision was made.

use rift_agent_install::ClientFamily;
use rift_recall_serve::{RoleDialect, ROLE_MARKERS};

#[test]
fn every_installed_client_family_has_a_declared_turn_dialect() {
    for family in ClientFamily::ALL {
        let rows: Vec<&str> = ROLE_MARKERS
            .iter()
            .filter(|m| m.taught_by.contains(&family.token()))
            .map(|m| m.spelling)
            .collect();
        assert!(
            !rows.is_empty(),
            "{} is installed by `rift` but no ROLE_MARKERS row names it: a manifest \
             injected into that client would be judged against a vocabulary that \
             does not include its turn boundaries",
            family.token()
        );
        // A family's own turn labels are what a manifest is judged against on
        // the push side, and only the transcript dialect is reachable there.
        for row in ROLE_MARKERS
            .iter()
            .filter(|m| m.taught_by.contains(&family.token()))
        {
            assert_eq!(
                row.dialect,
                RoleDialect::Transcript,
                "{:?} is claimed by {} but is spelled in the template dialect, which \
                 the push side refuses by charset and never by vocabulary",
                row.spelling,
                family.token()
            );
        }
    }
}

#[test]
fn no_dialect_row_claims_a_client_that_does_not_exist() {
    let known: Vec<&str> = ClientFamily::ALL.iter().map(|f| f.token()).collect();
    for row in ROLE_MARKERS {
        for token in row.taught_by {
            assert!(
                known.contains(token),
                "{:?} is carried for client {token:?}, which `rift` does not install: \
                 either the family was removed and this row is stale, or the token is \
                 misspelled and the coverage check above is passing vacuously",
                row.spelling
            );
        }
    }
}
