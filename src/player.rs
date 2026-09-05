use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use open;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tokio;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cmus::{PlaybackCommand, control_cmus, seek_cmus};
use crate::i18n::{Locale, format as tr_format, preferred_locale, set_preference, tr};
use crate::lyrics_cache::LyricLine;

static WEB_SERVER_STARTED: AtomicBool = AtomicBool::new(false);

pub struct Player {
    pub offset: f64,
    pub command_mode: bool,
    pub command_buffer: String,
    pub message: String,
    locale: Locale,
    help_visible: bool,
    help_scroll: u16,
    help_max_scroll: u16,
    help_page_height: u16,
    message_expires_at: Option<Instant>,
    title_scroll_key: String,
    title_scroll_started: Instant,
    progress_bar: Option<Rect>,
    progress_duration: u64,
    previous_button: Option<Rect>,
    play_pause_button: Option<Rect>,
    next_button: Option<Rect>,
}

impl Player {
    pub fn new(locale: Locale) -> Self {
        Self {
            offset: 0.0,
            command_mode: false,
            command_buffer: String::new(),
            message: String::new(),
            locale,
            help_visible: false,
            help_scroll: 0,
            help_max_scroll: 0,
            help_page_height: 1,
            message_expires_at: None,
            title_scroll_key: String::new(),
            title_scroll_started: Instant::now(),
            progress_bar: None,
            progress_duration: 0,
            previous_button: None,
            play_pause_button: None,
            next_button: None,
        }
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if self.help_visible {
            return self.handle_help_input(key);
        }
        if self.command_mode {
            match key.code {
                KeyCode::Enter => {
                    self.execute_command();
                    self.command_mode = false;
                }
                KeyCode::Backspace => {
                    self.command_buffer.pop();
                }
                KeyCode::Char(':') => {
                    self.command_mode = false;
                    self.command_buffer.clear();
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
                KeyCode::Char('h') | KeyCode::Char('?') => self.open_help(),
                KeyCode::Char(':') => {
                    self.command_mode = true;
                    self.command_buffer.clear();
                }
                KeyCode::Char(' ') => self.control_playback(PlaybackCommand::TogglePause),
                KeyCode::Char('n') => self.control_playback(PlaybackCommand::Next),
                KeyCode::Char('p') => self.control_playback(PlaybackCommand::Previous),
                KeyCode::Char('s') => self.control_playback(PlaybackCommand::Stop),
                KeyCode::Left => self.offset -= 0.1,
                KeyCode::Right => self.offset += 0.1,
                KeyCode::Up => self.offset -= 0.5,
                KeyCode::Down => self.offset += 0.5,
                _ => {}
            }
        }
        false
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        if self.help_visible {
            match event.kind {
                MouseEventKind::ScrollUp => self.scroll_help_up(3),
                MouseEventKind::ScrollDown => self.scroll_help_down(3),
                _ => {}
            }
            return;
        }
        if self.command_mode || event.kind != MouseEventKind::Down(MouseButton::Left) {
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

    fn control_playback(&mut self, command: PlaybackCommand) {
        match control_cmus(command) {
            Ok(()) => self.clear_message(),
            Err(error) => self.show_message(tr_format(
                self.locale,
                "control_failed",
                &[("error", &error.to_string())],
            )),
        }
    }

    fn show_message(&mut self, message: String) {
        self.message = message;
        self.message_expires_at = Some(Instant::now() + Duration::from_secs(1));
    }

    fn clear_message(&mut self) {
        self.message.clear();
        self.message_expires_at = None;
    }

    fn expire_message(&mut self) {
        if self
            .message_expires_at
            .is_some_and(|expires_at| Instant::now() >= expires_at)
        {
            self.clear_message();
        }
    }

    fn execute_command(&mut self) {
        let cmd = self.command_buffer.trim().to_string();
        if cmd == "help" {
            self.open_help();
        } else if cmd == "love" {
            self.show_message(tr(self.locale, "love_message").to_string());
        } else if cmd == "web" {
            if !WEB_SERVER_STARTED.load(Ordering::SeqCst) {
                self.start_web_server();
            }
            if let Err(e) = open::that("http://localhost:3000") {
                self.show_message(tr_format(
                    self.locale,
                    "browser_open_failed",
                    &[("error", &e.to_string())],
                ));
            } else {
                self.show_message(tr(self.locale, "browser_opened").to_string());
            }
        } else if let Some(language) = cmd.strip_prefix("lang ") {
            if !language.eq_ignore_ascii_case("auto") && Locale::from_code(language).is_none() {
                self.show_message(tr(self.locale, "language_invalid").to_string());
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
                    self.show_message(tr(self.locale, key).to_string());
                }
                Err(error) => {
                    self.show_message(tr_format(
                        self.locale,
                        "language_save_failed",
                        &[("error", &error.to_string())],
                    ));
                }
            }
        } else if cmd == "lang" {
            self.show_message(tr(self.locale, "language_invalid").to_string());
        } else if !cmd.is_empty() {
            self.show_message(tr_format(
                self.locale,
                "command_unknown",
                &[("command", &cmd)],
            ));
        }
        self.command_buffer.clear();
    }

    fn start_web_server(&mut self) {
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
        self.show_message(tr(self.locale, "web_started").to_string());
    }

    pub fn draw(&mut self, frame: &mut Frame, info: &CmusInfo, lyrics: &[LyricLine]) {
        let size = frame.area();
        self.expire_message();

        if self.help_visible {
            self.clear_mouse_areas();
            self.draw_help(frame, size);
            return;
        }

        let vertical = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(size);

        self.draw_lyrics(frame, vertical[0], lyrics, info.position);
        if self.command_mode {
            self.progress_bar = None;
            self.previous_button = None;
            self.play_pause_button = None;
            self.next_button = None;
            self.draw_command_line(frame, vertical[1]);
        } else if !self.message.is_empty() {
            self.progress_bar = None;
            self.previous_button = None;
            self.play_pause_button = None;
            self.next_button = None;
            self.draw_message_line(frame, vertical[1]);
        } else {
            self.draw_player_bar(frame, vertical[1], info);
        }
    }

    fn handle_help_input(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('?') => {
                self.help_visible = false;
                self.help_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll_help_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_help_down(1),
            KeyCode::PageUp => self.scroll_help_up(self.help_page_height),
            KeyCode::PageDown => self.scroll_help_down(self.help_page_height),
            KeyCode::Home => self.help_scroll = 0,
            KeyCode::End => self.help_scroll = self.help_max_scroll,
            _ => {}
        }
        false
    }

    fn open_help(&mut self) {
        self.command_mode = false;
        self.command_buffer.clear();
        self.help_visible = true;
        self.help_scroll = 0;
    }

    fn scroll_help_up(&mut self, amount: u16) {
        self.help_scroll = self.help_scroll.saturating_sub(amount);
    }

    fn scroll_help_down(&mut self, amount: u16) {
        self.help_scroll = self
            .help_scroll
            .saturating_add(amount)
            .min(self.help_max_scroll);
    }

    fn clear_mouse_areas(&mut self) {
        self.progress_bar = None;
        self.previous_button = None;
        self.play_pause_button = None;
        self.next_button = None;
    }

    fn draw_help(&mut self, frame: &mut Frame, area: Rect) {
        let vertical = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
        let content_area = Rect::new(
            vertical[0].x.saturating_add(1),
            vertical[0].y,
            vertical[0].width.saturating_sub(2),
            vertical[0].height,
        );
        let lines = self.help_lines();
        self.help_page_height = content_area.height.saturating_sub(1).max(1);
        self.help_max_scroll = lines.len().saturating_sub(content_area.height as usize) as u16;
        self.help_scroll = self.help_scroll.min(self.help_max_scroll);

        frame.render_widget(
            Paragraph::new(lines).scroll((self.help_scroll, 0)),
            content_area,
        );
        frame.render_widget(
            Paragraph::new(tr(self.locale, "help_footer"))
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            vertical[1],
        );
    }

    fn help_lines(&self) -> Vec<Line<'static>> {
        let sections: &[(&str, &[(&str, &str)])] = &[
            (
                tr(self.locale, "help_section_global"),
                &[
                    ("h / ?", tr(self.locale, "help_open")),
                    ("q", tr(self.locale, "help_quit")),
                    ("Ctrl+C", tr(self.locale, "help_safe_quit")),
                ],
            ),
            (
                tr(self.locale, "help_section_navigation"),
                &[
                    ("↑ / ↓, j / k", tr(self.locale, "help_scroll")),
                    ("PgUp / PgDn", tr(self.locale, "help_page")),
                    ("Home / End", tr(self.locale, "help_first_last")),
                    ("Esc / h / ?", tr(self.locale, "help_close")),
                ],
            ),
            (
                tr(self.locale, "help_section_playback"),
                &[
                    ("Space", tr(self.locale, "help_toggle_playback")),
                    ("n", tr(self.locale, "help_next")),
                    ("p", tr(self.locale, "help_previous")),
                    ("s", tr(self.locale, "help_stop")),
                ],
            ),
            (
                tr(self.locale, "help_section_lyrics"),
                &[
                    ("← / →", tr(self.locale, "help_offset_small")),
                    ("↑ / ↓", tr(self.locale, "help_offset_large")),
                ],
            ),
            (
                tr(self.locale, "help_section_command_mode"),
                &[
                    (":", tr(self.locale, "help_command_open")),
                    ("Enter", tr(self.locale, "help_command_run")),
                    ("Backspace", tr(self.locale, "help_command_erase")),
                    ("Esc / :", tr(self.locale, "help_command_cancel")),
                ],
            ),
            (
                tr(self.locale, "help_section_commands"),
                &[
                    (":help", tr(self.locale, "help_cmd_help")),
                    (":web", tr(self.locale, "help_cmd_web")),
                    (":lang en", tr(self.locale, "help_cmd_lang_en")),
                    (":lang zh-CN", tr(self.locale, "help_cmd_lang_zh")),
                    (":lang auto", tr(self.locale, "help_cmd_lang_auto")),
                ],
            ),
            (
                tr(self.locale, "help_section_mouse"),
                &[
                    (
                        tr(self.locale, "help_mouse_buttons"),
                        tr(self.locale, "help_mouse_control"),
                    ),
                    (
                        tr(self.locale, "help_mouse_progress"),
                        tr(self.locale, "help_mouse_seek"),
                    ),
                    (
                        tr(self.locale, "help_mouse_wheel"),
                        tr(self.locale, "help_mouse_scroll"),
                    ),
                ],
            ),
        ];

        let mut lines = vec![Line::from(Span::styled(
            tr(self.locale, "help_title"),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ))];
        for (section_index, (section, entries)) in sections.iter().enumerate() {
            let section_is_last = section_index + 1 == sections.len();
            lines.push(Line::from(vec![
                Span::styled(
                    if section_is_last {
                        "└── "
                    } else {
                        "├── "
                    },
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    *section,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            for (entry_index, (key, description)) in entries.iter().enumerate() {
                let entry_is_last = entry_index + 1 == entries.len();
                let prefix = match (section_is_last, entry_is_last) {
                    (false, false) => "│   ├── ",
                    (false, true) => "│   └── ",
                    (true, false) => "    ├── ",
                    (true, true) => "    └── ",
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!(
                            "{key}{}",
                            " ".repeat(18usize.saturating_sub(UnicodeWidthStr::width(*key)))
                        ),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(*description),
                ]));
            }
        }
        lines
    }

    fn draw_player_bar(&mut self, frame: &mut Frame, area: Rect, info: &CmusInfo) {
        let title_width = (area.width / 3).clamp(18, 42);
        let horizontal = Layout::horizontal([
            Constraint::Length(title_width),
            Constraint::Min(8),
            Constraint::Length(13),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(5),
        ])
        .spacing(1)
        .split(area);

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
        let mut title_text = format!("{} - {}", title, artist);
        if self.offset.abs() >= 0.05 {
            title_text.push_str(&format!(" [{:+.1}s]", self.offset));
        }
        let title_text = self.scrolling_title(&title_text, horizontal[0].width as usize);
        frame.render_widget(
            Paragraph::new(title_text).style(
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            horizontal[0],
        );

        let bar_width = horizontal[1].width as usize;
        let progress = if info.duration == 0 {
            0
        } else {
            ((info.position.min(info.duration) as f64 / info.duration as f64) * bar_width as f64)
                .round() as usize
        };
        let line = Line::from(vec![
            Span::styled("━".repeat(progress), Style::default().fg(Color::Green)),
            Span::styled(
                "─".repeat(bar_width.saturating_sub(progress)),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), horizontal[1]);
        self.progress_bar = Some(horizontal[1]);
        self.progress_duration = info.duration;

        let time = format!(
            "{} / {}",
            format_time(info.position),
            format_time(info.duration)
        );
        frame.render_widget(
            Paragraph::new(time)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Gray)),
            horizontal[2],
        );

        self.previous_button = Some(horizontal[3]);
        self.play_pause_button = Some(horizontal[4]);
        self.next_button = Some(horizontal[5]);
        self.draw_player_button(frame, horizontal[3], "⏮︎", false);
        self.draw_player_button(
            frame,
            horizontal[4],
            if info.status == "playing" {
                "⏸︎"
            } else {
                "▶︎"
            },
            true,
        );
        self.draw_player_button(frame, horizontal[5], "⏭︎", false);
    }

    fn draw_player_button(&self, frame: &mut Frame, area: Rect, icon: &str, primary: bool) {
        let style = if primary {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::White)
        }
        .add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(icon)
                .alignment(Alignment::Center)
                .style(style),
            area,
        );
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

    fn draw_command_line(&mut self, frame: &mut Frame, area: Rect) {
        let paragraph = Paragraph::new(format!(":{}", self.command_buffer))
            .style(Style::default().fg(Color::White));
        frame.render_widget(paragraph, area);
        let cursor_x = area
            .x
            .saturating_add(self.command_buffer.chars().count() as u16)
            .saturating_add(1)
            .min(area.right().saturating_sub(1));
        frame.set_cursor_position((cursor_x, area.y));
    }

    fn draw_message_line(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(self.message.as_str()).style(Style::default().fg(Color::White)),
            area,
        );
    }

    fn scrolling_title(&mut self, title: &str, width: usize) -> String {
        if self.title_scroll_key != title {
            self.title_scroll_key = title.to_string();
            self.title_scroll_started = Instant::now();
        }
        if width == 0 || UnicodeWidthStr::width(title) <= width {
            return title.to_string();
        }

        let characters: Vec<char> = title.chars().collect();
        let max_start = (1..characters.len())
            .find(|&start| display_width(&characters[start..]) <= width)
            .unwrap_or(characters.len().saturating_sub(1));
        let cycle = max_start.saturating_mul(2).max(1);
        let phase = (self.title_scroll_started.elapsed().as_millis() / 250) as usize % cycle;
        let start = if phase <= max_start {
            phase
        } else {
            cycle - phase
        };
        fit_width(&characters[start..], width)
    }
}

fn display_width(characters: &[char]) -> usize {
    characters
        .iter()
        .map(|character| UnicodeWidthChar::width(*character).unwrap_or(0))
        .sum()
}

fn fit_width(characters: &[char], width: usize) -> String {
    let mut used = 0;
    characters
        .iter()
        .take_while(|character| {
            let character_width = UnicodeWidthChar::width(**character).unwrap_or(0);
            if used + character_width > width {
                false
            } else {
                used += character_width;
                true
            }
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::{Player, contains, fit_width, seek_position};
    use crate::cmus::CmusInfo;
    use crate::i18n::Locale;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    #[test]
    fn colon_and_escape_leave_command_mode() {
        for key in [KeyCode::Char(':'), KeyCode::Esc] {
            let mut player = Player::new(Locale::En);
            player.command_mode = true;
            player.command_buffer = "web".to_string();
            player.handle_input(KeyEvent::new(key, KeyModifiers::NONE));
            assert!(!player.command_mode);
            assert!(player.command_buffer.is_empty());
        }
    }

    #[test]
    fn control_c_requests_a_clean_exit_in_any_mode() {
        for command_mode in [false, true] {
            let mut player = Player::new(Locale::En);
            player.command_mode = command_mode;
            assert!(player.handle_input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL,)));
        }
    }

    #[test]
    fn help_tree_opens_from_keys_and_command() {
        for key in [KeyCode::Char('h'), KeyCode::Char('?')] {
            let mut player = Player::new(Locale::En);
            assert!(!player.handle_input(KeyEvent::new(key, KeyModifiers::NONE)));
            assert!(player.help_visible);
            assert!(!player.handle_input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE,)));
            assert!(!player.help_visible);
        }

