//! Shared installer/project introduction and milestone line.

use super::{Purpose, Stage, Wizard};
use crate::layout::Rect;
use crate::render::{Color, Frame, Mods, Size, Style};
use crate::view::text::clip_at;

const GRAY: Style = Style::fg(Color::Rgb(144, 144, 144));
const DESCRIPTION: &str = "Capture agent sessions. Recall project context. Work in your terminal.";

pub(super) fn header_top(size: Size) -> u16 {
    if crate::welcome::fits(size) {
        9
    } else {
        3
    }
}

impl Wizard {
    pub(super) fn draw_header(&self, frame: &mut Frame, seconds: f32, unicode: bool) {
        self.draw_intro(frame, seconds, unicode);
        let size = frame.size();
        let width = size.w.saturating_sub(4);
        let top = header_top(size);
        let complete = self.progress.as_ref().is_some_and(|meter| meter.complete);
        let current = if self.progress.is_some() {
            4
        } else {
            self.stages()
                .iter()
                .position(|stage| *stage == self.stage)
                .unwrap_or(0)
                + usize::from(self.purpose == Purpose::Installation)
        };
        draw_milestones(frame, top, current, 5, complete, unicode);
        let title = if self.progress.is_some() {
            if self.purpose == Purpose::Installation {
                "Saving defaults"
            } else {
                "Setting up Rift"
            }
        } else {
            match (self.purpose, self.stage) {
                (Purpose::Installation, Stage::Settings) => "Choose default features",
                (Purpose::Installation, Stage::Review) => "Review installation defaults",
                _ => self.stage.title(),
            }
        };
        let style = if self.setup_confirmation {
            GRAY
        } else {
            Style::DEFAULT
        };
        clip_at(frame, 2, top + 2, width, title, style.with_mods(Mods::BOLD));
    }

    fn draw_intro(&self, frame: &mut Frame, seconds: f32, unicode: bool) {
        let size = frame.size();
        let width = size.w.saturating_sub(4);
        let commands = if self.purpose == Purpose::Installation {
            "Update: rift update --from <link> · Setup: rift (in your project)"
        } else {
            "Diagnose: rift doctor · Help: rift --help"
        };
        let version = format!("Rift v{}", env!("CARGO_PKG_VERSION"));
        if crate::welcome::fits(size) {
            crate::welcome::draw(
                frame,
                Rect {
                    x: 2,
                    y: 1,
                    w: crate::welcome::WIDTH,
                    h: crate::welcome::HEIGHT,
                },
                seconds,
                unicode,
            );
            clip_at(
                frame,
                12,
                2,
                size.w.saturating_sub(14),
                "Welcome to Rift",
                Style::DEFAULT.with_mods(Mods::BOLD),
            );
            clip_at(frame, 2, 5, width, &version, GRAY);
            clip_at(frame, 2, 6, width, DESCRIPTION, GRAY);
            clip_at(frame, 2, 7, width, commands, GRAY);
            let context = if self.purpose == Purpose::Installation {
                "Binary installed. Choose defaults for future projects."
            } else {
                &self.view.path
            };
            clip_at(
                frame,
                2,
                8,
                width,
                &crate::col2::sanitize_for_terminal(context),
                GRAY,
            );
        } else {
            clip_at(
                frame,
                2,
                0,
                width,
                &format!("{version} · {DESCRIPTION}"),
                GRAY,
            );
            clip_at(frame, 2, 1, width, commands, GRAY);
        }
    }
}

fn draw_milestones(
    frame: &mut Frame,
    top: u16,
    current: usize,
    total: usize,
    complete: bool,
    unicode: bool,
) {
    for index in 0..total {
        let done = complete || index < current;
        let (glyph, mut style) = crate::controls::toggle(done, unicode);
        let glyph = if index == current && !complete {
            style = Style::fg(Color::Rgb(255, 255, 255)).with_mods(Mods::BOLD);
            if unicode {
                '◉'
            } else {
                '@'
            }
        } else {
            glyph
        };
        let x = 2 + u16::try_from(index * 3).unwrap_or(0);
        frame.set(x, top, glyph, style);
        if index + 1 < total {
            clip_at(
                frame,
                x + 1,
                top,
                2,
                if unicode { "──" } else { "--" },
                GRAY,
            );
        }
    }
    let status = if complete {
        "Complete".into()
    } else {
        format!(
            "Step {}/{} · {} left",
            current + 1,
            total,
            total.saturating_sub(current + 1)
        )
    };
    clip_at(
        frame,
        18,
        top,
        frame.size().w.saturating_sub(20),
        &status,
        GRAY,
    );
}
