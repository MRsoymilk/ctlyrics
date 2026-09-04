use ctlyrics_utils::scan_music_dir;
use std::fs;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SongListError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Scan error: {0}")]
    Scan(#[from] ctlyrics_utils::ScanError),
}

pub fn generate_song_list(music_dir: &str, output_file: &str) -> Result<(), SongListError> {
    let songs = scan_music_dir(music_dir)?;
    let mut content = String::new();

    for song in songs {
        content.push_str(&song.title);
        if !song.artist.is_empty() {
            content.push_str(" - ");
            content.push_str(&song.artist);
        }
        content.push('\n');
    }

    fs::write(output_file, content)?;
    Ok(())
}
