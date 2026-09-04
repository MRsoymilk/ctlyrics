mod cmus;
mod downloader;
mod logger;
mod lyrics_cache;
mod mapping;
mod player;
mod song_list;
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
        if songs.is_empty() {
            // Debug: check what walkdir sees
            for entry in walkdir::WalkDir::new("/home/vv/warehouse/music").into_iter().filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_file() {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    println!("File: {} ext: '{}'", path.display(), ext);
                }
            }
        }
        for s in songs.iter().take(5) {
            println!("  {} - {} ({})", s.title, s.artist, s.ext);
        }
        assert!(!songs.is_empty());
    }
}

use anyhow::Result;
use clap::{Arg, Command};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
use std::io;
use std::time::Duration;

use crate::cmus::get_cmus_info;
use crate::logger::init_logger;
use crate::lyrics_cache::LyricsCache;
use crate::mapping::MappingStore;
use crate::player::Player;

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Command::new("ctlyrics")
        .version("0.1.0")
        .about("Terminal lyrics viewer for cmus")
        .subcommand(Command::new("download").about("Download lyrics from songs_list.txt"))
        .subcommand(
            Command::new("generate")
                .about("Generate songs_list.txt from music directory")
                .arg(Arg::new("dir").required(true).help("Music directory path")),
        )
        .subcommand(
            Command::new("web")
                .about("Start web server for mapping management")
                .arg(Arg::new("port").short('p').long("port").default_value("3000").help("Port to listen on")),
        )
        .get_matches();

    init_logger("ctlyrics.log");

    match matches.subcommand() {
        Some(("download", _)) => download_lyrics(),
        Some(("generate", sub_m)) => {
            let dir = sub_m.get_one::<String>("dir").unwrap();
            generate_song_list(dir)
        }
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
    execute!(stdout, EnableMouseCapture)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut player = Player::new();
    let mut lyrics_cache = LyricsCache::new().with_mapping(mapping_store);

    loop {
        let info = get_cmus_info().unwrap_or_default();
        let lyrics = lyrics_cache.load_lyrics(&info.title, Some(&info.artist), Some(&info.file));

        terminal.draw(|frame| player.draw(frame, &info, &lyrics))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if player.handle_input(key) {
                        break;
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture
    )?;
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

fn download_lyrics() -> Result<()> {
    use crate::downloader::{load_songs_from_file, log_error, Downloader};

    let downloader = Downloader::new();
    let songs = load_songs_from_file("songs_list.txt")?;
    let total = songs.len();

    for (i, (song_name, original_filename)) in songs.into_iter().enumerate() {
        println!("\n({}/{}) searching: {}", i + 1, total, song_name);

        let mut success = true;
        let mut error_msg = song_name.clone();

        match downloader.search_song(&song_name) {
            Ok(results) if !results.is_empty() => {
                let selected = &results[0];
                let lyrics_url = format!("{}{}", downloader.base_url(), selected.href);

                match downloader.get_lyrics(&lyrics_url) {
                    Ok(lyrics) => {
                        let filename = format!("{}.lrc", original_filename);
                        downloader.save_lyrics(&lyrics, &filename, "./lyrics")?;
                        println!("Saved: {}", filename);
                    }
                    Err(e) => {
                        println!("Lyrics not found: {}", e);
                        success = false;
                        error_msg.push_str("|lyrics not found");
                    }
                }
            }
            Ok(_) => {
                println!("Search results not found");
                success = false;
                error_msg.push_str("|search results not found");
            }
            Err(e) => {
                println!("Search failed: {}", e);
                success = false;
                error_msg.push_str("|search failed");
            }
        }

        if !success {
            log_error("error.txt", &error_msg)?;
        }
    }

    Ok(())
}

fn generate_song_list(dir: &str) -> Result<()> {
    crate::song_list::generate_song_list(dir, "songs_list.txt")?;
    println!("Generated songs_list.txt");
    Ok(())
}