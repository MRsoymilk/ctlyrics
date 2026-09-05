use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, RecvTimeoutError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use ksni::{Icon, MenuItem, Tray, blocking::TrayMethods, menu::StandardItem};

use crate::i18n::{Locale, tr};

use super::{ExitSignal, PlaybackInfo, icon::argb_icon, marquee_text};

const MENU_COLUMNS: usize = 42;
const SCROLL_INTERVAL: Duration = Duration::from_millis(220);
const SCROLL_ACTIVE_MILLIS: u64 = 60_000;

struct TrayItem {
    exit: ExitSignal,
    quit_label: String,
    icon: Vec<u8>,
    playback: PlaybackInfo,
    scroll_step: usize,
    scroll_active_until: Arc<AtomicU64>,
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

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let playback = self.playback.snapshot();
        vec![
            StandardItem {
                label: marquee_text(&playback.line, MENU_COLUMNS, self.scroll_step)
                    .replace('_', "__"),
                enabled: false,
                ..Default::default()
            }
            .into(),
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
}

enum Command {
    Refresh,
    Shutdown,
}

impl SniTray {
    pub fn start(locale: Locale, exit: ExitSignal, playback: PlaybackInfo) -> Result<Self> {
        let scroll_active_until = Arc::new(AtomicU64::new(0));
        let item = TrayItem {
            exit,
            quit_label: tr(locale, "tray_quit").to_string(),
            icon: argb_icon(32)?,
            playback,
            scroll_step: 0,
            scroll_active_until: scroll_active_until.clone(),
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
                            let _ = handle.update(|item| item.scroll_step = 0);
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
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
