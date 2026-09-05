use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use x11rb::CURRENT_TIME;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, ChangeGCAux, ChangeWindowAttributesAux, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, GrabMode, ImageFormat, ImageOrder,
    PropMode, Rectangle, StackMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::i18n::{Locale, tr};

use super::{ExitSignal, icon::rgba_icon};

const ICON_SIZE: u16 = 24;
const MENU_WIDTH: u16 = 104;
const MENU_HEIGHT: u16 = 28;
const SYSTEM_TRAY_REQUEST_DOCK: u32 = 0;
const XEMBED_MAPPED: u32 = 1;

#[derive(Clone, Copy)]
struct PixelFormat {
    depth: u8,
    bits_per_pixel: u8,
    scanline_pad: u8,
    byte_order: ImageOrder,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
}

pub struct XEmbedTray {
    stop: ExitSignal,
    thread: Option<JoinHandle<()>>,
}

impl XEmbedTray {
    pub fn start(locale: Locale, exit: ExitSignal) -> Result<Self> {
        if std::env::var_os("DISPLAY").is_none() {
            return Err(anyhow!("DISPLAY is not set"));
        }

        let stop = ExitSignal::new();
        let thread_stop = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("ctlyrics-xembed-tray".to_string())
            .spawn(move || {
                let result = run_tray(locale, exit, thread_stop, ready_tx);
                if let Err(error) = result {
                    tracing::debug!(%error, "XEmbed tray stopped");
                }
            })
            .context("failed to start XEmbed tray thread")?;

        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .context("XEmbed tray initialization timed out")??;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn shutdown(mut self) {
        self.stop.request();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn atom(connection: &RustConnection, name: &[u8]) -> Result<Atom> {
    Ok(connection.intern_atom(false, name)?.reply()?.atom)
}

fn dock(
    connection: &RustConnection,
    selection: Atom,
    tray_opcode: Atom,
    icon: Window,
) -> Result<Window> {
    let owner = connection.get_selection_owner(selection)?.reply()?.owner;
    if owner == x11rb::NONE {
        return Err(anyhow!("no XEmbed system tray manager"));
    }
    connection.change_window_attributes(
        owner,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
    )?;
    let request = ClientMessageEvent::new(
        32,
        icon,
        tray_opcode,
        [CURRENT_TIME, SYSTEM_TRAY_REQUEST_DOCK, icon, 0, 0],
    );
    connection.send_event(false, owner, EventMask::NO_EVENT, request)?;
    connection.flush()?;
    Ok(owner)
}

fn run_tray(
    locale: Locale,
    exit: ExitSignal,
    stop: ExitSignal,
    ready: std::sync::mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let (connection, screen_number) = match RustConnection::connect(None) {
        Ok(value) => value,
        Err(error) => {
            let message = error.to_string();
            let _ = ready.send(Err(anyhow!(message.clone())));
            return Err(anyhow!(message));
        }
    };
    let screen = &connection.setup().roots[screen_number];
    let root = screen.root;
    let depth = screen.root_depth;
    let visual = screen.root_visual;
    let black = screen.black_pixel;
    let white = screen.white_pixel;
    let pixmap_format = connection
        .setup()
        .pixmap_formats
        .iter()
        .find(|format| format.depth == depth)
        .context("missing X11 pixmap format for root depth")?;
    let visual_format = screen
        .allowed_depths
        .iter()
        .flat_map(|allowed| allowed.visuals.iter())
        .find(|candidate| candidate.visual_id == visual)
        .context("missing X11 root visual")?;
    let pixel_format = PixelFormat {
        depth,
        bits_per_pixel: pixmap_format.bits_per_pixel,
        scanline_pad: pixmap_format.scanline_pad,
        byte_order: connection.setup().image_byte_order,
        red_mask: visual_format.red_mask,
        green_mask: visual_format.green_mask,
        blue_mask: visual_format.blue_mask,
    };
    let selection = atom(
        &connection,
        format!("_NET_SYSTEM_TRAY_S{screen_number}").as_bytes(),
    )?;
    let tray_opcode = atom(&connection, b"_NET_SYSTEM_TRAY_OPCODE")?;
    let xembed_info = atom(&connection, b"_XEMBED_INFO")?;
    let manager = atom(&connection, b"MANAGER")?;

    connection.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
    )?;

    let icon = connection.generate_id()?;
    connection.create_window(
        depth,
        icon,
        root,
        0,
        0,
        ICON_SIZE,
        ICON_SIZE,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &CreateWindowAux::new()
            .background_pixel(black)
            .border_pixel(black)
            .override_redirect(1)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::STRUCTURE_NOTIFY,
            ),
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        icon,
        xembed_info,
        xembed_info,
        &[0, XEMBED_MAPPED],
    )?;

    let menu = connection.generate_id()?;
    connection.create_window(
        depth,
        menu,
        root,
        0,
        0,
        MENU_WIDTH,
        MENU_HEIGHT,
        1,
        WindowClass::INPUT_OUTPUT,
        visual,
        &CreateWindowAux::new()
            .background_pixel(black)
            .border_pixel(white)
            .override_redirect(1)
            .save_under(1)
            .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS),
    )?;

