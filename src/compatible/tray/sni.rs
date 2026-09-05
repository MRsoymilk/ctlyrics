use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, RecvTimeoutError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use ksni::{Icon, MenuItem, Tray, blocking::TrayMethods, menu::StandardItem};
use notify_rust::{Hint, Notification, NotificationHandle, Timeout};

use crate::cmus::{PlaybackCommand, control_cmus};
use crate::i18n::{Locale, tr};

use super::{ExitSignal, PlaybackInfo, icon::argb_icon, marquee_text, progress_offset};

const MENU_COLUMNS: usize = 42;
const SCROLL_INTERVAL: Duration = Duration::from_millis(220);
const SCROLL_ACTIVE_MILLIS: u64 = 60_000;

struct TrayItem {
    exit: ExitSignal,
    quit_label: String,
    icon: Vec<u8>,
    playback: PlaybackInfo,
    locale: Locale,
    scroll_step: usize,
    scroll_active_until: Arc<AtomicU64>,
    notification_commands: mpsc::Sender<NotificationCommand>,
}

impl Tray for TrayItem {
    fn id(&self) -> String {
        "ctlyrics".to_string()
    }

    fn title(&self) -> String {
        "ctlyrics".to_string()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![Icon {
            width: 32,
            height: 32,
            data: self.icon.clone(),
        }]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let lyric = self.playback.snapshot().lyric;
        let _ = self
            .notification_commands
            .send(NotificationCommand::Toggle(lyric));
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let playback = self.playback.snapshot();
        let toggle_label = if playback.status == "playing" {
            tr(self.locale, "tray_pause")
        } else {
            tr(self.locale, "tray_play")
        };
        vec![
            StandardItem {
                label: marquee_text(&playback.line, MENU_COLUMNS, self.scroll_step)
                    .replace('_', "__"),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: progress_label(playback.position, playback.duration),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr(self.locale, "tray_previous").to_string(),
                icon_name: "media-skip-backward".to_string(),
                activate: Box::new(|_| run_control(PlaybackCommand::Previous)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: toggle_label.to_string(),
                icon_name: if playback.status == "playing" {
                    "media-playback-pause"
                } else {
                    "media-playback-start"
                }
                .to_string(),
                activate: Box::new(|_| run_control(PlaybackCommand::TogglePause)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr(self.locale, "tray_next").to_string(),
                icon_name: "media-skip-forward".to_string(),
                activate: Box::new(|_| run_control(PlaybackCommand::Next)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: self.quit_label.clone(),
                activate: Box::new(|tray: &mut Self| tray.exit.request()),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn menu_about_to_show(&mut self) {
        self.scroll_step = 0;
        self.scroll_active_until.store(
            unix_millis().saturating_add(SCROLL_ACTIVE_MILLIS),
            Ordering::Release,
        );
    }
}

pub struct SniTray {
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    notification_commands: mpsc::Sender<NotificationCommand>,
    notification_thread: Option<JoinHandle<()>>,
}

enum Command {
    Refresh,
    Shutdown,
}

enum NotificationCommand {
    Toggle(String),
    Update(String),
    Shutdown,
}

impl SniTray {
    pub fn start(locale: Locale, exit: ExitSignal, playback: PlaybackInfo) -> Result<Self> {
        let (notification_tx, notification_rx) = mpsc::channel();
        let notification_thread = thread::Builder::new()
            .name("ctlyrics-lyric-notification".to_string())
            .spawn(move || run_notifications(notification_rx))
            .context("failed to start lyric notification thread")?;
        let scroll_active_until = Arc::new(AtomicU64::new(0));
        let item = TrayItem {
            exit,
            quit_label: tr(locale, "tray_quit").to_string(),
            icon: argb_icon(32)?,
            playback,
            locale,
            scroll_step: 0,
            scroll_active_until: scroll_active_until.clone(),
            notification_commands: notification_tx.clone(),
        };
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("ctlyrics-sni-tray".to_string())
            .spawn(move || {
                let handle = match item.spawn().context("failed to register SNI tray") {
                    Ok(handle) => {
                        let _ = ready_tx.send(Ok(()));
                        handle
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };

                loop {
                    match command_rx.recv_timeout(SCROLL_INTERVAL) {
                        Ok(Command::Refresh) => {
                            let _ = handle.update(|item| {
                                item.scroll_step = 0;
                                let lyric = item.playback.snapshot().lyric;
                                let _ = item
                                    .notification_commands
                                    .send(NotificationCommand::Update(lyric));
                            });
                        }
                        Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {
                            if unix_millis() < scroll_active_until.load(Ordering::Acquire) {
                                let _ = handle.update(|item| {
                                    item.scroll_step = item.scroll_step.wrapping_add(1)
                                });
                            }
                        }
                    }
                }
                handle.shutdown().wait();
            })
            .context("failed to start SNI tray thread")?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("SNI initialization thread stopped"))??;
        Ok(Self {
            commands: command_tx,
            thread: Some(thread),
            notification_commands: notification_tx,
            notification_thread: Some(notification_thread),
        })
    }

    pub fn refresh(&self) {
        let _ = self.commands.send(Command::Refresh);
    }

    pub fn shutdown(mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = self
            .notification_commands
            .send(NotificationCommand::Shutdown);
        if let Some(thread) = self.notification_thread.take() {
            let _ = thread.join();
        }
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn progress_label(position: u64, duration: u64) -> String {
    const WIDTH: usize = 16;
    let filled = usize::from(progress_offset(position, duration, WIDTH as u16));
    let mut bar = String::with_capacity(WIDTH);
    for index in 0..WIDTH {
        bar.push(if index < filled { '━' } else { '─' });
    }
    format!(
        "{}  {bar}  {}",
        format_time(position),
        format_time(duration)
    )
}

fn format_time(seconds: u64) -> String {
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn run_control(command: PlaybackCommand) {
    let _ = thread::spawn(move || {
        if let Err(error) = control_cmus(command) {
            tracing::warn!(%error, "tray playback control failed");
        }
    });
}

fn run_notifications(commands: mpsc::Receiver<NotificationCommand>) {
    let mut handle: Option<NotificationHandle> = None;
    while let Ok(command) = commands.recv() {
        match command {
            NotificationCommand::Toggle(lyric) => {
                if let Some(current) = handle.take() {
                    current.close();
                } else {
                    handle = show_notification(&lyric);
                }
            }
            NotificationCommand::Update(lyric) => {
                if let Some(current) = handle.as_mut() {
                    current.body(&lyric);
                    if current.update().is_err() {
                        handle = None;
                    }
                }
            }
            NotificationCommand::Shutdown => {
                if let Some(current) = handle.take() {
                    current.close();
                }
                break;
            }
        }
    }
}

fn show_notification(lyric: &str) -> Option<NotificationHandle> {
    Notification::new()
        .appname("ctlyrics")
        .summary("ctlyrics")
        .body(lyric)
        .icon("audio-x-generic")
        .hint(Hint::Resident(true))
        .timeout(Timeout::Never)
        .show()
        .map_err(|error| tracing::debug!(%error, "lyric notification unavailable"))
        .ok()
}
