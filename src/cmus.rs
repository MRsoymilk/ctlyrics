use regex::Regex;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use thiserror::Error;

const REMOTE_TIMEOUT: Duration = Duration::from_secs(1);
const STATUS_TIMEOUT: Duration = Duration::from_millis(100);
const STATUS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Error)]
pub enum CmusError {
    #[error("cmus-remote is unavailable: {0}")]
    CommandUnavailable(String),
    #[error("cmus-remote failed: {0}")]
    CommandFailed(String),
    #[error("cmus-remote timed out")]
    TimedOut,
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
    let output = run_cmus_remote(&[argument], REMOTE_TIMEOUT)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CmusError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

pub fn seek_cmus(position: u64) -> Result<(), CmusError> {
    let position = position.to_string();
    let output = run_cmus_remote(&["-k", &position], REMOTE_TIMEOUT)?;
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

pub struct CmusInfoPoller {
    latest: Arc<Mutex<CmusInfo>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl CmusInfoPoller {
    pub fn new() -> Self {
        Self::with_fetcher(|| get_cmus_info_with_timeout(STATUS_TIMEOUT))
    }

    fn with_fetcher<F>(fetch: F) -> Self
    where
        F: Fn() -> Result<CmusInfo, CmusError> + Send + 'static,
    {
        let latest = Arc::new(Mutex::new(CmusInfo::default()));
        let worker_latest = Arc::clone(&latest);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                if let Ok(info) = fetch()
                    && let Ok(mut latest) = worker_latest.lock()
                {
                    *latest = info;
                }
                sleep_until_next_poll(&worker_stop);
            }
        });
        Self {
            latest,
            stop,
            worker: Some(worker),
        }
    }

    pub fn latest(&self) -> CmusInfo {
        self.latest
            .lock()
            .map(|info| info.clone())
            .unwrap_or_default()
    }
}

impl Default for CmusInfoPoller {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CmusInfoPoller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn sleep_until_next_poll(stop: &AtomicBool) {
    let deadline = Instant::now() + STATUS_INTERVAL;
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
}

pub fn get_cmus_info() -> Result<CmusInfo, CmusError> {
    get_cmus_info_with_timeout(REMOTE_TIMEOUT)
}

fn get_cmus_info_with_timeout(timeout: Duration) -> Result<CmusInfo, CmusError> {
    let output = run_cmus_remote(&["-Q"], timeout)?;

    if !output.status.success() {
        return Err(CmusError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_cmus_output(&stdout)
}

pub fn probe_cmus() -> Result<bool, CmusError> {
    let output = run_cmus_remote(&["-Q"], REMOTE_TIMEOUT)?;
    if output.status.success() {
        return Ok(true);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.contains("not running") {
        Ok(false)
    } else {
        Err(CmusError::CommandFailed(stderr))
    }
}

fn run_cmus_remote(arguments: &[&str], timeout: Duration) -> Result<Output, CmusError> {
    let mut child = Command::new("cmus-remote")
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CmusError::CommandUnavailable(error.to_string()))?;
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|error| CmusError::CommandFailed(error.to_string()));
            }
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CmusError::TimedOut);
            }
            Err(error) => return Err(CmusError::CommandFailed(error.to_string())),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn poller_snapshot_does_not_wait_for_a_blocked_fetch() {
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let first_fetch = AtomicBool::new(true);
        let poller = CmusInfoPoller::with_fetcher(move || {
            if first_fetch.swap(false, Ordering::Relaxed) {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            Err(CmusError::TimedOut)
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let info = poller.latest();
        assert!(info.title.is_empty());

        release_tx.send(()).unwrap();
        drop(poller);
    }
}
