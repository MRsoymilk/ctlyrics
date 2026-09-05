use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::i18n::Locale;

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
    #[cfg(target_os = "linux")]
    backend: Option<Backend>,
}

#[cfg(target_os = "linux")]
enum Backend {
    Sni(sni::SniTray),
    XEmbed(xembed::XEmbedTray),
}

impl TrayService {
    pub fn start(locale: Locale, exit: ExitSignal) -> Self {
        #[cfg(target_os = "linux")]
        {
            match sni::SniTray::start(locale, exit.clone()) {
                Ok(tray) => {
                    tracing::info!("using StatusNotifierItem tray backend");
                    return Self {
                        backend: Some(Backend::Sni(tray)),
                    };
                }
                Err(error) => tracing::debug!(%error, "StatusNotifierItem unavailable"),
            }

            match xembed::XEmbedTray::start(locale, exit) {
                Ok(tray) => {
                    tracing::info!("using XEmbed tray backend");
                    Self {
                        backend: Some(Backend::XEmbed(tray)),
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "system tray unavailable");
                    Self { backend: None }
                }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (locale, exit);
            Self {}
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
