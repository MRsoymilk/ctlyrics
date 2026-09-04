use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::lyrics_cache::LyricLine;

pub struct Player {
    pub offset: f64,
    pub command_mode: bool,
    pub command_buffer: String,
    pub message: String,
    last_size: Option<(u16, u16)>,
}

impl Player {
    pub fn new() -> Self {
        Self {
            offset: 0.0,
            command_mode: false,
            command_buffer: String::new(),
            message: String::new(),
            last_size: None,
        }
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> bool {
        if self.command_mode {
            match key.code {
                KeyCode::Enter => {
                    self.execute_command();
                    self.command_mode = false;
                }
                KeyCode::Backspace => {
                    self.command_buffer.pop();
                }
                KeyCode::Char(c) => {
                    self.command_buffer.push(c);
                }
                KeyCode::Esc => {
                    self.command_mode = false;
                    self.command_buffer.clear();
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Char('q') => return true,
                KeyCode::Char(':') => {
                    self.command_mode = true;
                    self.command_buffer.clear();
                }
                KeyCode::Left => self.offset -= 0.1,
                KeyCode::Right => self.offset += 0.1,
                KeyCode::Up => self.offset -= 0.5,
                KeyCode::Down => self.offset += 0.5,
                _ => {}
            }
        }
        false
    }

    fn execute_command(&mut self) {
        let cmd = self.command_buffer.trim();
        if cmd == "love" {
            self.message = "to my beloved Can'.".to_string();
        } else if !cmd.is_empty() {
            self.message = format!("exec: {}", cmd);
        }
        self.command_buffer.clear();
    }

    pub fn draw(&mut self, frame: &mut Frame, info: &CmusInfo, lyrics: &[LyricLine]) {
        let size = frame.area();
        self.last_size = Some((size.width, size.height));

        let vertical = Layout::vertical([
            Constraint::Length(1), // Song info
            Constraint::Length(1), // Progress bar
            Constraint::Min(0),    // Lyrics
        ])
        .split(size);

        self.draw_song_info(frame, vertical[0], info);
        self.draw_progress_bar(frame, vertical[1], info);
        self.draw_lyrics(frame, vertical[2], lyrics, info.position);
        self.draw_command_line(frame, size);
    }

    fn draw_song_info(&self, frame: &mut Frame, area: Rect, info: &CmusInfo) {
        let text = format!(
            "{} - {} [{}] (Offset: {:.1}s)",
            info.title, info.artist, info.status, self.offset
        );
        let paragraph = Paragraph::new(text).style(Style::default().fg(Color::White));
        frame.render_widget(paragraph, area);
    }

    fn draw_progress_bar(&self, frame: &mut Frame, area: Rect, info: &CmusInfo) {
        if info.duration == 0 {
            return;
        }

        let time_str = format!(
            "{} / {}",
            format_time(info.position),
            format_time(info.duration)
        );
        let time_width = time_str.len() as u16 + 1;

        let available_width = area.width.saturating_sub(time_width);
        let progress = ((info.position as f64 / info.duration as f64) * available_width as f64) as u16;

        let bar = "=".repeat(progress as usize) + &"-".repeat((available_width - progress) as usize);
        let line = Line::from(vec![
            Span::raw(bar),
            Span::raw(" "),
            Span::styled(time_str, Style::default().fg(Color::Gray)),
        ]);

        frame.render_widget(Paragraph::new(line), area);
    }

    fn draw_lyrics(&self, frame: &mut Frame, area: Rect, lyrics: &[LyricLine], position: u64) {
        if lyrics.is_empty() || area.height == 0 {
            return;
        }

        let adjusted_pos = position as f64 + self.offset;
        let mut current_line = 0;

        for (i, line) in lyrics.iter().enumerate() {
            if adjusted_pos >= line.timestamp {
                current_line = i;
            } else {
                break;
            }
        }

        let visible_lines = area.height as usize;
        let start_line = current_line.saturating_sub(visible_lines / 2);
        let end_line = (start_line + visible_lines).min(lyrics.len());

        let mut text = Vec::new();
        for i in start_line..end_line {
            let line = &lyrics[i];
            let style = if i == current_line {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            text.push(Line::from(Span::styled(&line.text, style)));
        }

        let paragraph = Paragraph::new(text);
        frame.render_widget(paragraph, area);
    }

    fn draw_command_line(&mut self, frame: &mut Frame, size: Rect) {
        let area = Rect::new(0, size.height.saturating_sub(1), size.width, 1);
        let text = if self.command_mode {
            format!(":{}", self.command_buffer)
        } else {
            self.message.clone()
        };
        let paragraph = Paragraph::new(text).style(Style::default().fg(Color::Yellow));
        frame.render_widget(paragraph, area);
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

fn format_time(seconds: u64) -> String {
    let mins = seconds / 60;
    let secs = seconds % 60;
    format!("{:02}:{:02}", mins, secs)
}

use crate::cmus::CmusInfo;