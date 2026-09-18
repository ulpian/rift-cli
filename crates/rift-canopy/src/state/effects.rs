//! What one reduction hands back to the loop: the [`Flow`] and its
//! [`Effects`] — bus requests, insert-only persistence, delegated dispatches.
//!
//! Its own module because the reducer never performs I/O; everything it wants
//! done is spelled here as data, and the loop and the binary read these types
//! as their contract.

use crate::dispatch::DispatchRequest;

/// The outcome of one reduction.
#[derive(Debug)]
// Built and consumed once per event, never stored or moved through a channel,
// so the size difference costs nothing; boxing `Effects` would put one
// allocation on the idle `Tick` path that the ~0% CPU invariant keeps free.
#[allow(clippy::large_enum_variant)]
pub enum Flow {
    /// Stay in the loop; perform these side effects.
    Continue(Effects),
    /// Exit the dashboard.
    Quit,
}

/// Side effects the loop performs *after* the (pure) reducer returns. The reducer
/// never does I/O; all of it is expressed here.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Fresh-row requests for the data bus.
    pub data_requests: Vec<DataRequest>,
    /// Append-only persistence (projects, layouts, and profiles).
    pub persist: Vec<PersistOp>,
    /// Delegated CLI requests for the injected dispatcher, each
    /// with its resolved target repository (the selected project's path at
    /// emit time — never process cwd).
    pub dispatches: Vec<(DispatchRequest, String)>,
    /// Repository Project Settings lifecycle actions for the injected manager.
    pub project_settings_actions: Vec<crate::ProjectSettingsAction>,
    /// A project path whose saved layout the loop should load and apply:
    /// set on a selection change and at startup. The reducer emits the request;
    /// the loop performs the read through the injected layout loader.
    pub load_layout: Option<String>,
    /// Reload the profile plane after a profile persist op:
    /// names, active id, membership, selection, and arrangement all reload
    /// through the injected profile loader. The reducer only raises the flag.
    pub reload_profiles: bool,
    /// A profile whose member view + the catalogue the loop should load
    /// through the loader seam (the members editor): set
    /// on entry and after every membership op, so the editor's rows always
    /// re-read the store.
    pub load_membership: Option<String>,
    /// A NON-active profile whose theme overrides the loop should load for
    /// the colour picker's swatches: set when the picker opens
    /// for a profile whose overrides the state can't derive purely.
    pub load_picker_theme: Option<String>,
    /// A directory whose immediate children the loop should list through the
    /// injected lister (the destination explorer) — the reducer only *requests*. The reply
    /// lands on `App.explorer` in the same iteration, before the next frame, so
    /// navigation never shows a loading state.
    pub list_dir: Option<std::path::PathBuf>,
}

impl Effects {
    /// `true` if there is nothing to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data_requests.is_empty()
            && self.persist.is_empty()
            && self.dispatches.is_empty()
            && self.project_settings_actions.is_empty()
            && self.load_layout.is_none()
            && !self.reload_profiles
            && self.load_membership.is_none()
            && self.load_picker_theme.is_none()
            && self.list_dir.is_none()
    }
}

/// A scoped read-request the reducer emits for the live data bus. The loop
/// forwards these to the bus, which reads the projection off-thread and feeds the
/// result back as an [`Event::ProjectData`](crate::state::Event::ProjectData).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataRequest {
    /// Make this project the active (foreground) one; the bus prioritizes it and
    /// observes it for changes. `None` clears the active project.
    SetActive(Option<String>),
    /// Re-read this project's data now (by project key — the canonical path).
    Refresh(String),
    /// Compute the Recall feed over this period from now on, and recompute the
    /// active project's now.
    ///
    /// The window is worker state rather than a parameter on `Refresh` because
    /// every *other* wake — a watermark change, a filesystem doorbell, the idle
    /// re-stat — recomputes the feed too, and a window that only survived until
    /// the next such wake would silently snap back to the default while the
    /// button the viewer pressed stayed lit.
    SetRecallWindow(crate::col3::RecallWindow),
    /// A request routed exclusively through the read-only viewer worker.
    Viewer(crate::recall_viewer::ViewerRequest),
}

