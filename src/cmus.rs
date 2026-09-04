use regex::Regex;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CmusError {
    #[error("cmus-remote failed: {0}")]
    CommandFailed(String),
}

#[derive(Debug, Clone, Default)]
pub struct CmusInfo {
    pub position: u64,
    pub duration: u64,
    pub status: String,
    pub title: String,
    pub artist: String,
    pub file: String,
}

pub fn get_cmus_info() -> Result<CmusInfo, CmusError> {
    let output = Command::new("cmus-remote")
        .arg("-Q")
        .output()
        .map_err(|e| CmusError::CommandFailed(e.to_string()))?;

    if !output.status.success() {
        return Err(CmusError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_cmus_output(&stdout)
}

fn parse_cmus_output(output: &str) -> Result<CmusInfo, CmusError> {
    let position_re = Regex::new(r"position (\d+)").unwrap();
    let duration_re = Regex::new(r"duration (\d+)").unwrap();
    let status_re = Regex::new(r"status (\w+)").unwrap();
    let file_re = Regex::new(r"file (.+)").unwrap();
    let title_artist_re = Regex::new(r"/([^/]+?)(?:\s*-\s*([^/.]+))?\.").unwrap();

    let position = position_re
        .captures(output)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);

    let duration = duration_re
        .captures(output)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);

    let status = status_re
        .captures(output)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "stopped".to_string());

    let file = file_re
        .captures(output)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();

    let (title, artist) = title_artist_re
        .captures(&file)
        .and_then(|c| {
            let title = c.get(1).map(|m| m.as_str().trim().to_string())?;
            let artist = c.get(2).map(|m| m.as_str().trim().to_string());
            Some((title, artist))
        })
        .unwrap_or_else(|| ("unknown".to_string(), None));

    Ok(CmusInfo {
        position,
        duration,
        status,
        title,
        artist: artist.unwrap_or_else(|| "unknown".to_string()),
        file,
    })
}