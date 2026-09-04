use regex::Regex;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DownloaderError {
    #[error("Request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Downloader {
    client: Client,
    base_url: String,
}

impl Downloader {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn new() -> Self {
        Self {
            client: Client::new(),
            base_url: "https://www.sq0527.cn".to_string(),
        }
    }

    pub fn search_song(&self, song_name: &str) -> Result<Vec<SearchResult>, DownloaderError> {
        let url = format!("{}/search?ac={}", self.base_url, urlencoding::encode(song_name));
        let response = self.client.get(&url).send()?;
        let html = response.text()?;

        let document = Html::parse_document(&html);
        let selector = Selector::parse("ul.mul li a").map_err(|e| DownloaderError::Parse(e.to_string()))?;

        let results: Vec<SearchResult> = document
            .select(&selector)
            .filter_map(|element| {
                let text = element.text().collect::<String>().trim().to_string();
                let href = element.value().attr("href")?.to_string();
                if !text.is_empty() {
                    Some(SearchResult { text, href })
                } else {
                    None
                }
            })
            .collect();

        Ok(results)
    }

    pub fn get_lyrics(&self, lyrics_url: &str) -> Result<String, DownloaderError> {
        let response = self.client.get(lyrics_url).send()?;
        let html = response.text()?;

        let document = Html::parse_document(&html);
        let selector = Selector::parse("textarea.layui-textarea")
            .map_err(|e| DownloaderError::Parse(e.to_string()))?;

        let lyrics = document
            .select(&selector)
            .next()
            .map(|el| el.text().collect::<String>())
            .ok_or_else(|| DownloaderError::Parse("Lyrics not found".to_string()))?;

        Ok(lyrics)
    }

    pub fn save_lyrics(&self, lyrics: &str, filename: &str, dir: &str) -> Result<(), DownloaderError> {
        fs::create_dir_all(dir)?;
        let path = Path::new(dir).join(filename);
        fs::write(path, lyrics)?;
        Ok(())
    }
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SearchResult {
    pub text: String,
    pub href: String,
}

pub fn load_songs_from_file(file_path: &str) -> Result<Vec<(String, String)>, DownloaderError> {
    let content = fs::read_to_string(file_path)?;
    let re = Regex::new(r"^(.+?)\s*-\s*(.+)$").unwrap();

    let songs: Vec<(String, String)> = content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            if let Some(caps) = re.captures(line) {
                Some((caps[1].trim().to_string(), caps[2].trim().to_string()))
            } else {
                Some((line.to_string(), String::new()))
            }
        })
        .collect();

    Ok(songs)
}

pub fn log_error(file_path: &str, message: &str) -> Result<(), DownloaderError> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)?;
    writeln!(file, "{}", message)?;
    Ok(())
}