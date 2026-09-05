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
    Atom, AtomEnum, ChangeGCAux, ChangeWindowAttributesAux, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, GrabMode, ImageFormat, ImageOrder,
    PropMode, Rectangle, StackMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::cmus::{PlaybackCommand, control_cmus, seek_cmus};
use crate::i18n::{Locale, tr};

use super::{
    ExitSignal, PlaybackInfo, PlaybackSnapshot, icon::rgba_icon, progress_offset,
    seek_from_progress,
};

const ICON_SIZE: u16 = 24;
const MENU_WIDTH: u16 = 360;
const MENU_HEIGHT: u16 = 130;
const MENU_PADDING: i32 = 12;
const FONT_SIZE: f32 = 14.0;
const SCROLL_GAP: i32 = 48;
const PANEL_X: i16 = 8;
const PANEL_Y: i16 = 7;
const PANEL_WIDTH: u16 = 344;
const PANEL_HEIGHT: u16 = 84;
const TITLE_TOP: u16 = 9;
const TITLE_BOTTOM: u16 = 36;
const PROGRESS_X: i16 = 34;
const PROGRESS_Y: i16 = 43;
const PROGRESS_WIDTH: u16 = 292;
const PROGRESS_HEIGHT: u16 = 5;
const CONTROL_TOP: i16 = 54;
const CONTROL_HEIGHT: u16 = 31;
const CONTROL_WIDTH: u16 = 44;
const PREVIOUS_X: i16 = 108;
const TOGGLE_X: i16 = 158;
const NEXT_X: i16 = 208;
const SEPARATOR_Y: u16 = 97;
const QUIT_TOP: u16 = 98;
const SYSTEM_TRAY_REQUEST_DOCK: u32 = 0;
const XEMBED_MAPPED: u32 = 1;
const XK_ESCAPE: u32 = 0xff1b;
const BUBBLE_PADDING: usize = 14;
const BUBBLE_MAX_WIDTH: u16 = 520;
const BUBBLE_MARGIN: i32 = 10;
const BUBBLE_FRAME_INTERVAL: Duration = Duration::from_millis(40);

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuTarget {
    Previous,
    Toggle,
    Next,
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LyricOrientation {
    Horizontal,
    Vertical,
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
    let net_wm_window_type = atom(&connection, b"_NET_WM_WINDOW_TYPE")?;
    let net_wm_window_type_notification = atom(&connection, b"_NET_WM_WINDOW_TYPE_NOTIFICATION")?;
    let net_wm_state = atom(&connection, b"_NET_WM_STATE")?;
    let net_wm_state_above = atom(&connection, b"_NET_WM_STATE_ABOVE")?;
    let net_wm_state_sticky = atom(&connection, b"_NET_WM_STATE_STICKY")?;
    let net_wm_state_skip_taskbar = atom(&connection, b"_NET_WM_STATE_SKIP_TASKBAR")?;
    let net_wm_state_skip_pager = atom(&connection, b"_NET_WM_STATE_SKIP_PAGER")?;

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

    let bubble = connection.generate_id()?;
    connection.create_window(
        depth,
        bubble,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &CreateWindowAux::new()
            .background_pixel(black)
            .border_pixel(black)
            .override_redirect(1)
            .save_under(1)
            .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS),
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        bubble,
        net_wm_window_type,
        AtomEnum::ATOM,
        &[net_wm_window_type_notification],
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        bubble,
        net_wm_state,
        AtomEnum::ATOM,
        &[
            net_wm_state_above,
            net_wm_state_sticky,
            net_wm_state_skip_taskbar,
            net_wm_state_skip_pager,
        ],
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
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::BUTTON_PRESS
                    | EventMask::KEY_PRESS
                    | EventMask::POINTER_MOTION
                    | EventMask::LEAVE_WINDOW,
            ),
    )?;

    let icon_gc = connection.generate_id()?;
    connection.create_gc(
        icon_gc,
        icon,
        &CreateGCAux::new().foreground(white).background(black),
    )?;
    let menu_gc = connection.generate_id()?;
    let bubble_gc = connection.generate_id()?;
    connection.create_gc(
        bubble_gc,
        bubble,
        &CreateGCAux::new().foreground(white).background(black),
    )?;
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
    let mut menu_font = load_menu_font(&format!(
        "{} {} {quit_label}",
        playback_snapshot.line, playback_snapshot.lyric
    ));
    let mut menu_content = menu_font
        .as_ref()
        .map(|font| MenuContent::new(font, &playback_snapshot.line, quit_label));
    let mut bubble_text = menu_font
        .as_ref()
        .map(|font| rasterize_text(font, &playback_snapshot.lyric));
    let mut icon_width = ICON_SIZE;
    let mut icon_height = ICON_SIZE;
    let mut menu_open = false;
    let mut hovered = None;
    let mut menu_opened_at = Instant::now();
    let mut bubble_visible = false;
    let mut bubble_orientation = LyricOrientation::Horizontal;
    let mut bubble_anchor = (0_i16, 0_i16);
    let mut bubble_started_at = Instant::now();
    let mut last_bubble_draw = Instant::now();
    let mut last_left_click = None;
    let escape_keycodes = escape_keycodes(&connection)?;

    while !stop.is_requested() && !exit.is_requested() {
        let current_snapshot = playback.snapshot();
        if current_snapshot.revision != playback_snapshot.revision {
            if menu_font.as_ref().is_none_or(|font| {
                !font_supports(font, &current_snapshot.line)
                    || !font_supports(font, &current_snapshot.lyric)
            }) {
                menu_font = load_menu_font(&format!(
                    "{} {} {quit_label}",
                    current_snapshot.line, current_snapshot.lyric
                ));
            }
            menu_content = menu_font
                .as_ref()
                .map(|font| MenuContent::new(font, &current_snapshot.line, quit_label));
            bubble_text = menu_font
                .as_ref()
                .map(|font| rasterize_text(font, &current_snapshot.lyric));
            menu_opened_at = Instant::now();
            bubble_started_at = Instant::now();
            last_bubble_draw = Instant::now() - BUBBLE_FRAME_INTERVAL;
        }
        playback_snapshot = current_snapshot;
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
                        &playback_snapshot,
                        menu_opened_at.elapsed(),
                        hovered,
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
                Event::Expose(event) if event.window == bubble && event.count == 0 => {
                    last_bubble_draw = Instant::now() - BUBBLE_FRAME_INTERVAL;
                }
                Event::ButtonPress(event) if event.event == icon && event.detail == 1 => {
                    let duplicate = last_left_click
                        .is_some_and(|last: u32| event.time.wrapping_sub(last) < 200);
                    if !duplicate {
                        last_left_click = Some(event.time);
                        if bubble_visible {
                            hide_bubble(&connection, bubble)?;
                            bubble_visible = false;
                        } else {
                            bubble_anchor = (event.root_x, event.root_y);
                            bubble_started_at = Instant::now();
                            if let Some(text) = bubble_text.as_ref() {
                                draw_lyric_bubble(
                                    &connection,
                                    bubble,
                                    bubble_gc,
                                    pixel_format,
                                    text,
                                    bubble_orientation,
                                    bubble_started_at.elapsed(),
                                    bubble_anchor,
                                    screen.width_in_pixels,
                                    screen.height_in_pixels,
                                )?;
                            }
                            connection.map_window(bubble)?;
                            connection.flush()?;
                            bubble_visible = true;
                        }
                    }
                }
                Event::ButtonPress(event) if event.event == bubble && event.detail == 1 => {
                    hide_bubble(&connection, bubble)?;
                    bubble_visible = false;
                }
                Event::ButtonPress(event)
                    if (event.event == icon || event.event == bubble)
                        && matches!(event.detail, 4 | 5) =>
                {
                    bubble_orientation = match event.detail {
                        4 => LyricOrientation::Horizontal,
                        _ => LyricOrientation::Vertical,
                    };
                    bubble_started_at = Instant::now();
                    last_bubble_draw = Instant::now() - BUBBLE_FRAME_INTERVAL;
                }
                Event::ButtonPress(event) if event.event == icon && event.detail == 3 => {
                    if bubble_visible {
                        hide_bubble(&connection, bubble)?;
                        bubble_visible = false;
                    }
                    menu_opened_at = Instant::now();
                    draw_menu(
                        &connection,
                        menu,
                        menu_gc,
                        black,
                        white,
                        pixel_format,
                        menu_content.as_ref(),
                        &playback_snapshot,
                        menu_opened_at.elapsed(),
                        None,
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
                Event::MotionNotify(event) if menu_open => {
                    hovered = menu_target(event.event_x, event.event_y);
                }
                Event::LeaveNotify(_) if menu_open => {
                    hovered = None;
                }
                Event::KeyPress(event) if menu_open && escape_keycodes.contains(&event.detail) => {
                    close_menu(&connection, menu)?;
                    menu_open = false;
                    hovered = None;
                }
                Event::ButtonPress(event) if menu_open => {
                    let inside = event.event_x >= 0
                        && event.event_y >= 0
                        && event.event_x < MENU_WIDTH as i16
                        && event.event_y < MENU_HEIGHT as i16;
                    if event.detail == 1 && inside {
                        if let Some(position) =
                            seek_position(event.event_x, event.event_y, playback_snapshot.duration)
                        {
                            if let Err(error) = seek_cmus(position) {
                                tracing::warn!(%error, "tray seek failed");
                            }
                        } else if let Some(target) = menu_target(event.event_x, event.event_y) {
                            match target {
                                MenuTarget::Previous => {
                                    control_from_tray(PlaybackCommand::Previous)
                                }
                                MenuTarget::Toggle => {
                                    control_from_tray(PlaybackCommand::TogglePause)
                                }
                                MenuTarget::Next => control_from_tray(PlaybackCommand::Next),
                                MenuTarget::Quit => {
                                    close_menu(&connection, menu)?;
                                    menu_open = false;
                                    hovered = None;
                                    exit.request();
                                }
                            }
                        }
                    } else {
                        close_menu(&connection, menu)?;
                        menu_open = false;
                        hovered = None;
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
                &playback_snapshot,
                menu_opened_at.elapsed(),
                hovered,
            )?;
        }
        if bubble_visible && last_bubble_draw.elapsed() >= BUBBLE_FRAME_INTERVAL {
            if let Some(text) = bubble_text.as_ref() {
                draw_lyric_bubble(
                    &connection,
                    bubble,
                    bubble_gc,
                    pixel_format,
                    text,
                    bubble_orientation,
                    bubble_started_at.elapsed(),
                    bubble_anchor,
                    screen.width_in_pixels,
                    screen.height_in_pixels,
                )?;
            }
            last_bubble_draw = Instant::now();
        }
        thread::sleep(Duration::from_millis(20));
    }

    if menu_open {
        let _ = connection.ungrab_pointer(CURRENT_TIME);
        let _ = connection.ungrab_keyboard(CURRENT_TIME);
    }
    let _ = connection.destroy_window(menu);
    let _ = connection.destroy_window(bubble);
    let _ = connection.destroy_window(icon);
    let _ = connection.free_gc(menu_gc);
    let _ = connection.free_gc(bubble_gc);
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

#[allow(clippy::too_many_arguments)]
fn draw_lyric_bubble(
    connection: &RustConnection,
    window: Window,
    gc: u32,
    format: PixelFormat,
    text: &RasterizedText,
    orientation: LyricOrientation,
    elapsed: Duration,
    anchor: (i16, i16),
    screen_width: u16,
    screen_height: u16,
) -> Result<()> {
    let (rgba, width, height) =
        render_lyric_bubble(text, orientation, elapsed, screen_width, screen_height);
    let max_x = i32::from(screen_width.saturating_sub(width));
    let max_y = i32::from(screen_height.saturating_sub(height));
    let x = (i32::from(anchor.0) - i32::from(width) / 2).clamp(0, max_x);
    let y = if i32::from(anchor.1) < i32::from(screen_height) / 2 {
        (i32::from(anchor.1) + i32::from(ICON_SIZE) + BUBBLE_MARGIN).clamp(0, max_y)
    } else {
        (i32::from(anchor.1) - i32::from(height) - BUBBLE_MARGIN).clamp(0, max_y)
    };
    connection.configure_window(
        window,
        &ConfigureWindowAux::new()
            .x(x)
            .y(y)
            .width(u32::from(width))
            .height(u32::from(height))
            .stack_mode(StackMode::ABOVE),
    )?;
    let image = native_x11_pixels(&rgba, width, height, format)?;
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

fn render_lyric_bubble(
    text: &RasterizedText,
    orientation: LyricOrientation,
    elapsed: Duration,
    screen_width: u16,
    screen_height: u16,
) -> (Vec<u8>, u16, u16) {
    let padding = BUBBLE_PADDING * 2;
    let (width, height) = match orientation {
        LyricOrientation::Horizontal => {
            let limit = usize::from(BUBBLE_MAX_WIDTH.min(screen_width.saturating_sub(16))).max(1);
            (
                (text.width + padding).min(limit).max(limit.min(80)),
                (text.height + padding).max(42),
            )
        }
        LyricOrientation::Vertical => {
            let limit = usize::from(screen_height.saturating_sub(16)).max(1);
            (
                (text.height + padding).max(42),
                (text.width + padding).min(limit).max(limit.min(80)),
            )
        }
    };
    let width = width.min(usize::from(u16::MAX)) as u16;
    let height = height.min(usize::from(u16::MAX)) as u16;
    let mut pixels = vec![0; usize::from(width) * usize::from(height) * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[..3].copy_from_slice(&[10, 16, 20]);
        pixel[3] = 255;
    }
    fill_dynamic_rect(&mut pixels, width, height, 0, 0, width, 1, [70, 230, 158]);
    fill_dynamic_rect(
        &mut pixels,
        width,
        height,
        0,
        height as i16 - 1,
        width,
        1,
        [70, 230, 158],
    );
    fill_dynamic_rect(&mut pixels, width, height, 0, 0, 1, height, [70, 230, 158]);
    fill_dynamic_rect(
        &mut pixels,
        width,
        height,
        width as i16 - 1,
        0,
        1,
        height,
        [70, 230, 158],
    );

    match orientation {
        LyricOrientation::Horizontal => {
            draw_horizontal_lyric(&mut pixels, width, height, text, elapsed)
        }
        LyricOrientation::Vertical => {
            draw_vertical_lyric(&mut pixels, width, height, text, elapsed)
        }
    }
    (pixels, width, height)
}

fn draw_horizontal_lyric(
    pixels: &mut [u8],
    width: u16,
    height: u16,
    text: &RasterizedText,
    elapsed: Duration,
) {
    let viewport = usize::from(width).saturating_sub(BUBBLE_PADDING * 2);
    let overflow = text.width > viewport;
    let cycle = text.width + SCROLL_GAP as usize;
    let scroll = if overflow {
        (elapsed.as_millis() as usize / 35) % cycle
    } else {
        0
    };
    let left = if overflow {
        BUBBLE_PADDING
    } else {
        (usize::from(width).saturating_sub(text.width)) / 2
    };
    let top = (usize::from(height).saturating_sub(text.height)) / 2;
    for destination_x in 0..viewport {
        let source_x = if overflow {
            (scroll + destination_x) % cycle
        } else {
            destination_x
        };
        if source_x >= text.width {
            continue;
        }
        for source_y in 0..text.height {
            blend_dynamic_pixel(
                pixels,
                width,
                left + destination_x,
                top + source_y,
                text.alpha[source_y * text.width + source_x],
            );
        }
    }
}

fn draw_vertical_lyric(
    pixels: &mut [u8],
    width: u16,
    height: u16,
    text: &RasterizedText,
    elapsed: Duration,
) {
    let viewport = usize::from(height).saturating_sub(BUBBLE_PADDING * 2);
    let overflow = text.width > viewport;
    let cycle = text.width + SCROLL_GAP as usize;
    let scroll = if overflow {
        (elapsed.as_millis() as usize / 35) % cycle
    } else {
        0
    };
    let top = if overflow {
        BUBBLE_PADDING
    } else {
        (usize::from(height).saturating_sub(text.width)) / 2
    };
    let left = (usize::from(width).saturating_sub(text.height)) / 2;
    for destination_y in 0..viewport {
        let source_axis = if overflow {
            (scroll + destination_y) % cycle
        } else {
            destination_y
        };
        if source_axis >= text.width {
            continue;
        }
        let source_x = text.width - 1 - source_axis;
        for source_y in 0..text.height {
            blend_dynamic_pixel(
                pixels,
                width,
                left + source_y,
                top + destination_y,
                text.alpha[source_y * text.width + source_x],
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_dynamic_rect(
    pixels: &mut [u8],
    canvas_width: u16,
    canvas_height: u16,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    color: [u8; 3],
) {
    let left = x.max(0) as usize;
    let top = y.max(0) as usize;
    let right = (i32::from(x) + i32::from(width)).clamp(0, i32::from(canvas_width)) as usize;
    let bottom = (i32::from(y) + i32::from(height)).clamp(0, i32::from(canvas_height)) as usize;
    for row in top..bottom {
        for column in left..right {
            let offset = (row * usize::from(canvas_width) + column) * 4;
            pixels[offset..offset + 3].copy_from_slice(&color);
        }
    }
}

fn blend_dynamic_pixel(pixels: &mut [u8], width: u16, x: usize, y: usize, alpha: u8) {
    if x >= usize::from(width) || y * usize::from(width) * 4 >= pixels.len() {
        return;
    }
    let alpha = u16::from(alpha);
    let offset = (y * usize::from(width) + x) * 4;
    for channel in 0..3 {
        let background = u16::from(pixels[offset + channel]);
        pixels[offset + channel] = ((245 * alpha + background * (255 - alpha)) / 255) as u8;
    }
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
    playback: &PlaybackSnapshot,
    elapsed: Duration,
    hovered: Option<MenuTarget>,
) -> Result<()> {
    if let Some(content) = content {
        let rgba = render_menu(content, playback, elapsed, hovered);
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
        .line
        .chars()
        .map(
            |character| {
                if character.is_ascii() { character } else { ' ' }
            },
        )
        .collect();
    let fallback = fallback.trim().as_bytes();
    connection.image_text8(window, gc, 12, 25, &fallback[..fallback.len().min(54)])?;
    connection.image_text8(window, gc, 116, 79, b"|<     ||     >|")?;
    if hovered == Some(MenuTarget::Quit) {
        connection.poly_fill_rectangle(
            window,
            gc,
            &[Rectangle {
                x: 0,
                y: QUIT_TOP as i16,
                width: MENU_WIDTH,
                height: MENU_HEIGHT - QUIT_TOP,
            }],
        )?;
        connection.change_gc(gc, &ChangeGCAux::new().foreground(black).background(white))?;
    }
    connection.image_text8(window, gc, 168, 120, b"Quit")?;
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

fn render_menu(
    content: &MenuContent,
    playback: &PlaybackSnapshot,
    elapsed: Duration,
    hovered: Option<MenuTarget>,
) -> Vec<u8> {
    let mut pixels = vec![0; usize::from(MENU_WIDTH) * usize::from(MENU_HEIGHT) * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }

    draw_outline(
        &mut pixels,
        PANEL_X,
        PANEL_Y,
        PANEL_WIDTH,
        PANEL_HEIGHT,
        [70, 70, 70],
    );

    let title_left = i32::from(PANEL_X) + MENU_PADDING;
    let viewport_width = i32::from(PANEL_WIDTH) - MENU_PADDING * 2;
    let text_width = content.playback.width as i32;
    let scroll = if text_width > viewport_width {
        (elapsed.as_millis() / 35) as i32 % (text_width + SCROLL_GAP)
    } else {
        0
    };
    blit_scrolling_text(
        &mut pixels,
        &content.playback,
        scroll,
        title_left,
        viewport_width,
        TITLE_TOP as i32 + 3,
        TITLE_TOP,
        TITLE_BOTTOM,
        [210, 210, 210],
    );

    draw_progress(&mut pixels, playback.position, playback.duration);
    draw_control(
        &mut pixels,
        MenuTarget::Previous,
        hovered == Some(MenuTarget::Previous),
        false,
    );
    draw_control(
        &mut pixels,
        MenuTarget::Toggle,
        hovered == Some(MenuTarget::Toggle),
        playback.status == "playing",
    );
    draw_control(
        &mut pixels,
        MenuTarget::Next,
        hovered == Some(MenuTarget::Next),
        false,
    );

    let separator = usize::from(SEPARATOR_Y) * usize::from(MENU_WIDTH) * 4;
    for x in 0..usize::from(MENU_WIDTH) {
        let offset = separator + x * 4;
        pixels[offset..offset + 3].copy_from_slice(&[70, 70, 70]);
    }
    if hovered == Some(MenuTarget::Quit) {
        fill_rect(
            &mut pixels,
            0,
            QUIT_TOP as i16,
            MENU_WIDTH,
            MENU_HEIGHT - QUIT_TOP,
            [29, 96, 72],
        );
    }
    let quit_x = ((usize::from(MENU_WIDTH).saturating_sub(content.quit.width)) / 2) as i32;
    blit_text(
        &mut pixels,
        &content.quit,
        quit_x,
        i32::from(QUIT_TOP) + 5,
        0,
        MENU_WIDTH,
        QUIT_TOP,
        MENU_HEIGHT,
        [255, 255, 255],
    );
    pixels
}

fn draw_progress(pixels: &mut [u8], position: u64, duration: u64) {
    fill_rect(
        pixels,
        PROGRESS_X,
        PROGRESS_Y,
        PROGRESS_WIDTH,
        PROGRESS_HEIGHT,
        [70, 70, 70],
    );
    if duration == 0 {
        return;
    }
    let filled = progress_offset(position, duration, PROGRESS_WIDTH);
    fill_rect(
        pixels,
        PROGRESS_X,
        PROGRESS_Y,
        filled.max(1),
        PROGRESS_HEIGHT,
        [70, 230, 158],
    );
    let knob_x = PROGRESS_X + filled.min(PROGRESS_WIDTH).saturating_sub(2) as i16;
    fill_rect(
        pixels,
        knob_x,
        PROGRESS_Y - 2,
        5,
        PROGRESS_HEIGHT + 4,
        [235, 255, 246],
    );
}

fn draw_control(pixels: &mut [u8], target: MenuTarget, highlighted: bool, playing: bool) {
    let x = match target {
        MenuTarget::Previous => PREVIOUS_X,
        MenuTarget::Toggle => TOGGLE_X,
        MenuTarget::Next => NEXT_X,
        MenuTarget::Quit => return,
    };
    if highlighted {
        fill_rect(
            pixels,
            x,
            CONTROL_TOP,
            CONTROL_WIDTH,
            CONTROL_HEIGHT,
            [29, 96, 72],
        );
    }
    let center_x = x + CONTROL_WIDTH as i16 / 2;
    let center_y = CONTROL_TOP + CONTROL_HEIGHT as i16 / 2;
    let color = [245, 245, 245];
    match target {
        MenuTarget::Previous => {
            fill_rect(pixels, center_x - 9, center_y - 7, 2, 15, color);
            draw_triangle(pixels, center_x + 1, center_y, false, color);
        }
        MenuTarget::Toggle if playing => {
            fill_rect(pixels, center_x - 6, center_y - 7, 4, 15, color);
            fill_rect(pixels, center_x + 2, center_y - 7, 4, 15, color);
        }
        MenuTarget::Toggle => draw_triangle(pixels, center_x, center_y, true, color),
        MenuTarget::Next => {
            draw_triangle(pixels, center_x - 1, center_y, true, color);
            fill_rect(pixels, center_x + 7, center_y - 7, 2, 15, color);
        }
        MenuTarget::Quit => {}
    }
}

fn draw_triangle(pixels: &mut [u8], center_x: i16, center_y: i16, right: bool, color: [u8; 3]) {
    for step in 0..8_i16 {
        let half_height = 7 - step;
        let x = if right {
            center_x - 4 + step
        } else {
            center_x + 4 - step
        };
        fill_rect(
            pixels,
            x,
            center_y - half_height,
            1,
            (half_height * 2 + 1) as u16,
            color,
        );
    }
}

fn draw_outline(pixels: &mut [u8], x: i16, y: i16, width: u16, height: u16, color: [u8; 3]) {
    fill_rect(pixels, x, y, width, 1, color);
    fill_rect(pixels, x, y + height as i16 - 1, width, 1, color);
    fill_rect(pixels, x, y, 1, height, color);
    fill_rect(pixels, x + width as i16 - 1, y, 1, height, color);
}

fn fill_rect(pixels: &mut [u8], x: i16, y: i16, width: u16, height: u16, color: [u8; 3]) {
    let left = x.max(0) as usize;
    let top = y.max(0) as usize;
    let right = (i32::from(x) + i32::from(width)).clamp(0, i32::from(MENU_WIDTH)) as usize;
    let bottom = (i32::from(y) + i32::from(height)).clamp(0, i32::from(MENU_HEIGHT)) as usize;
    for row in top..bottom {
        for column in left..right {
            let offset = (row * usize::from(MENU_WIDTH) + column) * 4;
            pixels[offset..offset + 3].copy_from_slice(&color);
        }
    }
}

fn menu_target(x: i16, y: i16) -> Option<MenuTarget> {
    if y >= CONTROL_TOP && y < CONTROL_TOP + CONTROL_HEIGHT as i16 {
        if x >= PREVIOUS_X && x < PREVIOUS_X + CONTROL_WIDTH as i16 {
            return Some(MenuTarget::Previous);
        }
        if x >= TOGGLE_X && x < TOGGLE_X + CONTROL_WIDTH as i16 {
            return Some(MenuTarget::Toggle);
        }
        if x >= NEXT_X && x < NEXT_X + CONTROL_WIDTH as i16 {
            return Some(MenuTarget::Next);
        }
    }
    if x >= 0 && x < MENU_WIDTH as i16 && y >= QUIT_TOP as i16 && y < MENU_HEIGHT as i16 {
        return Some(MenuTarget::Quit);
    }
    None
}

fn seek_position(x: i16, y: i16, duration: u64) -> Option<u64> {
    if duration == 0
        || y < PROGRESS_Y - 5
        || y >= PROGRESS_Y + PROGRESS_HEIGHT as i16 + 5
        || x < PROGRESS_X
        || x >= PROGRESS_X + PROGRESS_WIDTH as i16
    {
        return None;
    }
    let offset = u16::try_from(x - PROGRESS_X).ok()?;
    Some(seek_from_progress(offset, PROGRESS_WIDTH, duration))
}

fn control_from_tray(command: PlaybackCommand) {
    if let Err(error) = control_cmus(command) {
        tracing::warn!(%error, "tray playback control failed");
    }
}

fn escape_keycodes(connection: &RustConnection) -> Result<Vec<u8>> {
    let setup = connection.setup();
    let count = setup.max_keycode - setup.min_keycode + 1;
    let mapping = connection
        .get_keyboard_mapping(setup.min_keycode, count)?
        .reply()?;
    let keysyms_per_keycode = usize::from(mapping.keysyms_per_keycode);
    if keysyms_per_keycode == 0 {
        return Ok(Vec::new());
    }
    Ok(mapping
        .keysyms
        .chunks(keysyms_per_keycode)
        .enumerate()
        .filter(|(_, keysyms)| keysyms.contains(&XK_ESCAPE))
        .map(|(index, _)| setup.min_keycode + index as u8)
        .collect())
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

#[allow(clippy::too_many_arguments)]
fn blit_scrolling_text(
    pixels: &mut [u8],
    text: &RasterizedText,
    scroll: i32,
    viewport_left: i32,
    viewport_width: i32,
    offset_y: i32,
    clip_top: u16,
    clip_bottom: u16,
    color: [u8; 3],
) {
    let cycle_width = text.width as i32 + SCROLL_GAP;
    let centered_x = viewport_left + (viewport_width - text.width as i32).max(0) / 2;
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
                if text.width as i32 > viewport_width {
                    viewport_left + destination_x
                } else {
                    centered_x + destination_x
                },
                offset_y,
                viewport_left,
                viewport_left + viewport_width,
                clip_top,
                clip_bottom,
                color,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_text(
    pixels: &mut [u8],
    text: &RasterizedText,
    offset_x: i32,
    offset_y: i32,
    clip_left: u16,
    clip_right: u16,
    clip_top: u16,
    clip_bottom: u16,
    color: [u8; 3],
) {
    for source_x in 0..text.width {
        blit_column(
            pixels,
            text,
            source_x,
            offset_x + source_x as i32,
            offset_y,
            i32::from(clip_left),
            i32::from(clip_right),
            clip_top,
            clip_bottom,
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
    clip_left: i32,
    clip_right: i32,
    clip_top: u16,
    clip_bottom: u16,
    color: [u8; 3],
) {
    if destination_x < clip_left || destination_x >= clip_right {
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
            let background = u16::from(pixels[offset + channel]);
            pixels[offset + channel] =
                ((u16::from(color[channel]) * alpha + background * (255 - alpha)) / 255) as u8;
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
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            CURRENT_TIME,
        )?
        .reply()?;
    connection
        .grab_keyboard(false, menu, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)?
        .reply()?;
    connection.flush()?;
    Ok(())
}

fn close_menu(connection: &RustConnection, menu: Window) -> Result<()> {
    connection.ungrab_pointer(CURRENT_TIME)?;
    connection.ungrab_keyboard(CURRENT_TIME)?;
    connection.unmap_window(menu)?;
    connection.flush()?;
    Ok(())
}

fn hide_bubble(connection: &RustConnection, bubble: Window) -> Result<()> {
    connection.unmap_window(bubble)?;
    connection.flush()?;
    Ok(())
}
