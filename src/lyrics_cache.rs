use regex::Regex;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct LyricLine {
    pub timestamp: f64,
    pub text: String,
}

pub struct LyricsCache {
    last_title: Option<String>,
    last_lyrics: Vec<LyricLine>,
}

impl LyricsCache {
    pub fn new() -> Self {
        Self {
            last_title: None,
            last_lyrics: Vec::new(),
        }
    }

    pub fn load_lyrics(&mut self, title: &str, artist: Option<&str>) -> Vec<LyricLine> {
        if title.is_empty() {
            return Vec::new();
        }

        if self.last_title.as_deref() == Some(title) {
            return self.last_lyrics.clone();
        }

        if let Some(path) = find_lrc_file("lyrics", title, artist) {
            self.last_lyrics = parse_lrc(Path::new(&path));
        } else {
            self.last_lyrics.clear();
        }

        self.last_title = Some(title.to_string());
        self.last_lyrics.clone()
    }
}

fn find_lrc_file(directory: &str, title: &str, artist: Option<&str>) -> Option<String> {
    let title_pattern = Regex::new(&regex::escape(title)).ok()?;
    let files = fs::read_dir(directory).ok()?;

    let mut matching_files: Vec<String> = files
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.path().extension().is_some_and(|ext| ext == "lrc")
                && title_pattern.is_match(&entry.file_name().to_string_lossy())
        })
        .filter_map(|entry| entry.path().to_str().map(|s| s.to_string()))
        .collect();

    if matching_files.is_empty() {
        return None;
    }

    if matching_files.len() == 1 {
        return Some(matching_files.remove(0));
    }

    if let Some(artist) = artist {
        let artist_pattern = Regex::new(&regex::escape(artist)).ok()?;
        for file in &matching_files {
            if artist_pattern.is_match(file) {
                return Some(file.clone());
            }
        }
    }

    matching_files.first().cloned()
}

fn parse_lrc(file_path: &Path) -> Vec<LyricLine> {
    let mut lyrics = Vec::new();
    let content = match fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(_) => return lyrics,
    };

    let re = Regex::new(r"\[(\d+):(\d+\.\d+)\](.*)").unwrap();

    for line in content.lines() {
        for cap in re.captures_iter(line) {
            let minutes: u64 = cap[1].parse().unwrap_or(0);
            let seconds: f64 = cap[2].parse().unwrap_or(0.0);
            let timestamp = minutes as f64 * 60.0 + seconds;
            let text = cap[3].trim().to_string();
            lyrics.push(LyricLine { timestamp, text });
        }
    }

    lyrics.sort_by(|a, b| a.timestamp.partial_cmp(&b.timestamp).unwrap());
    lyrics
}

impl Default for LyricsCache {
    fn default() -> Self {
        Self::new()
    }
}