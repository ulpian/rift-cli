//! Terminal host for onboarding, before any dashboard or data bus is started.

use std::time::{Duration, Instant};

use crate::onboarding::{Action, Wizard};
use crate::render::{Capabilities, RenderBackend, Size};
use crate::{ProjectInitialization, ProjectSettingsManager, ProjectSettingsView};
use rift_core::{Result, RiftError};

use super::{Event, EventSource, LiveEvents, Terminal};

/// Complete standalone project setup, or return `false` when cancelled.
///
/// # Errors
/// Returns terminal failures. Setup refusals remain on the review step so the
/// operator can read the reason, repair it, or cancel without opening Canopy.
pub fn run_onboarding(
    view: ProjectSettingsView,
    manager: &mut dyn ProjectSettingsManager,
    ascii: bool,
) -> Result<bool> {
    run_wizard(Wizard::new(view), Completion::Project(manager), ascii)
}

/// Choose and save per-user installer defaults without starting project setup.
///
/// # Errors
/// Returns terminal failures; persistence refusals stay visible for retry/cancel.
pub fn run_installation(
    view: ProjectSettingsView,
    save: &mut dyn FnMut(&crate::ProjectSetupPlan) -> Result<()>,
    ascii: bool,
) -> Result<bool> {
    run_wizard(Wizard::installer(view), Completion::Defaults(save), ascii)
}

enum Completion<'a> {
    Project(&'a mut dyn ProjectSettingsManager),
    Defaults(&'a mut dyn FnMut(&crate::ProjectSetupPlan) -> Result<()>),
}

impl Completion<'_> {
    fn apply(
        &mut self,
        wizard: &mut Wizard,
        action: &crate::ProjectSettingsAction,
        backend: &mut dyn RenderBackend,
        size: Size,
        seconds: f32,
    ) -> Result<bool> {
        match self {
            Self::Project(manager) => initialize(wizard, action, *manager, backend, size, seconds),
            Self::Defaults(save) => save_defaults(wizard, action, *save, backend, size, seconds),
        }
    }
}

fn run_wizard(mut wizard: Wizard, mut completion: Completion<'_>, ascii: bool) -> Result<bool> {
    let caps = crate::render::caps::detect(ascii);
    let mut backend = crate::render::select_backend(caps, false);
    let prefs = crate::splash::prefs::load();
    let (_terminal, _) =
        super::enter_with_fallback(super::mouse_enabled(prefs.mouse), Terminal::enter)?;
    let reduced = prefs.reduced_motion || ascii || std::env::var_os("NO_COLOR").is_some();
    let mut size = super::probe_size();
    let started = Instant::now();
    let mut events = LiveEvents::new();
    let mut dirty = true;
    loop {
        let animated = !reduced && crate::welcome::fits(size);
        let seconds = if animated {
            started.elapsed().as_secs_f32()
        } else {
            f32::NAN
        };
        if dirty || animated {
            present(&wizard, backend.as_mut(), size, seconds)?;
            dirty = false;
        }
        let timeout = Duration::from_millis(if animated { 120 } else { 500 });
        for event in events.poll(timeout)? {
            let before = wizard.input_stage();
            let action = match event {
                Event::Resize(next) => {
                    size = next;
                    dirty = true;
                    continue;
                }
                Event::Key(key) => wizard_key(&mut wizard, key, size),
                Event::Mouse(mouse) => wizard.mouse(mouse, size),
                _ => continue,
            };
            dirty = true;
            match action {
                Action::Cancel => return Ok(false),
                Action::Apply(action) => {
                    if completion.apply(&mut wizard, &action, backend.as_mut(), size, seconds)? {
                        return Ok(true);
                    }
                    break;
                }
                Action::None => {}
            }
            // Never let buffered input consent to a newly shown step.
            if before != wizard.input_stage() {
                break;
            }
        }
    }
}

fn wizard_key(wizard: &mut Wizard, key: crate::CanopyKey, size: Size) -> Action {
    if (size.w < 40 || size.h < 16)
        && !matches!(key, crate::CanopyKey::Esc | crate::CanopyKey::Quit)
    {
        return Action::None;
    }
    wizard.key(key)
}

fn present(
    wizard: &Wizard,
    backend: &mut dyn RenderBackend,
    size: Size,
    seconds: f32,
) -> Result<()> {
    let mut frame = backend.begin_frame(size);
    wizard.draw(
        &mut frame,
        seconds,
        backend.capabilities().contains(Capabilities::UNICODE),
    );
    backend
        .present(&frame)
        .map_err(|error| RiftError::Other(error.to_string()))
}

fn initialize(
    wizard: &mut Wizard,
    action: &crate::ProjectSettingsAction,
    manager: &mut dyn ProjectSettingsManager,
    backend: &mut dyn RenderBackend,
    size: Size,
    seconds: f32,
) -> Result<bool> {
    wizard.start_setup();
    present(wizard, backend, size, seconds)?;
    let mut render_error = None;
    let result = manager.initialize(action, &mut |progress| {
        wizard.record_setup(progress);
        if let Err(error) = present(wizard, backend, size, seconds) {
            render_error = Some(error);
        }
    });
    if let Some(error) = render_error {
        return Err(error);
    }
    match result {
        Ok(view) if view.initialization == ProjectInitialization::Initialized => {
            wizard.finish_setup(true);
            present(wizard, backend, size, seconds)?;
            return Ok(true);
        }
        Ok(_) => {
            wizard.status =
                "Setup is incomplete. Repair the reported step and retry, or Esc to go back."
                    .into();
        }
        Err(reason) => {
            wizard.status =
                format!("Setup stopped: {reason}. Repair and retry, or Ctrl-C to exit.");
        }
    }
    wizard.finish_setup(false);
    Ok(false)
}

fn save_defaults(
    wizard: &mut Wizard,
    action: &crate::ProjectSettingsAction,
    save: &mut dyn FnMut(&crate::ProjectSetupPlan) -> Result<()>,
    backend: &mut dyn RenderBackend,
    size: Size,
    seconds: f32,
) -> Result<bool> {
    let crate::ProjectSettingsActionKind::Initialize(plan) = &action.kind else {
        return Err(RiftError::Other(
            "installer requires reviewed defaults".into(),
        ));
    };
    wizard.start_setup();
    present(wizard, backend, size, seconds)?;
    let result = save(plan);
    let saved = result.is_ok();
    if let Err(error) = result {
        wizard.status = format!("Defaults were not saved: {error}");
    }
    if saved {
        wizard.record_setup(crate::SetupProgress::done(
            crate::SetupStep::Configuration,
            "defaults saved and verified",
        ));
    }
    wizard.finish_setup(saved);
    present(wizard, backend, size, seconds)?;
    Ok(saved)
}
