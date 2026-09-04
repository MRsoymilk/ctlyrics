mod cmus;
mod logger;
mod lyrics_cache;
mod mapping;
mod player;
mod web;

#[cfg(test)]
mod tests {
    #[test]
    fn test_generate_id() {
        let id1 = crate::mapping::generate_id();
        let id2 = crate::mapping::generate_id();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_scan_music_dir() {
        let songs = crate::mapping::scan_music_dir("/home/vv/warehouse/music").unwrap();
        println!("Found {} songs", songs.len());
        for s in songs.iter().take(5) {
            println!("  {} - {} ({})", s.title, s.artist, s.ext);
        }
        assert!(!songs.is_empty());
    }
}

use anyhow::Result;
use clap::{Arg, Command};
use crossterm::{
    cursor::MoveTo,
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
use std::fs;
use std::io;
use std::time::Duration;

use crate::cmus::get_cmus_info;
use crate::logger::init_logger;
use crate::lyrics_cache::LyricsCache;
use crate::mapping::MappingStore;
use crate::player::Player;

#[tokio::main]
async fn main() -> Result<()> {
    fs::create_dir_all("log")?;
    fs::create_dir_all("config")?;

    let matches = Command::new("ctlyrics")
        .version("0.1.0")
        .about("Terminal lyrics viewer for cmus")
        .subcommand(
            Command::new("web")
                .about("Start web server for mapping management")
                .arg(
                    Arg::new("port")
                        .short('p')
                        .long("port")
                        .default_value("3000")
                        .help("Port to listen on"),
                ),
        )
        .get_matches();

    init_logger("ctlyrics.log");

    match matches.subcommand() {
        Some(("web", sub_m)) => {
            let port: u16 = sub_m.get_one::<String>("port").unwrap().parse()?;
            run_web(port).await
        }
        _ => run_tui(),
    }
}

fn run_tui() -> Result<()> {
    let mapping_store = MappingStore::load(&crate::mapping::get_mapping_path()).unwrap_or_default();

    enable_raw_mode()?;
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

    let mut player = Player::new();
    let mut lyrics_cache = LyricsCache::new().with_mapping(mapping_store);

    loop {
        let info = get_cmus_info().unwrap_or_default();
        let lyrics = lyrics_cache.load_lyrics(&info.title, Some(&info.artist), Some(&info.file));

        terminal.draw(|frame| player.draw(frame, &info, &lyrics))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if player.handle_input(key) {
                        break;
                    }
                }
                Event::Resize(_, _) => {
                    terminal.autoresize()?;
                    terminal.clear()?;
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    terminal.show_cursor()?;

    Ok(())
}

async fn run_web(port: u16) -> Result<()> {
    let app = crate::web::create_router();
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    println!("Web server running at http://localhost:{}", port);
    axum::serve(listener, app).await?;
    Ok(())
}
