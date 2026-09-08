use anyhow::Result;
use clap::{Arg, ArgAction, Command};
use crossterm::{
    cursor::{MoveTo, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    style::Print,
    terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use ctlyrics::cmus::{CmusInfoPoller, probe_cmus};
use ctlyrics::compatible::tray::{ExitSignal, TrayService};
#[cfg(target_os = "linux")]
use ctlyrics::embedded_cmus::EmbeddedCmus;
use ctlyrics::i18n::{Locale, detect_system_locale, preferred_locale, set_preference, tr};
use ctlyrics::logger::init_logger;
use ctlyrics::lyrics_cache::{LyricsCache, current_lyric_line};
use ctlyrics::mapping::{MappingStore, get_mapping_path};
use ctlyrics::player::Player;

#[tokio::main]
async fn main() -> Result<()> {
    ctlyrics::paths::prepare_user_dirs()?;

    let command_locale = locale_from_args();
    let matches = localize_command(
        Command::new("ctlyrics")
            .version(env!("CARGO_PKG_VERSION"))
            .about(tr(command_locale, "app_about").to_string())
            .arg(
                Arg::new("language")
                    .long("lang")
                    .value_parser(["auto", "en", "zh-CN"])
                    .hide_possible_values(true)
                    .help_heading(tr(command_locale, "cli_options").to_string())
                    .help(tr(command_locale, "language_help").to_string()),
            )
            .subcommand(localize_command(
                Command::new("web")
                    .version(env!("CARGO_PKG_VERSION"))
                    .about(tr(command_locale, "web_about").to_string())
                    .arg(
                        Arg::new("bind")
                            .long("bind")
                            .default_value("127.0.0.1")
                            .value_parser(clap::value_parser!(IpAddr))
                            .help_heading(tr(command_locale, "cli_options").to_string())
                            .help(tr(command_locale, "bind_help").to_string()),
                    )
                    .arg(
                        Arg::new("port")
                            .short('p')
                            .long("port")
                            .default_value("3000")
                            .hide_default_value(true)
                            .help_heading(tr(command_locale, "cli_options").to_string())
                            .help(tr(command_locale, "port_help").to_string()),
                    ),
                command_locale,
            )),
        command_locale,
    )
    .get_matches();

    let locale = if let Some(language) = matches.get_one::<String>("language") {
        set_preference(language)?
    } else {
        command_locale
    };

    init_logger("ctlyrics.log");

    match matches.subcommand() {
        Some(("web", sub_m)) => {
            let port: u16 = sub_m.get_one::<String>("port").unwrap().parse()?;
            let bind = *sub_m
                .get_one::<IpAddr>("bind")
                .unwrap_or(&IpAddr::V4(Ipv4Addr::LOCALHOST));
            run_web(bind, port, locale).await
        }
        _ => {
            let exit = ExitSignal::new();
            let tray = TrayService::start(locale, exit.clone());
            run_tui(locale, exit, &tray)
        }
    }
}

fn localize_command(command: Command, locale: Locale) -> Command {
    command
        .disable_help_subcommand(true)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .subcommand_help_heading(tr(locale, "cli_commands").to_string())
        .help_template(format!(
            "{{about}}\n\n{}: {{usage}}\n\n{{all-args}}",
            tr(locale, "cli_usage")
        ))
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::Help)
                .help_heading(tr(locale, "cli_options").to_string())
                .help(tr(locale, "cli_help").to_string()),
        )
        .arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::Version)
                .help_heading(tr(locale, "cli_options").to_string())
                .help(tr(locale, "cli_version").to_string()),
        )
}

fn locale_from_args() -> Locale {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--lang=") {
            return if value.eq_ignore_ascii_case("auto") {
                detect_system_locale()
            } else {
                Locale::from_code(value).unwrap_or_else(preferred_locale)
            };
        }
        if arg == "--lang" {
            return match args.next().as_deref() {
                Some("auto") => detect_system_locale(),
                Some(value) => Locale::from_code(value).unwrap_or_else(preferred_locale),
                None => preferred_locale(),
            };
        }
    }
    preferred_locale()
}

