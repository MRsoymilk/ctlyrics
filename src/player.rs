use crossterm::event::{KeyCode, KeyEvent};
use open;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use tokio;

use crate::i18n::{Locale, format as tr_format, preferred_locale, set_preference, tr};
use crate::lyrics_cache::LyricLine;

static WEB_SERVER_STARTED: AtomicBool = AtomicBool::new(false);

pub struct Player {
    pub offset: f64,
    pub command_mode: bool,
    pub command_buffer: String,
    pub message: String,
    locale: Locale,
    last_size: Option<(u16, u16)>,
}

impl Player {
    pub fn new(locale: Locale) -> Self {
        Self {
            offset: 0.0,
            command_mode: false,
            command_buffer: String::new(),
            message: String::new(),
            locale,
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
        let cmd = self.command_buffer.trim().to_string();
        if cmd == "love" {
            self.message = tr(self.locale, "love_message").to_string();
        } else if cmd == "web" {
            if !WEB_SERVER_STARTED.load(Ordering::SeqCst) {
                self.start_web_server();
            }
            if let Err(e) = open::that("http://localhost:3000") {
                self.message = tr_format(
                    self.locale,
                    "browser_open_failed",
                    &[("error", &e.to_string())],
                );
            } else {
                self.message = tr(self.locale, "browser_opened").to_string();
            }
        } else if let Some(language) = cmd.strip_prefix("lang ") {
            if !language.eq_ignore_ascii_case("auto") && Locale::from_code(language).is_none() {
                self.message = tr(self.locale, "language_invalid").to_string();
                self.command_buffer.clear();
                return;
            }
            match set_preference(language) {
                Ok(locale) => {
                    self.locale = locale;
                    let key = if language.eq_ignore_ascii_case("auto") {
                        "language_auto"
                    } else {
                        "language_changed"
                    };
                    self.message = tr(self.locale, key).to_string();
                }
                Err(error) => {
                    self.message = tr_format(
                        self.locale,
                        "language_save_failed",
                        &[("error", &error.to_string())],
                    );
                }
            }
        } else if cmd == "lang" {
            self.message = tr(self.locale, "language_invalid").to_string();
        } else if !cmd.is_empty() {
            self.message = tr_format(self.locale, "command_unknown", &[("command", &cmd)]);
        }
        self.command_buffer.clear();
    }

    fn start_web_server(&mut self) {
        self.message = tr(self.locale, "web_starting").to_string();
        thread::spawn(|| {
            let Ok(rt) = tokio::runtime::Runtime::new() else {
                tracing::error!("failed to create web server runtime");
                return;
            };
            rt.block_on(async {
                let app = crate::web::create_router();
                let listener = match tokio::net::TcpListener::bind("0.0.0.0:3000").await {
                    Ok(listener) => listener,
                    Err(error) => {
                        tracing::error!(%error, "failed to bind web server");
                        return;
                    }
                };
                tracing::info!("web server running at http://localhost:3000");
                if let Err(error) = axum::serve(listener, app).await {
                    tracing::error!(%error, "web server stopped");
                }
            });
        });
        WEB_SERVER_STARTED.store(true, Ordering::SeqCst);
        self.message = tr(self.locale, "web_started").to_string();
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
        let status = match info.status.as_str() {
            "playing" => tr(self.locale, "status_playing"),
            "paused" => tr(self.locale, "status_paused"),
            "stopped" => tr(self.locale, "status_stopped"),
            _ => tr(self.locale, "status_unknown"),
        };
        let text = format!(
            "{} - {} [{}] ({}: {:.1}s)",
            title,
            artist,
            status,
            tr(self.locale, "offset"),
            self.offset
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
        let progress =
            ((info.position as f64 / info.duration as f64) * available_width as f64) as u16;

        let bar =
            "=".repeat(progress as usize) + &"-".repeat((available_width - progress) as usize);
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
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
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
        Self::new(preferred_locale())
    }
}

fn format_time(seconds: u64) -> String {
    let mins = seconds / 60;
    let secs = seconds % 60;
    format!("{:02}:{:02}", mins, secs)
}

use crate::cmus::CmusInfo;
