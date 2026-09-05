use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MappingError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Failed to scan music directory: {0}")]
    WalkDir(#[from] walkdir::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SongMapping {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub music_path: String,
    pub lrc_filename: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MappingStore {
    mappings: HashMap<String, SongMapping>,
    music_dir: Option<String>,
}

impl MappingStore {
    pub fn load(path: &Path) -> Result<Self, MappingError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)?;
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        let store: Self = serde_json::from_str(&content)?;
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<(), MappingError> {
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&SongMapping> {
        self.mappings.get(id)
    }

    pub fn get_by_music_path(&self, music_path: &str) -> Option<&SongMapping> {
        self.mappings.values().find(|m| m.music_path == music_path)
    }

    pub fn get_by_title_artist(&self, title: &str, artist: &str) -> Option<&SongMapping> {
        self.mappings
            .values()
            .find(|m| m.title.eq_ignore_ascii_case(title) && m.artist.eq_ignore_ascii_case(artist))
    }

    pub fn list(&self) -> Vec<&SongMapping> {
        let mut v: Vec<_> = self.mappings.values().collect();
        v.sort_by(|a, b| a.title.cmp(&b.title));
        v
    }

    pub fn insert(&mut self, mapping: SongMapping) {
        self.mappings.insert(mapping.id.clone(), mapping);
    }

    pub fn remove(&mut self, id: &str) -> Option<SongMapping> {
        self.mappings.remove(id)
    }

    pub fn set_music_dir(&mut self, dir: String) {
        self.music_dir = Some(dir);
    }

    pub fn music_dir(&self) -> Option<&str> {
        self.music_dir.as_deref()
    }
}

pub fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{:x}", now.as_millis())
}

pub fn get_mapping_path() -> PathBuf {
    PathBuf::from("config/mappings.json")
}

#[derive(Debug, Clone, Serialize)]
pub struct SongFile {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub ext: String,
}

pub fn scan_music_dir(dir: &str) -> Result<Vec<SongFile>, MappingError> {
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

pub fn list_lrc_files() -> Result<Vec<String>, MappingError> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir("lyrics") {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "lrc") {
                if let Some(name) = entry.file_name().to_str() {
                    files.push(name.to_string());
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

pub fn read_lrc_content(filename: &str) -> Result<String, MappingError> {
    let path = Path::new("lyrics").join(filename);
    fs::read_to_string(path).map_err(Into::into)
}