fn run_tui(locale: Locale, exit: ExitSignal, tray: &TrayService) -> Result<()> {
    let mapping_store = MappingStore::load(&get_mapping_path()).unwrap_or_default();

    enable_raw_mode()?;
    let _terminal_guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        Clear(ClearType::All),
        MoveTo(0, 0),
        EnableMouseCapture,
        // Crossterm also enables all-motion tracking, which floods the event queue.
        Print("\x1b[?1003l")
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut player = Player::new(locale);
    let mut lyrics_cache = LyricsCache::new().with_mapping(mapping_store);
    #[cfg(target_os = "linux")]
    let (mut embedded_cmus, mut active_view) = {
        let size = terminal.size()?;
        match probe_cmus() {
            Ok(false) => match EmbeddedCmus::spawn(size.width, size.height) {
                Ok(cmus) => (Some(cmus), ActiveView::Cmus),
                Err(error) => {
                    player.show_message(ctlyrics::i18n::format(
                        locale,
                        "cmus_start_failed",
                        &[("error", &error.to_string())],
                    ));
                    (None, ActiveView::Lyrics)
                }
            },
            Ok(true) => (None, ActiveView::Lyrics),
            Err(error) => {
                player.show_message(ctlyrics::i18n::format(
                    locale,
                    "cmus_probe_failed",
                    &[("error", &error.to_string())],
                ));
                (None, ActiveView::Lyrics)
            }
        }
    };
    let cmus_info = CmusInfoPoller::new();

    loop {
        if exit.is_requested() {
            break;
        }
        #[cfg(target_os = "linux")]
        if embedded_cmus
            .as_mut()
            .is_some_and(|cmus| !cmus.is_running().unwrap_or(false))
        {
            embedded_cmus.take();
            if active_view == ActiveView::Cmus {
                active_view = ActiveView::Lyrics;
                terminal.clear()?;
            }
            player.show_message(tr(locale, "cmus_exited").to_string());
        }
        let info = cmus_info.latest();
        let lyrics = lyrics_cache.load_lyrics(&info.title, Some(&info.artist), Some(&info.file));
        let lyric = current_lyric_line(&lyrics, info.position as f64 + player.lyric_offset())
            .map(|line| line.text.as_str())
            .unwrap_or_else(|| tr(locale, "no_lyrics"));
        tray.update_playback(
            locale,
            &info.title,
            &info.artist,
            &info.status,
            info.position,
            info.duration,
            lyric,
        );

        terminal.draw(|frame| {
            #[cfg(target_os = "linux")]
            if active_view == ActiveView::Cmus
                && let Some(cmus) = embedded_cmus.as_ref()
            {
                cmus.draw(frame);
                return;
            }
            player.draw(frame, &info, &lyrics);
        })?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if is_safe_quit(key) {
                        break;
                    }
                    #[cfg(target_os = "linux")]
                    if is_view_switch(key) && key.kind == KeyEventKind::Press {
                        match active_view {
                            ActiveView::Cmus => active_view = ActiveView::Lyrics,
                            ActiveView::Lyrics => {
                                if embedded_cmus.is_some() {
                                    active_view = ActiveView::Cmus;
                                } else {
                                    match probe_cmus() {
                                        Ok(true) => player.show_message(
                                            tr(locale, "cmus_external_running").to_string(),
                                        ),
                                        Ok(false) => {
                                            let size = terminal.size()?;
                                            match EmbeddedCmus::spawn(size.width, size.height) {
                                                Ok(cmus) => {
                                                    embedded_cmus = Some(cmus);
                                                    active_view = ActiveView::Cmus;
                                                }
                                                Err(error) => {
                                                    player.show_message(ctlyrics::i18n::format(
                                                        locale,
                                                        "cmus_start_failed",
                                                        &[("error", &error.to_string())],
                                                    ))
                                                }
                                            }
                                        }
                                        Err(error) => player.show_message(ctlyrics::i18n::format(
                                            locale,
                                            "cmus_probe_failed",
                                            &[("error", &error.to_string())],
                                        )),
                                    }
                                }
                            }
                        }
                        terminal.clear()?;
                        continue;
                    }
                    #[cfg(target_os = "linux")]
                    if active_view == ActiveView::Cmus {
                        if let Some(cmus) = embedded_cmus.as_mut()
                            && let Err(error) = cmus.send_key(key)
                        {
                            active_view = ActiveView::Lyrics;
                            player.show_message(ctlyrics::i18n::format(
                                locale,
                                "cmus_input_failed",
                                &[("error", &error.to_string())],
                            ));
                            terminal.clear()?;
                        }
                        continue;
                    }
                    if key.kind == KeyEventKind::Press && player.handle_input(key) {
                        break;
                    }
                }
                Event::Mouse(mouse) => {
                    #[cfg(target_os = "linux")]
                    if active_view == ActiveView::Cmus {
                        if let Some(cmus) = embedded_cmus.as_mut()
                            && let Err(error) = cmus.send_mouse(mouse)
                        {
                            active_view = ActiveView::Lyrics;
                            player.show_message(ctlyrics::i18n::format(
                                locale,
                                "cmus_input_failed",
                                &[("error", &error.to_string())],
                            ));
                            terminal.clear()?;
                        }
                        continue;
                    }
                    player.handle_mouse(mouse);
                }
                Event::Resize(width, height) => {
                    terminal.autoresize()?;
                    terminal.clear()?;
                    #[cfg(target_os = "linux")]
                    if let Some(cmus) = embedded_cmus.as_mut()
                        && let Err(error) = cmus.resize(width, height)
                    {
                        player.show_message(ctlyrics::i18n::format(
                            locale,
                            "cmus_resize_failed",
                            &[("error", &error.to_string())],
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveView {
    Lyrics,
    Cmus,
}

fn is_safe_quit(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(target_os = "linux")]
fn is_view_switch(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL)
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let row = crossterm::terminal::size()
            .map(|(_, height)| height.saturating_sub(1))
            .unwrap_or(0);
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            DisableMouseCapture,
            Show,
            MoveTo(0, row),
            Clear(ClearType::CurrentLine),
            Print("\r\n")
        );
        let _ = stdout.flush();
    }
}

async fn run_web(bind: IpAddr, port: u16, locale: Locale) -> Result<()> {
    let app = ctlyrics::web::create_router();
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    let url = format!("http://{}", SocketAddr::new(bind, port));
    println!(
        "{}",
        ctlyrics::i18n::format(locale, "web_running", &[("url", &url)])
    );
    axum::serve(listener, app).await?;
    Ok(())
}