/// An **insert-only** persistence operation, keyed by the project's canonical
/// `path` (Canopy's stable natural key — the same key status matching uses). There
/// is deliberately no `Update`/`Delete` variant: a removal, relabel, reorder, or
/// change is a new, superseding insert into an append-only log, which upholds the
/// immutable model. The bin resolves `path` → the stored
/// `project_id` when it writes the event, so ids never leak into Canopy. Profiles
/// are keyed by their stable `profile` id, which Canopy *does* hold —
/// names are mutable display data served through the profile loader seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistOp {
    /// Register a project at the given path.
    AddProject {
        /// The (user-entered) project path to register.
        path: String,
    },
    /// Hide a project (a superseding `removed` event; the log/history is kept).
    RemoveProject {
        /// The path of the project to hide.
        path: String,
    },
    /// Change a project's display label (a superseding `relabeled` event).
    RelabelProject {
        /// The path of the project to relabel.
        path: String,
        /// The new label.
        label: String,
    },
    /// Move a project up (`true`) or down (`false`) in the list (a superseding
    /// `reordered` event; the bin computes the new order keys).
    ReorderProject {
        /// The path of the project to move.
        path: String,
        /// Move up when `true`, down when `false`.
        up: bool,
    },
    /// Record the project's dashboard arrangement (a superseding `layout` event;
    /// per-project layout persistence, profile-scoped per the profile plane). `Some` is an encoded
    /// [`crate::layout_model::LayoutModel`]; `None` is a reset to the default —
    /// a superseding event, never a delete.
    SetLayout {
        /// The profile the arrangement belongs to (all new writes are
        /// profile-scoped; the legacy per-project table is frozen for writes).
        profile: String,
        /// The path of the project the layout belongs to.
        path: String,
        /// The encoded arrangement, or `None` to reset to the default.
        layout: Option<String>,
    },
    /// Mint a new profile: a `created` event, plus explicit
    /// membership `added` events copying the creating (active) profile's
    /// effective project list at creation time.
    CreateProfile {
        /// The validated display name (trimmed, non-empty, ≤ 24, unique).
        name: String,
    },
    /// Duplicate a profile: a full clone of the source's
    /// effective state at clone time — membership, per-project arrangements,
    /// and the `Some` theme slots — as explicit events under a new name.
    /// Never writes `selected` (cloning is not switching).
    DuplicateProfile {
        /// The stable id of the profile to clone.
        source: String,
        /// The validated new display name (trimmed, non-empty, ≤ 24, unique).
        name: String,
    },
    /// Rename a profile (a superseding `renamed` event).
    RenameProfile {
        /// The stable id of the profile to rename.
        profile: String,
        /// The validated new display name.
        name: String,
    },
    /// Remove a profile (a superseding `removed` event, never a row deletion).
    /// Removing the active or the last live profile is refused in the reducer
    /// and again in the store (the profile plane).
    RemoveProfile {
        /// The stable id of the profile to remove.
        profile: String,
    },
    /// Make a profile the one the dashboard shows (a superseding `selected`
    /// event; the latest fold names the active profile).
    SelectProfile {
        /// The stable id of the profile to activate.
        profile: String,
    },
    /// Include a catalogue project in a profile's membership view (this
    /// profile only; `canopy_project_event` stays the registration source of
    /// truth).
    AddProfileProject {
        /// The stable id of the profile whose view gains the project.
        profile: String,
        /// The catalogue project's canonical path.
        path: String,
    },
    /// Hide a project from a profile's membership view (this profile only —
    /// other profiles and the global catalogue are untouched).
    RemoveProfileProject {
        /// The stable id of the profile whose view loses the project.
        profile: String,
        /// The catalogue project's canonical path.
        path: String,
    },
    /// Move a project up (`true`) or down (`false`) within a profile's view (a
    /// superseding `reordered` event; the bin computes the new order keys).
    ReorderProfileProject {
        /// The stable id of the profile whose view is reordered.
        profile: String,
        /// The catalogue project's canonical path.
        path: String,
        /// Move up when `true`, down when `false`.
        up: bool,
    },
    /// Record one theme-slot override for a profile (a superseding event per
    /// (profile, slot) — the runtime surface theme). `colour` is resolved `#RRGGBB` hex; `None`
    /// is a reset to the Rift-dark default — a superseding event, never a
    /// delete. The slot carries [`crate::theme::ThemeSlot`]'s storage spelling.
    SetProfileTheme {
        /// The stable id of the profile the override belongs to.
        profile: String,
        /// The slot key (`background`/`border`/`button`/`graph`/`text`).
        slot: String,
        /// The resolved hex colour, or `None` to reset the slot.
        colour: Option<String>,
    },
    /// Pin — or with `None` unpin — the startup profile: one
    /// superseding latest-wins event. Launch itself only reads the fold.
    SetStartupProfile {
        /// The profile launch should open on, or `None` to follow
        /// last-selected (today's behaviour).
        profile: Option<String>,
    },
}

