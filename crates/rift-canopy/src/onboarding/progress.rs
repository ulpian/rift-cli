//! Step-based progress from the existing native setup reports. The final slot
//! is verification: a failed or incomplete setup can never display 100%.

use crate::render::{Color, Frame, Mods, Style};
use crate::{ProjectSettingsView, SetupProgress, SetupStep, SetupStepStatus};

pub(super) struct SetupMeter {
    steps: Vec<(SetupStep, bool)>,
    pub(super) complete: bool,
    failed: bool,
}

impl SetupMeter {
    pub(super) fn defaults() -> Self {
        Self {
            steps: vec![(SetupStep::Configuration, false)],
            complete: false,
            failed: false,
        }
    }

    pub(super) fn new(view: &ProjectSettingsView) -> Self {
        let mut steps = [
            SetupStep::Registry,
            SetupStep::Configuration,
            SetupStep::Projection,
            SetupStep::GitHooks,
            SetupStep::Policy,
            SetupStep::Recall,
        ]
        .into_iter()
        .map(|step| (step, false))
        .collect::<Vec<_>>();
        steps.extend(
            view.integrations
                .iter()
                .filter(|row| row.selected || row.enabled())
                .map(|row| (SetupStep::Agent(row.display_name.clone()), false)),
        );
        Self {
            steps,
            complete: false,
            failed: false,
        }
    }

    pub(super) fn record(&mut self, report: &SetupProgress) {
        self.failed |= report.status == SetupStepStatus::Failed;
        if let Some((_, done)) = self.steps.iter_mut().find(|(step, _)| *step == report.step) {
            *done = matches!(
                report.status,
                SetupStepStatus::Done | SetupStepStatus::Skipped
            );
        }
    }

    fn counts(&self) -> (usize, usize) {
        let total = self.steps.len() + 1;
        let done = if self.complete {
            total
        } else {
            self.steps.iter().filter(|(_, done)| *done).count()
        };
        (done, total)
    }

    pub(super) fn draw(&self, frame: &mut Frame, top: u16, unicode: bool, status: &str) {
        let width = frame.size().w.saturating_sub(4);
        let (done, total) = self.counts();
        let bar_width = usize::from(width.saturating_sub(8).min(48));
        let filled = bar_width * done / total;
        let (full, empty) = if unicode { ('━', '─') } else { ('#', '-') };
        let bar: String = (0..bar_width)
            .map(|i| if i < filled { full } else { empty })
            .collect();
        let color = if self.failed {
            crate::theme::HEALTH_DOWN
        } else {
            crate::theme::HEALTH_OK
        };
        let bar = format!("[{bar}] {}%", done * 100 / total);
        crate::view::text::clip_at(frame, 2, top, width, &bar, Style::fg(color));
        crate::view::text::clip_at(
            frame,
            2,
            top + 2,
            width,
            &format!("{done}/{total} steps complete (includes final verification)"),
            Style::DEFAULT.with_mods(Mods::BOLD),
        );
        super::render::draw_description(
            frame,
            status,
            top + 4,
            width,
            Style::fg(Color::Rgb(144, 144, 144)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_running_skipped_failed_and_optional_reports_are_counted_honestly() {
        let view = ProjectSettingsView::thin("/fixture".into(), "fixture".into());
        let mut meter = SetupMeter::new(&view);
        assert_eq!(meter.counts(), (0, 7));
        meter.record(&SetupProgress::running(SetupStep::Registry, "start"));
        assert_eq!(meter.counts(), (0, 7));
        for _ in 0..2 {
            meter.record(&SetupProgress::done(SetupStep::Registry, "registered"));
        }
        meter.record(&SetupProgress::done(
            SetupStep::Worktree,
            "optional adoption",
        ));
        assert_eq!(meter.counts(), (1, 7));
        meter.record(&SetupProgress::skipped(
            SetupStep::Policy,
            "committed policy",
        ));
        assert_eq!(meter.counts(), (2, 7));
        meter.record(&SetupProgress::failed(
            SetupStep::Projection,
            "fixture refusal",
        ));
        assert_eq!(meter.counts(), (2, 7));
        assert!(meter.failed);
        let mut frame = Frame::new(crate::render::Size::new(80, 24));
        meter.draw(&mut frame, 10, false, "fixture refusal");
        assert_eq!(
            frame.get(2, 10).unwrap().style.fg,
            crate::theme::HEALTH_DOWN
        );
        let text: String = frame.cells().iter().map(|cell| cell.ch).collect();
        assert!(text.contains("28%"));
        assert!(text.contains('#'));
        assert!(!text.contains("100%"));
    }
}
