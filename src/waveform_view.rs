use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::{Duration, Instant};

use crate::cmus::{CmusInfo, PlaybackCommand, control_cmus, seek_cmus};
use crate::i18n::{Locale, format as tr_format, tr};
use crate::waveform::SpectrumSnapshot;

pub struct SpectrumView {
    locale: Locale,
    progress_bar: Option<Rect>,
    previous_button: Option<Rect>,
    play_pause_button: Option<Rect>,
    next_button: Option<Rect>,
    progress_duration: u64,
    message: String,
    message_expires_at: Option<Instant>,
}

impl SpectrumView {
    pub fn new(locale: Locale) -> Self {
        Self {
            locale,
            progress_bar: None,
            previous_button: None,
            play_pause_button: None,
            next_button: None,
            progress_duration: 0,
            message: String::new(),
            message_expires_at: None,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, info: &CmusInfo, snapshot: &SpectrumSnapshot) {
        self.expire_message();
        let area = frame.area();
        let vertical = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        self.draw_header(frame, vertical[0], info);
        let spectrum_area = inset_horizontal(vertical[1], 1);

        match snapshot {
            SpectrumSnapshot::Ready { levels, peaks } => {
                draw_spectrum(frame, spectrum_area, levels, peaks);
            }
            SpectrumSnapshot::Starting | SpectrumSnapshot::Idle => {
                draw_centered_status(
                    frame,
                    spectrum_area,
                    tr(self.locale, "spectrum_starting"),
                    Color::DarkGray,
                );
            }
            SpectrumSnapshot::Error { message } => {
                draw_centered_status(
                    frame,
                    spectrum_area,
                    &tr_format(
                        self.locale,
                        "spectrum_error",
                        &[("error", message.as_ref())],
                    ),
                    Color::Red,
                );
            }
        }

        frame.render_widget(
            Paragraph::new(frequency_scale(vertical[2].width))
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            vertical[2],
        );
        self.draw_player_bar(frame, vertical[3], info);
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => true,
            KeyCode::Char(' ') => {
                self.control_playback(PlaybackCommand::TogglePause);
                false
            }
            KeyCode::Char('n') => {
                self.control_playback(PlaybackCommand::Next);
                false
            }
            KeyCode::Char('p') => {
                self.control_playback(PlaybackCommand::Previous);
                false
            }
            KeyCode::Char('s') => {
                self.control_playback(PlaybackCommand::Stop);
                false
            }
            _ => false,
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        if event.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }

        if contains(self.progress_bar, event.column, event.row) && self.progress_duration > 0 {
            let area = self.progress_bar.unwrap();
            let position = seek_position(area, event.column, self.progress_duration);
            if let Err(error) = seek_cmus(position) {
                self.show_message(tr_format(
                    self.locale,
                    "control_failed",
                    &[("error", &error.to_string())],
                ));
            }
        } else if contains(self.previous_button, event.column, event.row) {
            self.control_playback(PlaybackCommand::Previous);
        } else if contains(self.play_pause_button, event.column, event.row) {
            self.control_playback(PlaybackCommand::TogglePause);
        } else if contains(self.next_button, event.column, event.row) {
            self.control_playback(PlaybackCommand::Next);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect, info: &CmusInfo) {
        let title = if info.title.is_empty() {
            tr(self.locale, "unknown_title")
        } else {
            &info.title
        };
        let artist = if info.artist.is_empty() {
            tr(self.locale, "unknown_artist")
        } else {
            &info.artist
        };
        let subtitle = format!("{}  ·  {}", artist, tr(self.locale, "spectrum_live"));

        let lines = vec![
            Line::from(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                subtitle,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
        ];
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
    }

    fn draw_player_bar(&mut self, frame: &mut Frame, area: Rect, info: &CmusInfo) {
        let horizontal = Layout::horizontal([
            Constraint::Min(8),
            Constraint::Length(13),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(5),
        ])
        .spacing(1)
        .split(area);

        let bar_width = horizontal[0].width as usize;
        let progress = if info.duration == 0 {
            0
        } else {
            ((info.position.min(info.duration) as f64 / info.duration as f64) * bar_width as f64)
                .round() as usize
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("━".repeat(progress), Style::default().fg(Color::Green)),
                Span::styled(
                    "─".repeat(bar_width.saturating_sub(progress)),
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            horizontal[0],
        );
        self.progress_bar = Some(horizontal[0]);
        self.progress_duration = info.duration;

        let text = if self.message.is_empty() {
            format!(
                "{} / {}",
                format_time(info.position),
                format_time(info.duration)
            )
        } else {
            self.message.clone()
        };
        frame.render_widget(
            Paragraph::new(text)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Gray)),
            horizontal[1],
        );

        self.previous_button = Some(horizontal[2]);
        self.play_pause_button = Some(horizontal[3]);
        self.next_button = Some(horizontal[4]);
        draw_button(frame, horizontal[2], "⏮︎", false);
        draw_button(
            frame,
            horizontal[3],
            if info.status == "playing" {
                "⏸︎"
            } else {
                "▶︎"
            },
            true,
        );
        draw_button(frame, horizontal[4], "⏭︎", false);
    }

    fn control_playback(&mut self, command: PlaybackCommand) {
        if let Err(error) = control_cmus(command) {
            self.show_message(tr_format(
                self.locale,
                "control_failed",
                &[("error", &error.to_string())],
            ));
        } else {
            self.message.clear();
            self.message_expires_at = None;
        }
    }

    fn show_message(&mut self, message: String) {
        self.message = message;
        self.message_expires_at = Some(Instant::now() + Duration::from_secs(1));
    }

    fn expire_message(&mut self) {
        if self
            .message_expires_at
            .is_some_and(|expires_at| Instant::now() >= expires_at)
        {
            self.message.clear();
            self.message_expires_at = None;
        }
    }
}

impl Default for SpectrumView {
    fn default() -> Self {
        Self::new(crate::i18n::preferred_locale())
    }
}

fn draw_spectrum(frame: &mut Frame, area: Rect, levels: &[f32], peaks: &[f32]) {
    if area.width < 2 || area.height == 0 || levels.is_empty() {
        return;
    }

    let bar_count = (area.width as usize / 2).clamp(1, levels.len());
    let levels = resample_bands(levels, bar_count);
    let peaks = resample_bands(peaks, bar_count);
    let content_width = (bar_count * 2).saturating_sub(1) as u16;
    let left_padding = area.width.saturating_sub(content_width) / 2;
    let height = area.height as usize;

    let lines = (0..height)
        .map(|row| {
            let from_bottom = height - row;
            let mut spans = Vec::<Span<'static>>::with_capacity(bar_count * 2 + 1);
            if left_padding > 0 {
                spans.push(Span::raw(" ".repeat(left_padding as usize)));
            }

            for index in 0..bar_count {
                let filled = (levels[index].clamp(0.0, 1.0) * height as f32).round() as usize;
                let peak_row = (peaks[index].clamp(0.0, 1.0) * height as f32)
                    .round()
                    .max(1.0) as usize;
                let character = if from_bottom <= filled {
                    "█"
                } else if from_bottom == peak_row {
                    "▔"
                } else {
                    " "
                };

                let color = if character == "▔" {
                    Color::White
                } else {
                    spectrum_color(row, height)
                };
                spans.push(Span::styled(character, Style::default().fg(color)));
                if index + 1 < bar_count {
                    spans.push(Span::raw(" "));
                }
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();

    frame.render_widget(Paragraph::new(lines), area);
}

fn resample_bands(values: &[f32], target: usize) -> Vec<f32> {
    if target == 0 || values.is_empty() {
        return Vec::new();
    }

    (0..target)
        .map(|index| {
            let start = index * values.len() / target;
            let mut end = (index + 1) * values.len() / target;
            if end <= start {
                end = (start + 1).min(values.len());
            }
            values[start.min(values.len() - 1)..end.min(values.len())]
                .iter()
                .copied()
                .fold(0.0_f32, f32::max)
        })
        .collect()
}

fn spectrum_color(row: usize, height: usize) -> Color {
    if height == 0 {
        return Color::Green;
    }
    let fraction = row as f32 / height as f32;
    if fraction < 0.18 {
        Color::Red
    } else if fraction < 0.42 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn frequency_scale(width: u16) -> &'static str {
    if width >= 72 {
        "45Hz          250Hz          1kHz          4kHz          16kHz"
    } else if width >= 42 {
        "45Hz       250Hz       1kHz       16kHz"
    } else {
        "45Hz  ·  1kHz  ·  16kHz"
    }
}

fn draw_centered_status(frame: &mut Frame, area: Rect, text: &str, color: Color) {
    if area.height == 0 {
        return;
    }
    let row = Rect::new(area.x, area.y + area.height / 2, area.width, 1);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(color)),
        row,
    );
}

fn draw_button(frame: &mut Frame, area: Rect, icon: &str, primary: bool) {
    let style = Style::default()
        .fg(if primary { Color::Green } else { Color::White })
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(icon)
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
}

fn inset_horizontal(area: Rect, margin: u16) -> Rect {
    if area.width <= margin.saturating_mul(2) {
        return area;
    }
    Rect::new(
        area.x + margin,
        area.y,
        area.width - margin * 2,
        area.height,
    )
}

fn contains(area: Option<Rect>, column: u16, row: u16) -> bool {
    area.is_some_and(|area| {
        column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
    })
}

fn seek_position(area: Rect, column: u16, duration: u64) -> u64 {
    let width = area.width.saturating_sub(1).max(1);
    let offset = column.saturating_sub(area.x).min(width);
    (offset as f64 / width as f64 * duration as f64).round() as u64
}

fn format_time(seconds: u64) -> String {
    let mins = seconds / 60;
    let secs = seconds % 60;
    format!("{mins:02}:{secs:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::Arc;

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn spectrum_view_renders_live_bars_and_player() {
        let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
        let mut view = SpectrumView::new(Locale::En);
        let info = CmusInfo {
            title: "Song".to_string(),
            artist: "Artist".to_string(),
            position: 30,
            duration: 120,
            status: "playing".to_string(),
            ..CmusInfo::default()
        };
        let snapshot = SpectrumSnapshot::Ready {
            levels: Arc::from(vec![0.25, 0.5, 0.75, 1.0].into_boxed_slice()),
            peaks: Arc::from(vec![0.3, 0.55, 0.8, 1.0].into_boxed_slice()),
        };

        terminal
            .draw(|frame| view.draw(frame, &info, &snapshot))
            .unwrap();

        let text = buffer_text(&terminal);
        assert!(text.contains("Song"));
        assert!(text.contains("LIVE SPECTRUM"));
        assert!(text.contains("00:30 / 02:00"));
        assert!(text.contains('█'));
        assert!(text.contains("16kHz"));
    }

    #[test]
    fn resampling_keeps_strongest_band() {
        assert_eq!(resample_bands(&[0.1, 0.8, 0.2, 0.4], 2), vec![0.8, 0.4]);
    }
}
