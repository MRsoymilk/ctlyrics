use regex::Regex;
use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CmusError {
    #[error("cmus-remote failed: {0}")]
    CommandFailed(String),
}

pub enum PlaybackCommand {
    TogglePause,
    Next,
    Previous,
    Stop,
}

pub fn control_cmus(command: PlaybackCommand) -> Result<(), CmusError> {
    let argument = match command {
        PlaybackCommand::TogglePause => "-u",
        PlaybackCommand::Next => "-n",
        PlaybackCommand::Previous => "-r",
        PlaybackCommand::Stop => "-s",
    };
    let output = Command::new("cmus-remote")
        .arg(argument)
        .output()
        .map_err(|error| CmusError::CommandFailed(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CmusError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

pub fn seek_cmus(position: u64) -> Result<(), CmusError> {
    let output = Command::new("cmus-remote")
        .args(["-k", &position.to_string()])
        .output()
        .map_err(|error| CmusError::CommandFailed(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CmusError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
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

    let tag_title = output
        .lines()
        .find_map(|line| line.strip_prefix("tag title "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let tag_artist = output
        .lines()
        .find_map(|line| line.strip_prefix("tag artist "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let filename = Path::new(&file)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let (filename_title, filename_artist) = filename
        .rsplit_once('-')
        .map(|(title, artist)| (title.trim(), Some(artist.trim())))
        .unwrap_or((filename.trim(), None));
    let title = tag_title.unwrap_or_else(|| {
        if filename_title.is_empty() {
            String::new()
        } else {
            filename_title.to_string()
        }
    });
    let artist = tag_artist
        .or_else(|| filename_artist.map(str::to_string))
        .unwrap_or_default();

    Ok(CmusInfo {
        position,
        duration,
        status,
        title,
        artist,
        file,
    })
}
