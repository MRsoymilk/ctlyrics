use anyhow::Result;
use clap::{Arg, ArgAction, Command};
use crossterm::{
    cursor::{MoveTo, Show},
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    style::Print,
    terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
use std::fs;
use std::io::{self, Write};
use std::time::Duration;

use ctlyrics::cmus::get_cmus_info;
use ctlyrics::compatible::tray::{ExitSignal, TrayService};
use ctlyrics::i18n::{Locale, detect_system_locale, preferred_locale, set_preference, tr};
use ctlyrics::logger::init_logger;
use ctlyrics::lyrics_cache::LyricsCache;
use ctlyrics::mapping::{MappingStore, get_mapping_path};
use ctlyrics::player::Player;

#[tokio::main]
async fn main() -> Result<()> {
    fs::create_dir_all("log")?;
    fs::create_dir_all("config")?;

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
            run_web(port, locale).await
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
        EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut player = Player::new(locale);
    let mut lyrics_cache = LyricsCache::new().with_mapping(mapping_store);

    loop {
        if exit.is_requested() {
            break;
        }
        let info = get_cmus_info().unwrap_or_default();
        tray.update_playback(locale, &info.title, &info.artist, &info.status);
        let lyrics = lyrics_cache.load_lyrics(&info.title, Some(&info.artist), Some(&info.file));

        terminal.draw(|frame| player.draw(frame, &info, &lyrics))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if player.handle_input(key) {
                        break;
                    }
                }
                Event::Mouse(mouse) => player.handle_mouse(mouse),
                Event::Resize(_, _) => {
                    terminal.autoresize()?;
                    terminal.clear()?;
                }
                _ => {}
            }
        }
    }

    Ok(())
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

async fn run_web(port: u16, locale: Locale) -> Result<()> {
    let app = ctlyrics::web::create_router();
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    println!(
        "{}",
        ctlyrics::i18n::format(locale, "web_running", &[("port", &port.to_string())])
    );
    axum::serve(listener, app).await?;
    Ok(())
}
