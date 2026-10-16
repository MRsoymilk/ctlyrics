//! SNI transport for the application's own Wayland context menu.
//! Do not advertise a DBusMenu `Menu` property: hosts should call ContextMenu,
//! rather than building a native menu that cannot deliver slider motion events.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use zbus::blocking::{Connection, Proxy, connection::Builder};

use super::{PlaybackInfo, icon::argb_icon, sni::Command, wayland::WaylandBubbleSender};

const PATH: &str = "/StatusNotifierItem";
const INTERFACE: &str = "org.kde.StatusNotifierItem";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const BUS_TIMEOUT: Duration = Duration::from_millis(500);
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

struct Item {
    playback: PlaybackInfo,
    wayland: WaylandBubbleSender,
    icon: Vec<u8>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn context_menu(&self, x: i32, y: i32) -> zbus::fdo::Result<()> {
        if self.wayland.context_menu(x, y) {
            Ok(())
        } else {
            Err(zbus::fdo::Error::Failed(
                "Wayland menu is unavailable".into(),
            ))
        }
    }

    fn activate(&self, _x: i32, _y: i32) {
        self.wayland.toggle(self.playback.snapshot().lyric);
    }

    fn secondary_activate(&self, x: i32, y: i32) -> zbus::fdo::Result<()> {
        self.context_menu(x, y)
    }

