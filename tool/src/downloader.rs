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
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            base_url: "https://www.sq0527.cn".to_string(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn search_song(&self, song_name: &str) -> Result<Vec<SearchResult>, DownloaderError> {
        let url = format!(
            "{}/search?ac={}",
            self.base_url,
            urlencoding::encode(song_name)
        );
        let html = self.client.get(&url).send()?.text()?;
        let document = Html::parse_document(&html);
        let selector =
            Selector::parse("ul.mul li a").map_err(|e| DownloaderError::Parse(e.to_string()))?;

        Ok(document
            .select(&selector)
            .filter_map(|element| {
                let text = element.text().collect::<String>().trim().to_string();
                let href = element.value().attr("href")?.to_string();
                (!text.is_empty()).then_some(SearchResult { text, href })
            })
            .collect())
    }

    pub fn get_lyrics(&self, lyrics_url: &str) -> Result<String, DownloaderError> {
        let html = self.client.get(lyrics_url).send()?.text()?;
        let document = Html::parse_document(&html);
        let selector = Selector::parse("textarea.layui-textarea")
            .map_err(|e| DownloaderError::Parse(e.to_string()))?;

        document
            .select(&selector)
            .next()
            .map(|element| element.text().collect::<String>())
            .ok_or_else(|| DownloaderError::Parse("Lyrics not found".to_string()))
    }

    pub fn save_lyrics(
        &self,
        lyrics: &str,
        filename: &str,
        dir: &str,
    ) -> Result<(), DownloaderError> {
        fs::create_dir_all(dir)?;
        fs::write(Path::new(dir).join(filename), lyrics)?;
        Ok(())
    }
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub text: String,
    pub href: String,
}

pub fn load_songs_from_file(file_path: &str) -> Result<Vec<(String, String)>, DownloaderError> {
    let content = fs::read_to_string(file_path)?;
    let re = Regex::new(r"^(.+?)\s*-\s*(.+)$").unwrap();

    Ok(content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            if let Some(captures) = re.captures(line) {
                Some((
                    captures[1].trim().to_string(),
                    captures[2].trim().to_string(),
                ))
            } else {
                Some((line.to_string(), String::new()))
            }
        })
        .collect())
}

fn log_error(file_path: &str, message: &str) -> Result<(), DownloaderError> {
    use std::io::Write;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)?;
    writeln!(file, "{}", message)?;
    Ok(())
}

pub fn download_lyrics() -> Result<(), DownloaderError> {
    let downloader = Downloader::new();
    let songs = load_songs_from_file("songs_list.txt")?;
    let total = songs.len();

    for (i, (song_name, original_filename)) in songs.into_iter().enumerate() {
        println!("\n({}/{}) searching: {}", i + 1, total, song_name);
        let mut error_msg = song_name.clone();

        let success = match downloader.search_song(&song_name) {
            Ok(results) if !results.is_empty() => {
                let lyrics_url = format!("{}{}", downloader.base_url(), results[0].href);
                match downloader.get_lyrics(&lyrics_url) {
                    Ok(lyrics) => {
                        let filename = format!("{}.lrc", original_filename);
                        downloader.save_lyrics(&lyrics, &filename, "./lyrics")?;
                        println!("Saved: {}", filename);
                        true
                    }
                    Err(e) => {
                        println!("Lyrics not found: {}", e);
                        error_msg.push_str("|lyrics not found");
                        false
                    }
                }
            }
            Ok(_) => {
                println!("Search results not found");
                error_msg.push_str("|search results not found");
                false
            }
            Err(e) => {
                println!("Search failed: {}", e);
                error_msg.push_str("|search failed");
                false
            }
        };

        if !success {
            log_error("error.txt", &error_msg)?;
        }
    }

    Ok(())
}
