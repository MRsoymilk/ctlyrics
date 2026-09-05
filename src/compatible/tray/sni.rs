use anyhow::{Context, Result, anyhow};
use ksni::{Icon, MenuItem, Tray, blocking::TrayMethods, menu::StandardItem};

use crate::i18n::{Locale, tr};

use super::{ExitSignal, icon::argb_icon};

struct TrayItem {
    exit: ExitSignal,
    quit_label: String,
    icon: Vec<u8>,
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
        vec![
            StandardItem {
                label: self.quit_label.clone(),
                activate: Box::new(|tray: &mut Self| tray.exit.request()),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub struct SniTray {
    handle: ksni::blocking::Handle<TrayItem>,
}

impl SniTray {
    pub fn start(locale: Locale, exit: ExitSignal) -> Result<Self> {
        let item = TrayItem {
            exit,
            quit_label: tr(locale, "tray_quit").to_string(),
            icon: argb_icon(32)?,
        };
        let handle = std::thread::spawn(move || item.spawn())
            .join()
            .map_err(|_| anyhow!("SNI initialization thread panicked"))?
            .context("failed to register SNI tray")?;
        Ok(Self { handle })
    }

    pub fn shutdown(self) {
        let _ = std::thread::spawn(move || self.handle.shutdown().wait()).join();
    }
}