    fn scroll(&self, delta: i32, _orientation: &str) {
        if delta != 0 {
            self.wayland.set_vertical(delta < 0);
        }
    }

    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "ctlyrics"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "ctlyrics"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        vec![(32, 32, self.icon.clone())]
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }

    #[zbus(property, name = "ToolTip")]
    fn tool_tip(&self) -> (String, Pixmaps, String, String) {
        (
            String::new(),
            Vec::new(),
            "ctlyrics".into(),
            self.playback.snapshot().line,
        )
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

pub(super) fn start(
    playback: PlaybackInfo,
    wayland: WaylandBubbleSender,
) -> Result<(Sender<Command>, JoinHandle<()>)> {
    let (commands, receiver) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name("ctlyrics-custom-sni".into())
        .spawn(move || {
            let initialize = || -> Result<(Connection, String)> {
                let item = Item {
                    playback: playback.clone(),
                    wayland: wayland.clone(),
                    icon: argb_icon(32)?,
                };
                let connection = Builder::session()?
                    .method_timeout(BUS_TIMEOUT)
                    .serve_at(PATH, item)?
                    .build()?;
                let owner = watcher_owner(&connection)?;
                register(&connection)?;
                Ok((connection, owner))
            };
            let (connection, mut owner) = match initialize() {
                Ok(value) => value,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            if ready_tx.send(Ok(())).is_err() {
                return;
            }
            let mut last_lyric = playback.snapshot().lyric;
            let mut next_watch = Instant::now() + WATCH_INTERVAL;
            loop {
                let timeout = next_watch.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(timeout) {
                    Ok(Command::Refresh) => {
                        let lyric = playback.snapshot().lyric;
                        if lyric != last_lyric {
                            wayland.update(lyric.clone());
                            last_lyric = lyric;
                        }
                        let _ = connection.emit_signal(
                            None::<&str>,
                            PATH,
                            INTERFACE,
                            "NewToolTip",
                            &(),
                        );
                    }
                    Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if Instant::now() >= next_watch {
                    match watcher_owner(&connection) {
                        Ok(current) if current != owner => {
                            if register(&connection).is_ok() {
                                owner = current;
                            }
                        }
                        Err(_) => owner.clear(),
                        _ => {}
                    }
                    next_watch = Instant::now() + WATCH_INTERVAL;
                }
            }
            // Closing the connection unregisters this item from the watcher.
            let _ = connection.close();
        })
        .context("failed to start custom SNI thread")?;
    match ready_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => Ok((commands, thread)),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(error) => {
            let _ = commands.send(Command::Shutdown);
            // Do not block startup if connecting to the bus itself is stuck.
            Err(error).context("custom SNI initialization timed out")
        }
    }
}

fn watcher_owner(connection: &Connection) -> Result<String> {
    let proxy = Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    Ok(proxy.call("GetNameOwner", &(WATCHER,))?)
}

fn register(connection: &Connection) -> Result<()> {
    let proxy = Proxy::new(connection, WATCHER, WATCHER_PATH, WATCHER)?;
    let name = connection
        .unique_name()
        .context("session bus has no unique name")?
        .as_str();
    proxy.call::<_, _, ()>("RegisterStatusNotifierItem", &(name,))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::wayland::Command as WaylandCommand;
    use super::*;
    use crate::i18n::Locale;

    struct Watcher(Sender<String>);

    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl Watcher {
        fn register_status_notifier_item(&self, service: &str) {
            self.0.send(service.into()).unwrap();
        }
    }

    #[test]
    #[ignore = "run under dbus-run-session to avoid registering a test item on the desktop bus"]
    fn right_click_calls_the_custom_menu_without_a_dbusmenu() {
        let (registered_tx, registered_rx) = mpsc::channel();
        let host = Builder::session()
            .unwrap()
            .name(WATCHER)
            .unwrap()
            .serve_at(WATCHER_PATH, Watcher(registered_tx))
            .unwrap()
            .build()
            .unwrap();
        let (wayland, events) = WaylandBubbleSender::test_channel();
        let playback = PlaybackInfo::new(Locale::En);
        let (commands, worker) = start(playback.clone(), wayland).unwrap();
        let service = registered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let client = Builder::session()
            .unwrap()
            .method_timeout(BUS_TIMEOUT)
            .build()
            .unwrap();
        let item = Proxy::new(&client, service.as_str(), PATH, INTERFACE).unwrap();
        assert_eq!(item.get_property::<String>("Id").unwrap(), "ctlyrics");
        assert!(
            item.get_property::<zbus::zvariant::OwnedObjectPath>("Menu")
                .is_err()
        );
        let pixmaps = item.get_property::<Pixmaps>("IconPixmap").unwrap();
        assert_eq!(
            (pixmaps[0].0, pixmaps[0].1, pixmaps[0].2.len()),
            (32, 32, 32 * 32 * 4)
        );
        item.call::<_, _, ()>("ContextMenu", &(120, 28)).unwrap();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            WaylandCommand::ShowControls(Some((120, 28)))
        ));
        assert!(events.try_recv().is_err());
        item.call::<_, _, ()>("Activate", &(120, 28)).unwrap();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            WaylandCommand::Toggle(_)
        ));
        playback.update("Song".into(), "Song", "New lyric", "playing", 1, 180);
        commands.send(Command::Refresh).unwrap();
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(1)).unwrap(),
            WaylandCommand::Update(lyric) if lyric == "New lyric")
        );
        playback.update("Song".into(), "Song", "New lyric", "playing", 2, 180);
        commands.send(Command::Refresh).unwrap();
        // Progress changes must not resend lyrics or restart their animation.
        assert!(events.recv_timeout(Duration::from_millis(100)).is_err());
        host.release_name(WATCHER).unwrap();
        host.close().unwrap();
        let (registered_tx, registered_rx) = mpsc::channel();
        let replacement_host = Builder::session()
            .unwrap()
            .name(WATCHER)
            .unwrap()
            .serve_at(WATCHER_PATH, Watcher(registered_tx))
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            registered_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            service
        );
        commands.send(Command::Shutdown).unwrap();
        worker.join().unwrap();
        let bus = Proxy::new(
            &client,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while bus
            .call::<_, _, bool>("NameHasOwner", &(service.as_str(),))
            .unwrap()
        {
            assert!(Instant::now() < deadline, "tray connection was not closed");
            thread::sleep(Duration::from_millis(10));
        }
        drop(replacement_host);
    }
}
