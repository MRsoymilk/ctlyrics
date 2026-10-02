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
use crate::waveform::{Waveform, WaveformSnapshot};

pub struct WaveformView {
    locale: Locale,
    waveform_area: Option<Rect>,
    previous_button: Option<Rect>,
    play_pause_button: Option<Rect>,
    next_button: Option<Rect>,
    progress_duration: u64,
    message: String,
    message_expires_at: Option<Instant>,
}

impl WaveformView {
    pub fn new(locale: Locale) -> Self {
        Self {
            locale,
            waveform_area: None,
            previous_button: None,
            play_pause_button: None,
            next_button: None,
            progress_duration: 0,
            message: String::new(),
            message_expires_at: None,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, info: &CmusInfo, snapshot: &WaveformSnapshot) {
        self.expire_message();
        let area = frame.area();
        let vertical = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        self.draw_header(frame, vertical[0], info);

        let waveform_area = inset_horizontal(vertical[1], 2);
        self.waveform_area = Some(waveform_area);
        self.progress_duration = info.duration;

        match snapshot {
            WaveformSnapshot::Ready { file, waveform } if file.to_string_lossy() == info.file => {
                draw_waveform(frame, waveform_area, waveform, info.position, info.duration);
            }
            WaveformSnapshot::Loading { file } if file.to_string_lossy() == info.file => {
                draw_centered_status(
                    frame,
                    waveform_area,
                    tr(self.locale, "waveform_loading"),
                    Color::DarkGray,
                );
            }
            WaveformSnapshot::Error { file, message } if file.to_string_lossy() == info.file => {
                draw_centered_status(
                    frame,
                    waveform_area,
                    &tr_format(
                        self.locale,
                        "waveform_error",
                        &[("error", message.as_ref())],
                    ),
                    Color::Red,
                );
            }
            _ if info.file.is_empty() => {
                draw_centered_status(
                    frame,
                    waveform_area,
                    tr(self.locale, "waveform_no_track"),
                    Color::DarkGray,
                );
            }
            _ => {
                draw_centered_status(
                    frame,
                    waveform_area,
                    tr(self.locale, "waveform_loading"),
                    Color::DarkGray,
                );
            }
        }

        self.draw_time(frame, vertical[2], info);
        self.draw_controls(frame, vertical[3], info);
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

        if contains(self.waveform_area, event.column, event.row) && self.progress_duration > 0 {
            let area = self.waveform_area.unwrap();
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

        let lines = vec![
            Line::from(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(artist, Style::default().fg(Color::DarkGray))),
        ];
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
    }

    fn draw_time(&self, frame: &mut Frame, area: Rect, info: &CmusInfo) {
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
            area,
        );
    }

    fn draw_controls(&mut self, frame: &mut Frame, area: Rect, info: &CmusInfo) {
        let (previous, play_pause, next) = control_rects(area);
        self.previous_button = Some(previous);
        self.play_pause_button = Some(play_pause);
        self.next_button = Some(next);

        draw_button(frame, previous, "⏮︎", false);
        draw_button(
            frame,
            play_pause,
            if info.status == "playing" {
                "⏸︎"
            } else {
                "▶︎"
            },
            true,
        );
        draw_button(frame, next, "⏭︎", false);
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

impl Default for WaveformView {
    fn default() -> Self {
        Self::new(crate::i18n::preferred_locale())
    }
}

fn draw_waveform(frame: &mut Frame, area: Rect, waveform: &Waveform, position: u64, duration: u64) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let peaks = waveform.resample(area.width as usize);
    if peaks.is_empty() {
        return;
    }

    let center = area.height / 2;
    let upper_height = center.max(1);
    let lower_height = area.height.saturating_sub(center + 1).max(1);
    let cursor = if duration == 0 {
        None
    } else {
        Some(
            ((position.min(duration) as f64 / duration as f64)
                * f64::from(area.width.saturating_sub(1)))
            .round() as usize,
        )
    };

    let lines = (0..area.height)
        .map(|row| {
            let glyphs = peaks
                .iter()
                .map(|peak| {
                    if row == center {
                        '─'
                    } else if row < center {
                        let distance = center - row;
                        let filled = (peak.max.max(0.0) * f32::from(upper_height)).ceil() as u16;
                        if filled > 0 && distance <= filled {
                            '█'
                        } else {
                            ' '
                        }
                    } else {
                        let distance = row - center;
                        let filled = (-peak.min.min(0.0) * f32::from(lower_height)).ceil() as u16;
                        if filled > 0 && distance <= filled {
                            '█'
                        } else {
                            ' '
                        }
                    }
                })
                .collect::<Vec<_>>();

            if let Some(cursor) = cursor.filter(|cursor| *cursor < glyphs.len()) {
                let before = glyphs[..cursor].iter().collect::<String>();
                let after = glyphs[cursor + 1..].iter().collect::<String>();
                Line::from(vec![
                    Span::styled(before, Style::default().fg(Color::Green)),
                    Span::styled(
                        if row == center { "┼" } else { "│" },
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(after, Style::default().fg(Color::DarkGray)),
                ])
            } else {
                Line::from(Span::styled(
                    glyphs.iter().collect::<String>(),
                    Style::default().fg(Color::DarkGray),
                ))
            }
        })
        .collect::<Vec<_>>();

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_centered_status(frame: &mut Frame, area: Rect, text: &str, color: Color) {
    if area.height == 0 {
        return;
    }
    let row = Rect::new(
        area.x,
        area.y + area.height / 2,
        area.width,
        1.min(area.height),
    );
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

fn control_rects(area: Rect) -> (Rect, Rect, Rect) {
    let button_width = 5u16;
    let total_width = button_width * 3;
    let start = area.x + area.width.saturating_sub(total_width) / 2;
    (
        Rect::new(start, area.y, button_width, area.height),
        Rect::new(start + button_width, area.y, button_width, area.height),
        Rect::new(start + button_width * 2, area.y, button_width, area.height),
    )
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
    use std::{path::PathBuf, sync::Arc};

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
    fn waveform_view_renders_track_time_and_cursor() {
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        let mut view = WaveformView::new(Locale::En);
        let info = CmusInfo {
            title: "Song".to_string(),
            artist: "Artist".to_string(),
            position: 30,
            duration: 120,
            status: "playing".to_string(),
            file: "/tmp/song.flac".to_string(),
        };
        let snapshot = WaveformSnapshot::Ready {
            file: PathBuf::from("/tmp/song.flac"),
            waveform: Arc::new(Waveform {
                peaks: vec![
                    crate::waveform::WaveformPeak {
                        min: -0.8,
                        max: 0.8,
                    };
                    100
                ],
            }),
        };

        terminal
            .draw(|frame| view.draw(frame, &info, &snapshot))
            .unwrap();

        let text = buffer_text(&terminal);
        assert!(text.contains("Song"));
        assert!(text.contains("Artist"));
        assert!(text.contains("00:30 / 02:00"));
        assert!(text.contains('│') || text.contains('┼'));
    }
}