/// Wrap an optional persist op into a `Flow::Continue` (no op ⇒ no effect).
pub(super) fn persist_flow(op: Option<PersistOp>) -> Flow {
    Flow::Continue(Effects {
        persist: op.into_iter().collect(),
        ..Effects::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One of every persist op. Adding a variant without listing it here
    /// leaves the exhaustive match below incomplete, so the compiler asks.
    fn every_persist_op() -> [PersistOp; 15] {
        [
            PersistOp::AddProject {
                path: String::new(),
            },
            PersistOp::RemoveProject {
                path: String::new(),
            },
            PersistOp::RelabelProject {
                path: String::new(),
                label: String::new(),
            },
            PersistOp::ReorderProject {
                path: String::new(),
                up: true,
            },
            PersistOp::SetLayout {
                profile: String::new(),
                path: String::new(),
                layout: None,
            },
            PersistOp::CreateProfile {
                name: String::new(),
            },
            PersistOp::DuplicateProfile {
                source: String::new(),
                name: String::new(),
            },
            PersistOp::RenameProfile {
                profile: String::new(),
                name: String::new(),
            },
            PersistOp::RemoveProfile {
                profile: String::new(),
            },
            PersistOp::SelectProfile {
                profile: String::new(),
            },
            PersistOp::AddProfileProject {
                profile: String::new(),
                path: String::new(),
            },
            PersistOp::RemoveProfileProject {
                profile: String::new(),
                path: String::new(),
            },
            PersistOp::ReorderProfileProject {
                profile: String::new(),
                path: String::new(),
                up: true,
            },
            PersistOp::SetProfileTheme {
                profile: String::new(),
                slot: String::new(),
                colour: None,
            },
            PersistOp::SetStartupProfile { profile: None },
        ]
    }

    #[test]
    fn persist_ops_are_insert_only() {
        // Exhaustive match: every PersistOp is an insert (add/remove/relabel/
        // reorder/layout and the profile ops are all superseding
        // appends). If anyone adds an Update/Delete variant, this stops
        // compiling — the immutability guard.
        for op in every_persist_op() {
            match op {
                PersistOp::AddProject { .. }
                | PersistOp::RemoveProject { .. }
                | PersistOp::RelabelProject { .. }
                | PersistOp::ReorderProject { .. }
                | PersistOp::SetLayout { .. }
                | PersistOp::CreateProfile { .. }
                | PersistOp::DuplicateProfile { .. }
                | PersistOp::RenameProfile { .. }
                | PersistOp::RemoveProfile { .. }
                | PersistOp::SelectProfile { .. }
                | PersistOp::AddProfileProject { .. }
                | PersistOp::RemoveProfileProject { .. }
                | PersistOp::ReorderProfileProject { .. }
                | PersistOp::SetProfileTheme { .. }
                | PersistOp::SetStartupProfile { .. } => {}
            }
        }
    }
}
