use std::fs;
use std::process::Command;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use fontdue::{
    Font, FontSettings,
    layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle},
};
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

use super::{ExitSignal, PlaybackInfo, icon::rgba_icon};

const ICON_SIZE: u16 = 24;
const MENU_WIDTH: u16 = 360;
const MENU_HEIGHT: u16 = 58;
const INFO_HEIGHT: u16 = 30;
const MENU_PADDING: i32 = 12;
const FONT_SIZE: f32 = 14.0;
const SCROLL_GAP: i32 = 48;
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

struct RasterizedText {
    width: usize,
    height: usize,
    alpha: Vec<u8>,
}

struct MenuContent {
    playback: RasterizedText,
    quit: RasterizedText,
}

impl MenuContent {
    fn new(font: &Font, playback: &str, quit_label: &str) -> Self {
        Self {
            playback: rasterize_text(font, playback),
            quit: rasterize_text(font, quit_label),
        }
    }
}

pub struct XEmbedTray {
    stop: ExitSignal,
    thread: Option<JoinHandle<()>>,
}

impl XEmbedTray {
    pub fn start(locale: Locale, exit: ExitSignal, playback: PlaybackInfo) -> Result<Self> {
        if std::env::var_os("DISPLAY").is_none() {
            return Err(anyhow!("DISPLAY is not set"));
        }

        let stop = ExitSignal::new();
        let thread_stop = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("ctlyrics-xembed-tray".to_string())
            .spawn(move || {
                let result = run_tray(locale, exit, playback, thread_stop, ready_tx);
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
    playback: PlaybackInfo,
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

    let quit_label = tr(locale, "tray_quit");
    let mut playback_snapshot = playback.snapshot();
    let mut menu_font = load_menu_font(&format!("{} {quit_label}", playback_snapshot.line));
    let mut menu_content = menu_font
        .as_ref()
        .map(|font| MenuContent::new(font, &playback_snapshot.line, quit_label));
    let mut icon_width = ICON_SIZE;
    let mut icon_height = ICON_SIZE;
    let mut menu_open = false;
    let mut menu_opened_at = Instant::now();

    while !stop.is_requested() && !exit.is_requested() {
        let current_snapshot = playback.snapshot();
        if current_snapshot.revision != playback_snapshot.revision {
            playback_snapshot = current_snapshot;
            if menu_font
                .as_ref()
                .is_none_or(|font| !font_supports(font, &playback_snapshot.line))
            {
                menu_font = load_menu_font(&format!("{} {quit_label}", playback_snapshot.line));
            }
            menu_content = menu_font
                .as_ref()
                .map(|font| MenuContent::new(font, &playback_snapshot.line, quit_label));
            menu_opened_at = Instant::now();
        }
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
                    draw_menu(
                        &connection,
                        menu,
                        menu_gc,
                        black,
                        white,
                        pixel_format,
                        menu_content.as_ref(),
                        &playback_snapshot.line,
                        menu_opened_at.elapsed(),
                    )?;
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
                    menu_opened_at = Instant::now();
                    draw_menu(
                        &connection,
                        menu,
                        menu_gc,
                        black,
                        white,
                        pixel_format,
                        menu_content.as_ref(),
                        &playback_snapshot.line,
                        menu_opened_at.elapsed(),
                    )?;
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
                    let quit_clicked = event.event_y >= INFO_HEIGHT as i16;
                    close_menu(&connection, menu)?;
                    menu_open = false;
                    if event.detail == 1 && inside && quit_clicked {
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
        if menu_open {
            draw_menu(
                &connection,
                menu,
                menu_gc,
                black,
                white,
                pixel_format,
                menu_content.as_ref(),
                &playback_snapshot.line,
                menu_opened_at.elapsed(),
            )?;
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
    let rgba = rgba_icon(u32::from(width), u32::from(height))?;
    native_x11_pixels(&rgba, width, height, format)
}

fn native_x11_pixels(rgba: &[u8], width: u16, height: u16, format: PixelFormat) -> Result<Vec<u8>> {
    let bytes_per_pixel = match format.bits_per_pixel {
        16 => 2,
        24 => 3,
        32 => 4,
        value => return Err(anyhow!("unsupported X11 pixel width: {value}")),
    };
    let pad = usize::from(format.scanline_pad);
    let row_bits = usize::from(width) * usize::from(format.bits_per_pixel);
    let row_bytes = row_bits.div_ceil(pad) * pad / 8;
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

#[allow(clippy::too_many_arguments)]
fn draw_menu(
    connection: &RustConnection,
    window: Window,
    gc: u32,
    black: u32,
    white: u32,
    format: PixelFormat,
    content: Option<&MenuContent>,
    playback: &str,
    elapsed: Duration,
) -> Result<()> {
    if let Some(content) = content {
        let rgba = render_menu(content, elapsed);
        let image = native_x11_pixels(&rgba, MENU_WIDTH, MENU_HEIGHT, format)?;
        connection.put_image(
            ImageFormat::Z_PIXMAP,
            window,
            gc,
            MENU_WIDTH,
            MENU_HEIGHT,
            0,
            0,
            0,
            format.depth,
            &image,
        )?;
        connection.flush()?;
        return Ok(());
    }

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
    let fallback: String = playback
        .chars()
        .map(
            |character| {
                if character.is_ascii() { character } else { ' ' }
            },
        )
        .collect();
    let fallback = fallback.trim().as_bytes();
    connection.image_text8(window, gc, 12, 19, &fallback[..fallback.len().min(54)])?;
    connection.image_text8(window, gc, 12, 49, b"Quit")?;
    connection.flush()?;
    Ok(())
}

fn load_menu_font(sample: &str) -> Option<Font> {
    let pattern = sample
        .chars()
        .find(|character| !character.is_ascii() && !character.is_whitespace())
        .map(|character| format!(":charset={:x}", character as u32))
        .unwrap_or_else(|| "sans-serif".to_string());
    let matched = Command::new("fc-match")
        .args(["-f", "%{file}\t%{index}\n", &pattern])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| {
            let (path, index) = output.lines().next()?.split_once('\t')?;
            Some((path.to_string(), index.parse().unwrap_or(0)))
        });
    let fonts = matched.into_iter().chain([
        (
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".to_string(),
            0,
        ),
        ("/usr/share/fonts/TTF/DejaVuSans.ttf".to_string(), 0),
    ]);

    for (path, collection_index) in fonts {
        let Ok(data) = fs::read(path) else {
            continue;
        };
        let settings = FontSettings {
            collection_index,
            ..Default::default()
        };
        if let Ok(font) = Font::from_bytes(data, settings) {
            return Some(font);
        }
    }
    None
}

fn font_supports(font: &Font, text: &str) -> bool {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .all(|character| font.lookup_glyph_index(character) != 0)
}

fn render_menu(content: &MenuContent, elapsed: Duration) -> Vec<u8> {
    let mut pixels = vec![0; usize::from(MENU_WIDTH) * usize::from(MENU_HEIGHT) * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }

    let viewport_width = i32::from(MENU_WIDTH) - MENU_PADDING * 2;
    let text_width = content.playback.width as i32;
    let scroll = if text_width > viewport_width {
        (elapsed.as_millis() / 35) as i32 % (text_width + SCROLL_GAP)
    } else {
        0
    };
    blit_scrolling_text(&mut pixels, &content.playback, scroll, 5, [210, 210, 210]);

    let separator = usize::from(INFO_HEIGHT) * usize::from(MENU_WIDTH) * 4;
    for x in 0..usize::from(MENU_WIDTH) {
        let offset = separator + x * 4;
        pixels[offset..offset + 3].copy_from_slice(&[70, 70, 70]);
    }
    blit_text(
        &mut pixels,
        &content.quit,
        MENU_PADDING,
        i32::from(INFO_HEIGHT) + 4,
        [255, 255, 255],
    );
    pixels
}

fn rasterize_text(font: &Font, text: &str) -> RasterizedText {
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.reset(&LayoutSettings::default());
    layout.append(&[font], &TextStyle::new(text, FONT_SIZE, 0));
    let width = layout
        .glyphs()
        .iter()
        .map(|glyph| glyph.x.ceil() as usize + glyph.width)
        .max()
        .unwrap_or(0);
    let height = layout
        .glyphs()
        .iter()
        .map(|glyph| glyph.y.ceil() as usize + glyph.height)
        .max()
        .unwrap_or(0);
    let mut alpha = vec![0; width * height];
    for glyph in layout.glyphs() {
        let (_, bitmap) = font.rasterize_config(glyph.key);
        for y in 0..glyph.height {
            let destination_y = glyph.y.ceil() as usize + y;
            for x in 0..glyph.width {
                let destination_x = glyph.x.ceil() as usize + x;
                let destination = destination_y * width + destination_x;
                alpha[destination] = alpha[destination].max(bitmap[y * glyph.width + x]);
            }
        }
    }
    RasterizedText {
        width,
        height,
        alpha,
    }
}

fn blit_scrolling_text(
    pixels: &mut [u8],
    text: &RasterizedText,
    scroll: i32,
    offset_y: i32,
    color: [u8; 3],
) {
    let viewport_width = i32::from(MENU_WIDTH) - MENU_PADDING * 2;
    let cycle_width = text.width as i32 + SCROLL_GAP;
    for destination_x in 0..viewport_width {
        let source_x = if text.width as i32 > viewport_width {
            (scroll + destination_x) % cycle_width
        } else {
            destination_x
        };
        if source_x < text.width as i32 {
            blit_column(
                pixels,
                text,
                source_x as usize,
                MENU_PADDING + destination_x,
                offset_y,
                0,
                INFO_HEIGHT,
                color,
            );
        }
    }
}

fn blit_text(
    pixels: &mut [u8],
    text: &RasterizedText,
    offset_x: i32,
    offset_y: i32,
    color: [u8; 3],
) {
    for source_x in 0..text.width {
        blit_column(
            pixels,
            text,
            source_x,
            offset_x + source_x as i32,
            offset_y,
            INFO_HEIGHT,
            MENU_HEIGHT,
            color,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_column(
    pixels: &mut [u8],
    text: &RasterizedText,
    source_x: usize,
    destination_x: i32,
    offset_y: i32,
    clip_top: u16,
    clip_bottom: u16,
    color: [u8; 3],
) {
    if destination_x < MENU_PADDING || destination_x >= i32::from(MENU_WIDTH) - MENU_PADDING {
        return;
    }
    for source_y in 0..text.height {
        let destination_y = offset_y + source_y as i32;
        if destination_y < i32::from(clip_top) || destination_y >= i32::from(clip_bottom) {
            continue;
        }
        let alpha = u16::from(text.alpha[source_y * text.width + source_x]);
        let offset =
            (destination_y as usize * usize::from(MENU_WIDTH) + destination_x as usize) * 4;
        for channel in 0..3 {
            pixels[offset + channel] = (u16::from(color[channel]) * alpha / 255) as u8;
        }
    }
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
