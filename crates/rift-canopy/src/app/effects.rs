//! The reducer's returned side effects, performed: data requests forwarded to
//! the bus and append-only persistence reflected back into the dashboard.

use rift_core::Result as RiftResult;

use crate::state::{AppState, DataRequest, Effects, PersistOp};
use crate::Project;

/// Perform the side effects the reducer returned: forward data requests to the
/// bus and apply append-only persistence. Reducer purity is preserved — all
/// I/O happens here.
pub(super) fn apply_effects<F>(
    effects: Effects,
    state: &mut AppState,
    on_persist: &mut F,
    req_sink: &mut dyn FnMut(DataRequest),
) where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
{
    for req in effects.data_requests {
        req_sink(req);
    }
    for op in effects.persist {
        apply_persist(state, on_persist, &op, req_sink);
    }
}

/// Apply one append-only persistence op and reflect the refreshed registry back
/// into the dashboard: keep the selection on the same project, set a status line,
/// and (re-)track every current project on the bus so new adds get live data.
fn apply_persist<F>(
    state: &mut AppState,
    on_persist: &mut F,
    op: &PersistOp,
    req_sink: &mut dyn FnMut(DataRequest),
) where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
{
    let keep = state.app.selected_project().map(|p| p.path.clone());
    match on_persist(op) {
        Ok(projects) => {
            state.app.set_projects(projects);
            if let PersistOp::AddProject { path } = op {
                // The add flow lands on the new row and asks its first status
                // read whether to open the setup wizard (the reducer decides,
                // once the data plane has looked at the machine).
                state.app.select_path(path);
                state.pending_setup_prompt = Some(path.clone());
            } else if let Some(path) = keep {
                state.app.select_path(&path); // selection follows the same project
            }
            // A success is a Notice; it auto-dismisses. When the
            // removal emptied the registry, `set_projects` already raised the
            // wizard, which the Notice's settle-close re-raises after it fades.
            // A theme apply is the one silent op (the colour picker and its presets): the live repaint
            // IS the acknowledgement — a toast would cover the preview.
            if !matches!(op, PersistOp::SetProfileTheme { .. }) {
                state.open_notice(crate::dispatch::Message::success(
                    "registry",
                    vec![persist_status(op)],
                ));
            }
            // Track every current project on the bus (new adds included), and
            // prioritize the selection so its live status/detail refreshes.
            for p in &state.app.projects {
                req_sink(DataRequest::Refresh(p.path.clone()));
            }
            let active = state.app.selected_project().map(|p| p.path.clone());
            req_sink(DataRequest::SetActive(active));
        }
        Err(e) => state.open_alert(crate::dispatch::Message::error(
            "registry",
            vec![e.to_string()],
        )),
    }
    state.dirty = true;
}

/// A short toast line for a completed persistence op.
fn persist_status(op: &PersistOp) -> String {
    match op {
        PersistOp::AddProject { path } => format!("registered {path}"),
        PersistOp::RemoveProject { path } => format!("removed {path}"),
        PersistOp::RelabelProject { label, .. } => format!("relabeled to {label}"),
        PersistOp::ReorderProject { up, .. } | PersistOp::ReorderProfileProject { up, .. } => {
            format!("moved {}", if *up { "up" } else { "down" })
        }
        PersistOp::SetLayout { layout, .. } => {
            if layout.is_some() {
                "saved layout".to_string()
            } else {
                "reset layout".to_string()
            }
        }
        PersistOp::CreateProfile { name } => format!("created profile {name}"),
        PersistOp::DuplicateProfile { name, .. } => format!("duplicated to {name}"),
        PersistOp::RenameProfile { name, .. } => format!("renamed profile to {name}"),
        PersistOp::RemoveProfile { .. } => "deleted profile".to_string(),
        PersistOp::SelectProfile { .. } => "switched profile".to_string(),
        PersistOp::AddProfileProject { path, .. } => format!("added {path} to profile"),
        PersistOp::RemoveProfileProject { path, .. } => {
            format!("removed {path} from profile")
        }
        PersistOp::SetProfileTheme { slot, colour, .. } => {
            if colour.is_some() {
                format!("set {slot} colour")
            } else {
                format!("reset {slot} colour")
            }
        }
        PersistOp::SetStartupProfile { profile } => {
            if profile.is_some() {
                "pinned startup profile".to_string()
            } else {
                "startup follows last selected".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::event_loop;
    use super::super::fixtures::{no_lister, ready_state, MockBackend, ScriptedEvents};
    use crate::state::{Event, PersistOp};
    use crate::{CanopyKey, EntryKind, Project};

    #[test]
    fn add_project_effect_invokes_on_add() {
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![
            // `a` browses, `i` falls back to the typed flow (the destination explorer).
            vec![
                Event::Key(CanopyKey::Char('a')),
                Event::Key(CanopyKey::Char('i')),
            ],
            "/tmp/x"
                .chars()
                .map(|c| Event::Key(CanopyKey::Char(c)))
                .collect(),
            vec![Event::Key(CanopyKey::Enter)],
            vec![Event::Key(CanopyKey::Quit)],
        ]);
        let mut state = ready_state();
        let mut seen: Vec<String> = Vec::new();
        let mut on_persist = |op: &PersistOp| {
            let PersistOp::AddProject { path } = op else {
                return Ok(vec![]);
            };
            seen.push(path.clone());
            Ok(vec![Project {
                name: "x".into(),
                path: path.clone(),
                status: crate::ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }])
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut on_persist,
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
        )
        .unwrap();
        assert_eq!(seen, vec!["/tmp/x".to_string()]);
        assert!(
            matches!(&state.app.overlay, Some(crate::dispatch::Overlay::Notice(m))
                if m.body.iter().any(|l| l.contains("registered /tmp/x"))),
            "the add success opened a Notice: {:?}",
            state.app.overlay
        );
        // The add landed on the new row and deferred the setup-wizard
        // decision to its first status read (the reducer's test covers the
        // decision itself).
        assert_eq!(
            state.app.selected_project().map(|p| p.path.as_str()),
            Some("/tmp/x")
        );
        assert_eq!(state.pending_setup_prompt.as_deref(), Some("/tmp/x"));
    }
}