        let mut player = Player::new(Locale::En);
        player.handle_input(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for character in "help".chars() {
            player.handle_input(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        player.handle_input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(player.help_visible);
        assert!(!player.command_mode);
    }

    #[test]
    fn help_tree_is_localized_and_scrollable() {
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut player = Player::new(Locale::ZhCn);
        player.handle_input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let help_text = player
            .help_lines()
            .into_iter()
            .flat_map(|line| line.spans.into_iter())
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("");
        assert!(help_text.contains("ctlyrics 帮助"));
        assert!(help_text.contains("全局"));

        terminal
            .draw(|frame| player.draw(frame, &CmusInfo::default(), &[]))
            .unwrap();
        let top = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(top.contains("ctlyrics"));
        assert!(player.help_max_scroll > 0);

        player.handle_input(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        terminal
            .draw(|frame| player.draw(frame, &CmusInfo::default(), &[]))
            .unwrap();
        let bottom = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert_eq!(player.help_scroll, player.help_max_scroll);
        assert!(bottom.contains("鼠"));
        assert!(bottom.contains("播"));
    }

    #[test]
    fn player_controls_are_reserved_on_the_bottom_row() {
        let backend = TestBackend::new(100, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut player = Player::new(Locale::En);
        let info = CmusInfo {
            title: "Song".to_string(),
            artist: "Artist".to_string(),
            duration: 180,
            position: 60,
            status: "playing".to_string(),
            ..CmusInfo::default()
        };

        terminal
            .draw(|frame| player.draw(frame, &info, &[]))
            .unwrap();

        let previous = player.previous_button.unwrap();
        let play_pause = player.play_pause_button.unwrap();
        let next = player.next_button.unwrap();
        let bottom_row = terminal.backend().buffer().content()[900..1000]
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert_eq!(previous.y, 9);
        assert!(previous.x < play_pause.x && play_pause.x < next.x);
        assert!(bottom_row.contains("⏮︎"));
        assert!(bottom_row.contains("⏸︎"));
        assert!(bottom_row.contains("⏭︎"));
        assert!(contains(Some(play_pause), play_pause.x, play_pause.y));
        assert!(!contains(Some(Rect::new(0, 9, 5, 1)), 5, 9));
    }

    #[test]
    fn title_clipping_respects_wide_characters() {
        assert_eq!(fit_width(&['歌', '曲', 'A'], 3), "歌");
        assert_eq!(fit_width(&['歌', '曲', 'A'], 4), "歌曲");
    }

    #[test]
    fn progress_click_maps_to_playback_position() {
        let area = Rect::new(10, 4, 11, 1);
        assert_eq!(seek_position(area, 10, 200), 0);
        assert_eq!(seek_position(area, 15, 200), 100);
        assert_eq!(seek_position(area, 20, 200), 200);
    }

    #[test]
    fn command_message_expires_back_to_player_bar() {
        let mut player = Player::new(Locale::En);
        player.command_mode = true;
        player.command_buffer = "love".to_string();
        player.handle_input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!player.message.is_empty());

        player.message_expires_at = Some(std::time::Instant::now());
        let backend = TestBackend::new(100, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| player.draw(frame, &CmusInfo::default(), &[]))
            .unwrap();
        assert!(player.message.is_empty());
        assert!(player.progress_bar.is_some());
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
