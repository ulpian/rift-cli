//! Plain terminal prompts for the standalone wizard; deliberately no panel frame.

use super::chrome::header_top;
use super::{Choice, Purpose, Stage, Wizard};
use crate::layout::Rect;
use crate::render::{Color, Frame, Mods, Style};
use crate::view::text::clip_at;

pub(super) struct Row {
    label: String,
    detail: String,
    checked: Option<bool>,
    permission: Option<(u16, String, Color)>,
}

impl Row {
    fn text(label: String) -> Self {
        Self {
            label,
            detail: String::new(),
            checked: None,
            permission: None,
        }
    }
}

impl Wizard {
    pub(super) fn review(&self) -> Vec<Row> {
        let plan = crate::ProjectSetupPlan::from_view(&self.view);
        let mut rows = vec![Row::text(format!(
            "Harnesses: {}",
            if plan.integrations.is_empty() {
                "none (configure capture later)".into()
            } else {
                plan.integrations.join(", ")
            }
        ))];
        rows.extend(
            self.view
                .permissions
                .iter()
                .filter(|row| self.harness_selected(&row.agent))
                .filter_map(|row| permission_row(row, true)),
        );
        // The installer has no feature page: when every feature is on, which
        // is how Rift ships, say so in one line and where to change it.
        if self.purpose == Purpose::Installation && self.view.settings.iter().all(|row| row.enabled)
        {
            rows.push(Row::text(
                "Features: all on (switch any off per project in Project Settings)".into(),
            ));
            return rows;
        }
        rows.extend(self.view.settings.iter().map(|row| Row {
            checked: Some(row.enabled),
            ..Row::text(row.id.label().into())
        }));
        rows
    }

    fn row(&self, choice: &Choice) -> Option<Row> {
        match *choice {
            Choice::Harness(i) => {
                let row = self.view.integrations.get(i)?;
                Some(Row {
                    label: row.display_name.clone(),
                    detail: format!(
                        "{}. Capture new sessions; historical import is a separate opt-in.",
                        row.availability()
                    ),
                    checked: Some(row.selected),
                    permission: None,
                })
            }
            Choice::Permission(i) => permission_row(self.view.permissions.get(i)?, false),
            Choice::Setting(i) => {
                let row = self.view.settings.get(i)?;
                let suffix = if row.actionable {
                    ""
                } else {
                    " (repository policy)"
                };
                Some(Row {
                    label: format!("{}{suffix}", row.id.label()),
                    detail: row
                        .detail
                        .clone()
                        .unwrap_or_else(|| row.id.description().into()),
                    checked: Some(row.enabled),
                    permission: None,
                })
            }
        }
    }

    pub(crate) fn draw(&self, frame: &mut Frame, seconds: f32, unicode: bool) {
        let size = frame.size();
        if size.w < 40 || size.h < 16 {
            clip_at(
                frame,
                0,
                0,
                size.w,
                "Rift setup: enlarge terminal to 40x16",
                Style::DEFAULT,
            );
            clip_at(frame, 0, 1, size.w, "Esc / Ctrl-C cancel", Style::DEFAULT);
            return;
        }
        let width = size.w.saturating_sub(4);
        let dim = Style::fg(Color::Rgb(144, 144, 144));
        self.draw_header(frame, seconds, unicode);
        if let Some(meter) = &self.progress {
            meter.draw(frame, header_top(size) + 4, unicode, &self.status);
            return;
        }
        let rows = self.rows();
        self.draw_rows(frame, &rows, unicode);
        let description = if !self.status.is_empty() {
            self.status.as_str()
        } else if self.setup_confirmation && self.purpose == Purpose::Installation {
            "Save these as suggestions for future projects. Project setup still reviews permissions and raw transcript retention. No project is changed."
        } else if self.setup_confirmation
            && crate::ProjectSetupPlan::from_view(&self.view).raw_transcripts
        {
            "Raw transcripts can contain secrets and personal data. Confirm setup also acknowledges keeping them locally. Back applies no changes."
        } else if self.setup_confirmation && self.status.is_empty() {
            "Confirm setup applies the reviewed choices to this folder. Back returns to review."
        } else {
            rows.get(self.cursor)
                .filter(|row| !row.detail.is_empty())
                .map_or("Review every choice before continuing.", |row| {
                    row.detail.as_str()
                })
        };
        draw_description(frame, description, size.h.saturating_sub(5), width, dim);
        let hint = if self.setup_confirmation {
            "↑/↓ choose  Enter confirm / back  Esc back  Ctrl-C cancel"
        } else {
            "↑/↓ move  Space change  Enter choose / continue  Esc back  Ctrl-C cancel"
        };
        clip_at(frame, 2, size.h.saturating_sub(1), width, hint, dim);
    }

    fn rows(&self) -> Vec<Row> {
        if self.stage == Stage::Review {
            let mut rows = self.review();
            if self.setup_confirmation {
                rows.extend(
                    [self.continue_label(), self.confirm_label(), "Back"]
                        .into_iter()
                        .map(|label| Row::text(label.into())),
                );
            }
            return rows;
        }
        self.choices()
            .iter()
            .filter_map(|choice| self.row(choice))
            .collect()
    }

