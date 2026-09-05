use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};

use crate::i18n::{Locale, tr};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[cfg(target_os = "linux")]
mod icon;
#[cfg(target_os = "linux")]
mod sni;
#[cfg(target_os = "linux")]
mod xembed;

#[derive(Clone, Default)]
pub struct ExitSignal(Arc<AtomicBool>);

impl ExitSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct TrayService {
    playback: PlaybackInfo,
    #[cfg(target_os = "linux")]
    backend: Option<Backend>,
}

#[derive(Clone)]
struct PlaybackInfo(Arc<RwLock<PlaybackSnapshot>>);

#[derive(Clone, Default)]
struct PlaybackSnapshot {
    line: String,
    revision: u64,
}

impl PlaybackInfo {
    fn new(locale: Locale) -> Self {
        Self(Arc::new(RwLock::new(PlaybackSnapshot {
            line: playback_line(locale, "", "", "stopped"),
            revision: 0,
        })))
    }

    fn update(&self, line: String) -> bool {
        let mut snapshot = self.0.write().unwrap_or_else(|error| error.into_inner());
        if snapshot.line == line {
            return false;
        }
        snapshot.line = line;
        snapshot.revision = snapshot.revision.wrapping_add(1);
        true
    }

    fn snapshot(&self) -> PlaybackSnapshot {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

pub fn playback_line(locale: Locale, title: &str, artist: &str, status: &str) -> String {
    let title = clean_menu_text(title);
    let artist = clean_menu_text(artist);
    let title = if title.is_empty() {
        tr(locale, "unknown_title").to_string()
    } else {
        title
    };
    let song = if artist.is_empty() {
        title
    } else {
        format!("{title} - {artist}")
    };
    let status = match status {
        "playing" => tr(locale, "status_playing"),
        "paused" => tr(locale, "status_paused"),
        "stopped" => tr(locale, "status_stopped"),
        _ => tr(locale, "status_unknown"),
    };
    format!("{song} · {status}")
}

fn clean_menu_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn marquee_text(text: &str, width: usize, step: usize) -> String {
    if width == 0 || UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }

    let looped: Vec<char> = format!("{text}   ").chars().collect();
    let mut output = String::new();
    let mut output_width = 0;
    for offset in 0..looped.len() {
        let character = looped[(step + offset) % looped.len()];
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if output_width + character_width > width {
            break;
        }
        output.push(character);
        output_width += character_width;
    }
    output
}

#[cfg(target_os = "linux")]
enum Backend {
    Sni(sni::SniTray),
    XEmbed(xembed::XEmbedTray),
}

impl TrayService {
    pub fn start(locale: Locale, exit: ExitSignal) -> Self {
        let playback = PlaybackInfo::new(locale);
        #[cfg(target_os = "linux")]
        {
            match sni::SniTray::start(locale, exit.clone(), playback.clone()) {
                Ok(tray) => {
                    tracing::info!("using StatusNotifierItem tray backend");
                    return Self {
                        playback,
                        backend: Some(Backend::Sni(tray)),
                    };
                }
                Err(error) => tracing::debug!(%error, "StatusNotifierItem unavailable"),
            }

            match xembed::XEmbedTray::start(locale, exit, playback.clone()) {
                Ok(tray) => {
                    tracing::info!("using XEmbed tray backend");
                    Self {
                        playback,
                        backend: Some(Backend::XEmbed(tray)),
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "system tray unavailable");
                    Self {
                        playback,
                        backend: None,
                    }
                }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (locale, exit);
            Self { playback }
        }
    }

    pub fn update_playback(&self, locale: Locale, title: &str, artist: &str, status: &str) {
        if !self
            .playback
            .update(playback_line(locale, title, artist, status))
        {
            return;
        }

        #[cfg(target_os = "linux")]
        if let Some(Backend::Sni(tray)) = &self.backend {
            tray.refresh();
        }
    }
}

impl Drop for TrayService {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(backend) = self.backend.take() {
            match backend {
                Backend::Sni(tray) => tray.shutdown(),
                Backend::XEmbed(tray) => tray.shutdown(),
            }
        }
    }
}