    let icon_gc = connection.generate_id()?;
    connection.create_gc(
        icon_gc,
        icon,
        &CreateGCAux::new().foreground(white).background(black),
    )?;
    let menu_gc = connection.generate_id()?;
    let font = connection.generate_id()?;
    connection.open_font(font, b"fixed")?;
    connection.create_gc(
        menu_gc,
        menu,
        &CreateGCAux::new()
            .foreground(white)
            .background(black)
            .font(font),
    )?;

    let mut tray_owner = match dock(&connection, selection, tray_opcode, icon) {
        Ok(owner) => owner,
        Err(error) => {
            let message = error.to_string();
            let _ = ready.send(Err(anyhow!(message.clone())));
            return Err(anyhow!(message));
        }
    };
    draw_icon(
        &connection,
        icon,
        icon_gc,
        ICON_SIZE,
        ICON_SIZE,
        pixel_format,
    )?;
    connection.flush()?;
    let _ = ready.send(Ok(()));

    let translated_label = tr(locale, "tray_quit");
    let menu_label = if translated_label.is_ascii() {
        translated_label.as_bytes()
    } else {
        b"Quit"
    };
    let mut icon_width = ICON_SIZE;
    let mut icon_height = ICON_SIZE;
    let mut menu_open = false;

    while !stop.is_requested() && !exit.is_requested() {
        while let Some(event) = connection.poll_for_event()? {
            match event {
                Event::Expose(event) if event.window == icon && event.count == 0 => {
                    draw_icon(
                        &connection,
                        icon,
                        icon_gc,
                        icon_width,
                        icon_height,
                        pixel_format,
                    )?;
                }
                Event::Expose(event) if event.window == menu && event.count == 0 => {
                    draw_menu(&connection, menu, menu_gc, black, white, menu_label)?;
                }
                Event::ConfigureNotify(event) if event.window == icon => {
                    icon_width = event.width;
                    icon_height = event.height;
                    draw_icon(
                        &connection,
                        icon,
                        icon_gc,
                        icon_width,
                        icon_height,
                        pixel_format,
                    )?;
                }
                Event::ButtonPress(event) if event.event == icon && event.detail == 3 => {
                    open_menu(
                        &connection,
                        menu,
                        event.root_x,
                        event.root_y,
                        screen.width_in_pixels,
                        screen.height_in_pixels,
                    )?;
                    menu_open = true;
                }
                Event::ButtonPress(event) if menu_open => {
                    let inside = event.event_x >= 0
                        && event.event_y >= 0
                        && event.event_x < MENU_WIDTH as i16
                        && event.event_y < MENU_HEIGHT as i16;
                    close_menu(&connection, menu)?;
                    menu_open = false;
                    if event.detail == 1 && inside {
                        exit.request();
                    }
                }
                Event::ClientMessage(event) if event.type_ == manager => {
                    let data = event.data.as_data32();
                    if event.window == root && data[1] == selection {
                        tray_owner = dock(&connection, selection, tray_opcode, icon)?;
                    }
                }
                Event::DestroyNotify(event) if event.window == tray_owner => {
                    tray_owner = x11rb::NONE;
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(20));
    }

    if menu_open {
        let _ = connection.ungrab_pointer(CURRENT_TIME);
    }
    let _ = connection.destroy_window(menu);
    let _ = connection.destroy_window(icon);
    let _ = connection.free_gc(menu_gc);
    let _ = connection.free_gc(icon_gc);
    let _ = connection.close_font(font);
    let _ = connection.flush();
    Ok(())
}

fn draw_icon(
    connection: &RustConnection,
    window: Window,
    gc: u32,
    width: u16,
    height: u16,
    format: PixelFormat,
) -> Result<()> {
    let image = native_x11_image(width, height, format)?;
    connection.put_image(
        ImageFormat::Z_PIXMAP,
        window,
        gc,
        width,
        height,
        0,
        0,
        0,
        format.depth,
        &image,
    )?;
    connection.flush()?;
    Ok(())
}

fn native_x11_image(width: u16, height: u16, format: PixelFormat) -> Result<Vec<u8>> {
    let bytes_per_pixel = match format.bits_per_pixel {
        16 => 2,
        24 => 3,
        32 => 4,
        value => return Err(anyhow!("unsupported X11 pixel width: {value}")),
    };
    let pad = usize::from(format.scanline_pad);
    let row_bits = usize::from(width) * usize::from(format.bits_per_pixel);
    let row_bytes = row_bits.div_ceil(pad) * pad / 8;
    let rgba = rgba_icon(u32::from(width), u32::from(height))?;
    let mut output = vec![0; row_bytes * usize::from(height)];

    for (index, rgba_pixel) in rgba.as_chunks::<4>().0.iter().enumerate() {
        let alpha = u32::from(rgba_pixel[3]);
        let red = u32::from(rgba_pixel[0]) * alpha / 255;
        let green = u32::from(rgba_pixel[1]) * alpha / 255;
        let blue = u32::from(rgba_pixel[2]) * alpha / 255;
        let pixel = encode_component(red, format.red_mask)
            | encode_component(green, format.green_mask)
            | encode_component(blue, format.blue_mask);
        let row = index / usize::from(width);
        let column = index % usize::from(width);
        let offset = row * row_bytes + column * bytes_per_pixel;
        let encoded = if format.byte_order == ImageOrder::LSB_FIRST {
            pixel.to_le_bytes()
        } else {
            pixel.to_be_bytes()
        };
        if format.byte_order == ImageOrder::LSB_FIRST {
            output[offset..offset + bytes_per_pixel].copy_from_slice(&encoded[..bytes_per_pixel]);
        } else {
            output[offset..offset + bytes_per_pixel]
                .copy_from_slice(&encoded[4 - bytes_per_pixel..]);
        }
    }
    Ok(output)
}

fn encode_component(value: u32, mask: u32) -> u32 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let maximum = mask >> shift;
    (((value * maximum + 127) / 255) << shift) & mask
}

fn draw_menu(
    connection: &RustConnection,
    window: Window,
    gc: u32,
    black: u32,
    white: u32,
    label: &[u8],
) -> Result<()> {
    connection.change_gc(gc, &ChangeGCAux::new().foreground(black).background(black))?;
    connection.poly_fill_rectangle(
        window,
        gc,
        &[Rectangle {
            x: 0,
            y: 0,
            width: MENU_WIDTH,
            height: MENU_HEIGHT,
        }],
    )?;
    connection.change_gc(gc, &ChangeGCAux::new().foreground(white).background(black))?;
    connection.image_text8(window, gc, 12, 19, label)?;
    connection.flush()?;
    Ok(())
}

fn open_menu(
    connection: &RustConnection,
    menu: Window,
    root_x: i16,
    root_y: i16,
    screen_width: u16,
    screen_height: u16,
) -> Result<()> {
    let x = (root_x as i32).clamp(0, screen_width.saturating_sub(MENU_WIDTH) as i32);
    let y = (root_y as i32).clamp(0, screen_height.saturating_sub(MENU_HEIGHT) as i32);
    connection.configure_window(
        menu,
        &ConfigureWindowAux::new()
            .x(x)
            .y(y)
            .stack_mode(StackMode::ABOVE),
    )?;
    connection.map_window(menu)?;
    connection
        .grab_pointer(
            false,
            menu,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            CURRENT_TIME,
        )?
        .reply()?;
    connection.flush()?;
    Ok(())
}

fn close_menu(connection: &RustConnection, menu: Window) -> Result<()> {
    connection.ungrab_pointer(CURRENT_TIME)?;
    connection.unmap_window(menu)?;
    connection.flush()?;
    Ok(())
}
