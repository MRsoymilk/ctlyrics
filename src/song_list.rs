use std::fs;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SongListError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn generate_song_list(music_dir: &str, output_file: &str) -> Result<(), SongListError> {
    let valid_extensions = [".mp3", ".flac", ".wav"];
    let mut songs = Vec::new();

    for entry in fs::read_dir(music_dir)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !valid_extensions.contains(&ext) {
            continue;
        }

        let filename = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let (song_name, song_artist) = if let Some(idx) = filename.find('-') {
            let name = filename[..idx].trim().to_string();
            let artist = filename[idx + 1..].trim().to_string();
            (name, artist)
        } else {
            (filename.trim().to_string(), String::new())
        };

        songs.push((song_name, song_artist));
    }

    save_songs_to_file(&songs, output_file)?;
    Ok(())
}

fn save_songs_to_file(songs: &[(String, String)], file_path: &str) -> Result<(), SongListError> {
    let mut content = String::new();
    for (name, artist) in songs {
        if artist.is_empty() {
            content.push_str(name);
        } else {
            content.push_str(&format!("{} - {}", name, artist));
        }
        content.push('\n');
    }
    fs::write(file_path, content)?;
    Ok(())
}