    fn continue_label(&self) -> &'static str {
        if self.stage == Stage::Review {
            if self.purpose == Purpose::Installation {
                "Save installation defaults"
            } else {
                "Set up project and open Canopy"
            }
        } else {
            "Continue"
        }
    }

    fn confirm_label(&self) -> &'static str {
        if self.purpose == Purpose::Installation {
            "Confirm defaults"
        } else {
            "Confirm setup"
        }
    }

    fn draw_rows(&self, frame: &mut Frame, rows: &[Row], unicode: bool) {
        let next = Row::text(self.continue_label().into());
        for (index, area) in self.row_areas(frame.size(), rows.len()) {
            let row = rows.get(index).unwrap_or(&next);
            draw_choice(
                frame,
                area,
                row,
                index == self.cursor,
                unicode,
                self.stage == Stage::Review,
                self.setup_confirmation && index < rows.len().saturating_sub(2),
            );
        }
    }

    // Drawing and mouse input use the same compact, scrolled option rows.
    pub(super) fn row_areas(&self, size: crate::render::Size, count: usize) -> Vec<(usize, Rect)> {
        if size.w < 40 || size.h < 16 {
            return Vec::new();
        }
        let rows = self.rows();
        let top = header_top(size) + 4;
        let footer = if self.setup_confirmation { 5 } else { 6 };
        let height = usize::from(size.h.saturating_sub(top + footer))
            .saturating_sub(usize::from(self.setup_confirmation));
        let last = if self.setup_confirmation {
            rows.len().saturating_sub(1)
        } else {
            count
        };
        let cursor = if self.setup_confirmation {
            last
        } else {
            self.cursor
        };
        let start = cursor.saturating_sub(height.saturating_sub(1));
        (start..=last)
            .take(height)
            .enumerate()
            .map(|(offset, index)| {
                let gap =
                    u16::from(self.setup_confirmation && index >= rows.len().saturating_sub(2));
                let row = rows.get(index);
                let label = row.map_or(self.continue_label(), |row| row.label.as_str());
                let prefix = if row.and_then(|row| row.checked).is_some() {
                    4
                } else {
                    2
                };
                let width = u16::try_from(label.chars().count())
                    .unwrap_or(u16::MAX)
                    .saturating_add(prefix)
                    .min(size.w.saturating_sub(4));
                (
                    index,
                    Rect {
                        x: 2,
                        y: top + u16::try_from(offset).unwrap_or(0) + gap,
                        w: width,
                        h: 1,
                    },
                )
            })
            .collect()
    }
}

pub(super) fn draw_description(frame: &mut Frame, text: &str, top: u16, width: u16, style: Style) {
    let text = crate::col2::sanitize_for_terminal(text);
    let mut line = String::new();
    let mut y = top;
    for word in text.split_whitespace() {
        if line.chars().count() + word.chars().count() + 1 > usize::from(width) && !line.is_empty()
        {
            clip_at(frame, 2, y, width, &line, style);
            line.clear();
            y = y.saturating_add(1);
            if y >= frame.size().h.saturating_sub(1) {
                return;
            }
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    clip_at(frame, 2, y, width, &line, style);
}

fn draw_choice(
    frame: &mut Frame,
    area: Rect,
    row: &Row,
    focused: bool,
    unicode: bool,
    review: bool,
    dimmed: bool,
) {
    let gray = Style::fg(Color::Rgb(144, 144, 144));
    let white = if dimmed {
        gray
    } else {
        Style::fg(Color::Rgb(255, 255, 255))
    };
    let normal = if dimmed { gray } else { Style::DEFAULT };
    let normal = if focused {
        normal.with_mods(Mods::BOLD)
    } else {
        normal
    };
    let indicator = if focused { ">" } else { " " };
    clip_at(frame, area.x, area.y, 1, indicator, white);
    let x = area.x + 2;
    let prefix = if row.checked.is_some() { 2 } else { 0 };
    let width = area.w.saturating_sub(2);
    let y = area.y;
    if let Some(checked) = row.checked {
        let (mut glyph, mut style) = crate::controls::toggle(checked, unicode);
        if review && !checked {
            if unicode {
                glyph = '●';
            }
            style = Style::fg(crate::theme::HEALTH_DOWN);
        }
        if dimmed {
            style = gray;
        }
        frame.set(x, y, glyph, style);
    }
    clip_at(
        frame,
        x + prefix,
        y,
        width.saturating_sub(prefix),
        &row.label,
        normal,
    );
    if let Some((offset, label, color)) = &row.permission {
        let color = if dimmed {
            Color::Rgb(144, 144, 144)
        } else {
            *color
        };
        let offset = prefix.saturating_add(*offset);
        clip_at(
            frame,
            x.saturating_add(offset),
            y,
            width.saturating_sub(offset),
            label,
            Style::fg(color).with_mods(normal.mods),
        );
    }
}

fn permission_row(permission: &crate::AgentPermissionRow, review: bool) -> Option<Row> {
    let option = permission.options.get(permission.selected)?;
    let prefix = format!("{}: ", permission.agent);
    let mut row = Row::text(format!("{prefix}{}", option.label));
    if review {
        row.label.push_str(&format!(" — {}", option.detail));
    } else {
        row.detail.clone_from(&option.detail);
    }
    let color = match option.token.as_str() {
        "keep" => Color::Rgb(240, 196, 92),
        "restricted" => Color::Rgb(92, 168, 255),
        "workspace" => Color::Rgb(255, 165, 0),
        _ => return Some(row),
    };
    row.permission = Some((
        u16::try_from(prefix.chars().count()).unwrap_or(u16::MAX),
        option.label.clone(),
        color,
    ));
    Some(row)
}
