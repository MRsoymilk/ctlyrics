use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("Failed to scan music directory: {0}")]
    WalkDir(#[from] walkdir::Error),
}

#[derive(Debug, Clone, Serialize)]
pub struct SongFile {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub ext: String,
}

pub fn scan_music_dir(dir: &str) -> Result<Vec<SongFile>, ScanError> {
    let valid_extensions = ["mp3", "flac", "wav", "m4a", "ogg", "ape"];
    let mut songs = Vec::new();

    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !valid_extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            continue;
        }

        let filename = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let (title, artist) = if let Some(idx) = filename.find('-') {
            (
                filename[..idx].trim().to_string(),
                filename[idx + 1..].trim().to_string(),
            )
        } else {
            (filename.trim().to_string(), String::new())
        };

        songs.push(SongFile {
            path: path.to_string_lossy().to_string(),
            title,
            artist,
            ext: ext.to_string(),
        });
    }

    songs.sort_by(|a, b| a.title.cmp(&b.title));
    Ok(songs)
}
