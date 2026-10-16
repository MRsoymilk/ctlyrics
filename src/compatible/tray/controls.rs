use std::thread;
use std::time::{Duration, Instant};

use ab_glyph::FontVec;
use anyhow::Result;
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::reexports::client::{
    QueueHandle,
    protocol::{wl_output, wl_shm, wl_surface},
};
use smithay_client_toolkit::seat::pointer::{BTN_LEFT, PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::{
    WaylandSurface,
    wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell},
    xdg::{XdgShell, window::WindowDecorations},
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};

use super::{
    ExitSignal, PlaybackInfo, PlaybackSnapshot,
    bubble::{RasterizedText, font_supports, load_font, rasterize_text},
    wayland::{ShellSurface, State},
};
use crate::cmus::{PlaybackCommand, control_cmus, seek_cmus};
use crate::i18n::{Locale, tr};
use crate::web::start_and_open;

const WIDTH: u16 = 360;
const HEIGHT: u16 = 162;
const WEB_Y: u16 = 98;
const QUIT_Y: u16 = 130;
const FOOTER_ROW_HEIGHT: u16 = 32;
const PADDING: u16 = 20;
const TIME_GAP: u16 = 8;
const PROGRESS_Y: u16 = 43;
const PROGRESS_HEIGHT: u16 = 5;
const BUTTON_Y: u16 = 54;
const BUTTON_WIDTH: u16 = 44;
const BUTTON_HEIGHT: u16 = 31;
const BUTTON_SPACING: u16 = 50;
const SCROLL_GAP: usize = 48;
const FRAME_INTERVAL: Duration = Duration::from_millis(40);

#[derive(Clone, Copy, Debug)]
struct ProgressRow {
    elapsed_x: u16,
    duration_x: u16,
    track_x: u16,
    track_width: u16,
}

impl ProgressRow {
    fn new(width: u16, elapsed_width: u16, duration_width: u16) -> Self {
        let duration_x = width.saturating_sub(PADDING + duration_width);
        let track_x = PADDING + elapsed_width + TIME_GAP;
        Self {
            elapsed_x: PADDING,
            duration_x,
            track_x,
            track_width: duration_x.saturating_sub(TIME_GAP + track_x),
        }
    }

    fn contains(&self, position: (f64, f64)) -> bool {
        position.0 >= f64::from(self.track_x)
            && position.0 <= f64::from(self.track_x + self.track_width)
            && (f64::from(PROGRESS_Y - 8)..f64::from(BUTTON_Y)).contains(&position.1)
    }

    fn seek_position(&self, x: f64, duration: u64) -> u64 {
        if self.track_width == 0 {
            return 0;
        }
        let fraction =
            ((x - f64::from(self.track_x)) / f64::from(self.track_width)).clamp(0.0, 1.0);
        (fraction * duration as f64).round() as u64
    }
}

struct SeekDrag {
    position: u64,
    duration: u64,
    line: String,
}

impl SeekDrag {
    fn valid_for(&self, playback: &PlaybackSnapshot) -> bool {
        self.duration == playback.duration && self.line == playback.line
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Previous,
    Toggle,
    Next,
    Web,
    Quit,
    Close,
}

enum PanelAction {
    Seek(u64),
    Control(PlaybackCommand),
    Close,
    Web,
    Quit,
}

pub(super) struct ControlPanel {
    pub(super) shell: Option<ShellSurface>,
    pub(super) configured: bool,
    pub(super) frame_ready: bool,
    pub(super) dirty: bool,
    playback: PlaybackInfo,
    exit: ExitSignal,
    locale: Locale,
    font: Option<FontVec>,
    time_metrics: Option<(u64, u16)>,
    title: Option<RasterizedText>,
    title_line: String,
    title_started: Instant,
    buffer: Option<Buffer>,
    buffer_size: (u16, u16),
    width: u16,
    row: ProgressRow,
    drag: Option<SeekDrag>,
    preview: Option<(SeekDrag, Instant)>,
    pressed: Option<Button>,
    hovered: Option<Button>,
    last_draw: Instant,
    viewport: (u16, u16),
    origin: (u16, u16),
    anchor: Option<(i32, i32)>,
    painted_rect: Option<((u16, u16), u16)>,
}

impl ControlPanel {
    pub(super) fn new(locale: Locale, playback: PlaybackInfo, exit: ExitSignal) -> Self {
        Self {
            shell: None,
            configured: false,
            frame_ready: true,
            dirty: true,
            playback,
            exit,
            locale,
            font: None,
            time_metrics: None,
            title: None,
            title_line: String::new(),
            title_started: Instant::now(),
            buffer: None,
            buffer_size: (0, 0),
            width: WIDTH,
            row: ProgressRow::new(WIDTH, 40, 40),
            drag: None,
            preview: None,
            pressed: None,
            hovered: None,
            last_draw: Instant::now(),
            viewport: (WIDTH, HEIGHT),
            origin: (0, 0),
            anchor: None,
            painted_rect: None,
        }
    }

    pub(super) fn owns(&self, surface: &wl_surface::WlSurface) -> bool {
        self.shell
            .as_ref()
            .is_some_and(|shell| shell.wl_surface() == surface)
    }

    pub(super) fn show(
        &mut self,
        compositor: &CompositorState,
        layer_shell: Option<&LayerShell>,
        xdg_shell: Option<&XdgShell>,
        anchor: Option<(i32, i32)>,
        output: Option<&wl_output::WlOutput>,
        qh: &QueueHandle<State>,
    ) {
        // A second right-click dismisses the same menu, not the lyric surface.
        if self.shell.is_some() {
            self.close();
            return;
        }
        self.anchor = anchor;
        self.title_started = Instant::now();
        self.origin = (0, 0);
        self.viewport = (self.width, HEIGHT);
        let surface = compositor.create_surface(qh);
        if let Some(layer_shell) = layer_shell {
            let layer = layer_shell.create_layer_surface(
                qh,
                surface,
                Layer::Overlay,
                Some("ctlyrics-controls"),
                output,
            );
            // A transparent full-output surface catches outside clicks like a popup
            // grab. Only the menu rectangle is painted; the lyric stays visible.
            layer.set_anchor(Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM | Anchor::LEFT);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.set_size(0, 0);
            layer.commit();
            self.shell = Some(ShellSurface::Layer(layer));
        } else if let Some(xdg_shell) = xdg_shell {
            let window = xdg_shell.create_window(surface, WindowDecorations::ServerDefault, qh);
            window.set_title("ctlyrics");
            window.set_app_id("ctlyrics-controls");
            window.set_min_size(Some((u32::from(self.width), u32::from(HEIGHT))));
            window.set_max_size(Some((u32::from(self.width), u32::from(HEIGHT))));
            window.commit();
            self.shell = Some(ShellSurface::Xdg(window));
        }
        self.configured = false;
        self.frame_ready = true;
        self.dirty = true;
    }

    pub(super) fn configure_layer(&mut self, size: (u32, u32)) {
        self.viewport = (
            size.0.clamp(u32::from(self.width), u32::from(u16::MAX)) as u16,
            size.1.clamp(u32::from(HEIGHT), u32::from(u16::MAX)) as u16,
        );
        self.origin = menu_origin(self.anchor, self.viewport, (self.width, HEIGHT));
        self.configured = true;
        self.dirty = true;
    }

    pub(super) fn cancel_drag(&mut self) {
        self.drag = None;
        self.pressed = None;
        self.dirty = true;
    }

    pub(super) fn close(&mut self) {
        self.cancel_drag();
        self.preview = None;
        self.hovered = None;
        self.shell = None;
        self.buffer = None;
        self.buffer_size = (0, 0);
        self.painted_rect = None;
        self.configured = false;
    }

    pub(super) fn pointer_event(&mut self, event: &PointerEvent) {
        let position = (
            event.position.0 - f64::from(self.origin.0),
            event.position.1 - f64::from(self.origin.1),
        );
        match self.handle_pointer(event.kind.clone(), position) {
            Some(PanelAction::Seek(position)) => {
                // Never block Wayland dispatch on a remote seek; submit once on release.
                thread::spawn(move || {
                    if let Err(error) = seek_cmus(position) {
                        tracing::warn!(%error, "control panel seek failed");
                    }
                });
            }
            Some(PanelAction::Control(command)) => run_control(command),
            Some(PanelAction::Close) => self.close(),
            Some(PanelAction::Web) => {
                self.close();
                thread::spawn(|| {
                    if let Err(error) = start_and_open() {
                        tracing::warn!(%error, "tray Web launch failed");
                    }
                });
            }
            Some(PanelAction::Quit) => {
                self.close();
                self.exit.request();
            }
            None => {}
        }
    }

    fn handle_pointer(
        &mut self,
        kind: PointerEventKind,
        position: (f64, f64),
    ) -> Option<PanelAction> {
        if matches!(kind, PointerEventKind::Press { .. })
            && (position.0 < 0.0
                || position.0 >= f64::from(self.width)
                || position.1 < 0.0
                || position.1 >= f64::from(HEIGHT))
        {
            self.cancel_drag();
            return Some(PanelAction::Close);
        }
        let playback = self.playback.snapshot();
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| !drag.valid_for(&playback))
        {
            self.cancel_drag();
        }
        let mut action = None;
        match kind {
            PointerEventKind::Press {
                button: BTN_LEFT, ..
            } => {
                self.pressed = button_at(position, self.width);
                if playback.duration > 0 && self.row.contains(position) {
                    self.preview = None;
                    self.drag = Some(SeekDrag {
                        position: self.row.seek_position(position.0, playback.duration),
                        duration: playback.duration,
                        line: playback.line,
                    });
                }
            }
            PointerEventKind::Motion { .. } => {
                self.hovered = button_at(position, self.width);
                if let Some(drag) = self.drag.as_mut() {
                    drag.position = self.row.seek_position(position.0, drag.duration);
                }
            }
            PointerEventKind::Release {
                button: BTN_LEFT, ..
            } => {
                if let Some(mut drag) = self.drag.take() {
                    drag.position = self.row.seek_position(position.0, drag.duration);
                    let position = drag.position;
                    self.preview = Some((drag, Instant::now()));
                    action = Some(PanelAction::Seek(position));
                    self.pressed = None;
                } else if let Some(button) = self.pressed.take()
                    && button_at(position, self.width) == Some(button)
                {
                    action = Some(match button {
                        Button::Close => PanelAction::Close,
                        Button::Previous => PanelAction::Control(PlaybackCommand::Previous),
                        Button::Toggle => PanelAction::Control(PlaybackCommand::TogglePause),
                        Button::Next => PanelAction::Control(PlaybackCommand::Next),
                        Button::Web => PanelAction::Web,
                        Button::Quit => PanelAction::Quit,
                    });
                }
            }
            PointerEventKind::Leave { .. } => self.hovered = None,
            _ => return None,
        }
        self.dirty = true;
        action
    }

    pub(super) fn draw(&mut self, pool: &mut SlotPool, qh: &QueueHandle<State>) -> Result<()> {
        if self.shell.is_none()
            || !self.configured
            || !self.frame_ready
            || (!self.dirty && self.last_draw.elapsed() < FRAME_INTERVAL)
        {
            return Ok(());
        }
        let mut playback = self.playback.snapshot();
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| !drag.valid_for(&playback))
        {
            self.cancel_drag();
        }
        if self.preview.as_ref().is_some_and(|(seek, since)| {
            !seek.valid_for(&playback)
                || playback.position.abs_diff(seek.position) <= 1
                || since.elapsed() >= Duration::from_secs(1)
        }) {
            self.preview = None;
        }
        if let Some(drag) = &self.drag {
            playback.position = drag.position;
        } else if let Some((seek, _)) = &self.preview {
            playback.position = seek.position;
        }
        let sample = format!(
            "{} 0123456789: {} {}",
            playback.line,
            tr(self.locale, "tray_web"),
            tr(self.locale, "tray_quit")
        );
        if self
            .font
            .as_ref()
            .is_none_or(|font| !font_supports(font, &sample))
        {
            self.font = load_font(&sample);
            self.time_metrics = None;
            self.title = None;
        }
        let Some(font) = &self.font else {
            return Ok(());
        };
        if self.title.is_none() || self.title_line != playback.line {
            self.title = Some(rasterize_text(font, &playback.line));
            if self.title_line != playback.line {
                self.title_started = Instant::now();
                self.title_line = playback.line.clone();
            }
        }
        let elapsed = rasterize_text(font, &format_time(playback.position.min(playback.duration)));
        let duration = rasterize_text(font, &format_time(playback.duration));
        // Reserve the widest digits, including hour-long times, so a proportional font
        // cannot make the elapsed label overlap the track or shift it while dragging.
        let elapsed_width = match self.time_metrics {
            Some((seconds, width)) if seconds == playback.duration => width,
            _ => {
                let template = format_time(playback.duration);
                let width = ('0'..='9')
                    .map(|digit| {
                        let text: String = template
                            .chars()
                            .map(|c| if c.is_ascii_digit() { digit } else { c })
                            .collect();
                        rasterize_text(font, &text).width as u16
                    })
                    .max()
                    .unwrap_or(0);
                self.time_metrics = Some((playback.duration, width));
                width
            }
        };
        self.width =
            WIDTH.max(elapsed_width + duration.width as u16 + 2 * PADDING + 2 * TIME_GAP + 80);
        if matches!(self.shell, Some(ShellSurface::Layer(_))) {
            self.origin = menu_origin(self.anchor, self.viewport, (self.width, HEIGHT));
        } else {
            self.viewport = (self.width, HEIGHT);
        }
        self.row = ProgressRow::new(self.width, elapsed_width, duration.width as u16);
        let pixels = render_panel(
            font,
            self.locale,
            &playback,
            &elapsed,
            &duration,
            self.width,
            self.row,
            self.hovered,
            self.title.as_ref().expect("rasterized menu title"),
            self.title_started.elapsed(),
        );
        let shell = self.shell.as_ref().expect("visible control panel");
        let surface = shell.wl_surface().clone();
        if self.buffer_size != self.viewport {
            if matches!(shell, ShellSurface::Xdg(_)) {
                shell.set_size(u32::from(self.width), u32::from(HEIGHT));
            }
            self.buffer = None;
            self.buffer_size = self.viewport;
            self.painted_rect = None;
        }
        if self.buffer.is_none() {
            self.buffer = Some(
                pool.create_buffer(
                    i32::from(self.viewport.0),
                    i32::from(self.viewport.1),
                    i32::from(self.viewport.0) * 4,
                    wl_shm::Format::Argb8888,
                )?
                .0,
            );
        }
        let buffer = self
            .buffer
            .as_mut()
            .expect("allocated control panel buffer");
        let Some(canvas) = pool.canvas(buffer) else {
            self.dirty = true;
            return Ok(());
        };
        let clear_backdrop = self.painted_rect != Some((self.origin, self.width));
        paint_menu(
            canvas,
            &pixels,
            self.viewport,
            self.origin,
            self.width,
            clear_backdrop,
        );
        if clear_backdrop {
            surface.damage_buffer(0, 0, i32::from(self.viewport.0), i32::from(self.viewport.1));
        } else {
            surface.damage_buffer(
                i32::from(self.origin.0),
                i32::from(self.origin.1),
                i32::from(self.width),
                i32::from(HEIGHT),
            );
        }
        self.painted_rect = Some((self.origin, self.width));
        surface.frame(qh, surface.clone());
        buffer.attach_to(&surface)?;
        surface.commit();
        self.frame_ready = false;
        self.dirty = false;
        self.last_draw = Instant::now();
        Ok(())
    }
}

fn paint_menu(
    canvas: &mut [u8],
    pixels: &[u8],
    viewport: (u16, u16),
    origin: (u16, u16),
    menu_width: u16,
    clear: bool,
) {
    if clear {
        canvas.fill(0);
    }
    let width = usize::from(menu_width.min(viewport.0.saturating_sub(origin.0)));
    for y in 0..usize::from(HEIGHT.min(viewport.1.saturating_sub(origin.1))) {
        let start =
            ((usize::from(origin.1) + y) * usize::from(viewport.0) + usize::from(origin.0)) * 4;
        let row = &pixels[y * usize::from(menu_width) * 4..][..width * 4];
        for (source, destination) in row
            .chunks_exact(4)
            .zip(canvas[start..start + width * 4].chunks_exact_mut(4))
        {
            destination.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
        }
    }
}

fn button_at(position: (f64, f64), width: u16) -> Option<Button> {
    if (f64::from(width - 32)..f64::from(width - 9)).contains(&position.0)
        && (9.0..30.0).contains(&position.1)
    {
        return Some(Button::Close);
    }
    if (1.0..f64::from(width - 1)).contains(&position.0) {
        if (f64::from(WEB_Y)..f64::from(QUIT_Y)).contains(&position.1) {
            return Some(Button::Web);
        }
        if (f64::from(QUIT_Y)..f64::from(HEIGHT - 1)).contains(&position.1) {
            return Some(Button::Quit);
        }
    }
    if !(f64::from(BUTTON_Y)..f64::from(BUTTON_Y + BUTTON_HEIGHT)).contains(&position.1) {
        return None;
    }
    button_positions(width).into_iter().find_map(|(button, x)| {
        (f64::from(x)..f64::from(x + BUTTON_WIDTH))
            .contains(&position.0)
            .then_some(button)
    })
}

fn button_positions(width: u16) -> [(Button, u16); 3] {
    [
        (
            Button::Previous,
            (width - BUTTON_WIDTH) / 2 - BUTTON_SPACING,
        ),
        (Button::Toggle, (width - BUTTON_WIDTH) / 2),
        (Button::Next, (width - BUTTON_WIDTH) / 2 + BUTTON_SPACING),
    ]
}

#[allow(clippy::too_many_arguments)]
fn render_panel(
    font: &FontVec,
    locale: Locale,
    playback: &PlaybackSnapshot,
    elapsed: &RasterizedText,
    duration: &RasterizedText,
    width: u16,
    row: ProgressRow,
    hovered: Option<Button>,
    title: &RasterizedText,
    title_elapsed: Duration,
) -> Vec<u8> {
    let mut pixels = vec![0; usize::from(width) * usize::from(HEIGHT) * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[0, 0, 0, 255]);
    }
    outline(&mut pixels, width, (0, 0, width, HEIGHT), [70, 70, 70]);
    outline(&mut pixels, width, (8, 7, width - 16, 84), [70, 70, 70]);
    blit_scrolling_title(&mut pixels, width, title, title_elapsed);
    // A discreet close glyph rather than a text button in the title row.
    for step in 0..7 {
        fill(
            &mut pixels,
            width,
            (width - 24 + step, 16 + step, 1, 1),
            [150, 150, 150],
        );
        fill(
            &mut pixels,
            width,
            (width - 18 - step, 16 + step, 1, 1),
            [150, 150, 150],
        );
    }
    blit(
        &mut pixels,
        width,
        elapsed,
        row.elapsed_x,
        PROGRESS_Y - 6,
        row.track_x - TIME_GAP,
    );
    blit(
        &mut pixels,
        width,
        duration,
        row.duration_x,
        PROGRESS_Y - 6,
        width - PADDING,
    );
    fill(
        &mut pixels,
        width,
        (row.track_x, PROGRESS_Y, row.track_width, PROGRESS_HEIGHT),
        [70, 70, 70],
    );
    let fraction = if playback.duration == 0 {
        0.0
    } else {
        playback.position as f64 / playback.duration as f64
    };
    let filled = (fraction.clamp(0.0, 1.0) * f64::from(row.track_width)).round() as u16;
    fill(
        &mut pixels,
        width,
        (row.track_x, PROGRESS_Y, filled, PROGRESS_HEIGHT),
        [70, 230, 158],
    );
    if playback.duration > 0 {
        let knob = row.track_x + filled.min(row.track_width.saturating_sub(5));
        fill(
            &mut pixels,
            width,
            (knob, PROGRESS_Y - 2, 5, PROGRESS_HEIGHT + 4),
            [235, 255, 246],
        );
    }
    for (button, x) in button_positions(width) {
        draw_transport(
            &mut pixels,
            width,
            button,
            x,
            hovered == Some(button),
            playback.status == "playing",
        );
    }
    fill(
        &mut pixels,
        width,
        (1, WEB_Y - 1, width - 2, 1),
        [70, 70, 70],
    );
    for (button, y, label) in [
        (Button::Web, WEB_Y, "tray_web"),
        (Button::Quit, QUIT_Y, "tray_quit"),
    ] {
        if hovered == Some(button) {
            fill(
                &mut pixels,
                width,
                (1, y, width - 2, FOOTER_ROW_HEIGHT.min(HEIGHT - 1 - y)),
                [29, 96, 72],
            );
        }
        let text = rasterize_text(font, tr(locale, label));
        blit(
            &mut pixels,
            width,
            &text,
            width.saturating_sub(text.width as u16) / 2,
            y + 5,
            width - 1,
        );
    }
    pixels
}

fn menu_origin(anchor: Option<(i32, i32)>, viewport: (u16, u16), size: (u16, u16)) -> (u16, u16) {
    let max_x = viewport.0.saturating_sub(size.0);
    let max_y = viewport.1.saturating_sub(size.1);
    match anchor {
        Some((x, y)) if x != 0 || y != 0 => {
            let below = y.saturating_add(4);
            let y = if below > i32::from(max_y) {
                y.saturating_sub(i32::from(size.1) + 4)
            } else {
                below
            };
            (
                x.clamp(0, i32::from(max_x)) as u16,
                y.clamp(0, i32::from(max_y)) as u16,
            )
        }
        _ => (max_x.saturating_sub(16), max_y.min(16)),
    }
}

fn outline(pixels: &mut [u8], width: u16, rect: (u16, u16, u16, u16), color: [u8; 3]) {
    let (x, y, w, h) = rect;
    fill(pixels, width, (x, y, w, 1), color);
    fill(pixels, width, (x, y + h - 1, w, 1), color);
    fill(pixels, width, (x, y, 1, h), color);
    fill(pixels, width, (x + w - 1, y, 1, h), color);
}

fn draw_transport(
    pixels: &mut [u8],
    width: u16,
    button: Button,
    x: u16,
    hovered: bool,
    playing: bool,
) {
    if hovered {
        fill(
            pixels,
            width,
            (x, BUTTON_Y, BUTTON_WIDTH, BUTTON_HEIGHT),
            [29, 96, 72],
        );
    }
    let cx = x + BUTTON_WIDTH / 2;
    let cy = BUTTON_Y + BUTTON_HEIGHT / 2;
    let color = [245, 245, 245];
    match button {
        Button::Previous => {
            fill(pixels, width, (cx - 9, cy - 7, 2, 15), color);
            triangle(pixels, width, cx + 1, cy, false, color);
        }
        Button::Toggle if playing => {
            fill(pixels, width, (cx - 6, cy - 7, 4, 15), color);
            fill(pixels, width, (cx + 2, cy - 7, 4, 15), color);
        }
        Button::Toggle => triangle(pixels, width, cx, cy, true, color),
        Button::Next => {
            triangle(pixels, width, cx - 1, cy, true, color);
            fill(pixels, width, (cx + 7, cy - 7, 2, 15), color);
        }
        _ => {}
    }
}

fn triangle(pixels: &mut [u8], width: u16, cx: u16, cy: u16, right: bool, color: [u8; 3]) {
    for step in 0..8 {
        let half_height = 7 - step;
        let x = if right { cx - 4 + step } else { cx + 4 - step };
        fill(
            pixels,
            width,
            (x, cy - half_height, 1, half_height * 2 + 1),
            color,
        );
    }
}

fn blit_scrolling_title(pixels: &mut [u8], width: u16, text: &RasterizedText, elapsed: Duration) {
    let visible_width = usize::from(width - PADDING * 2 - 24);
    let offset = if text.width > visible_width {
        (elapsed.as_millis() / 35) as usize % (text.width + SCROLL_GAP)
    } else {
        0
    };
    for y in 0..text.height.min(20) {
        for x in 0..visible_width.min(text.width + SCROLL_GAP) {
            let source_x = (offset + x) % (text.width + SCROLL_GAP);
            if source_x >= text.width {
                continue;
            }
            let alpha = u16::from(text.alpha[y * text.width + source_x]);
            let target = ((12 + y) * usize::from(width) + usize::from(PADDING) + x) * 4;
            for channel in &mut pixels[target..target + 3] {
                *channel = ((u16::from(*channel) * (255 - alpha) + 210 * alpha) / 255) as u8;
            }
        }
    }
}

fn fill(pixels: &mut [u8], width: u16, rect: (u16, u16, u16, u16), color: [u8; 3]) {
    let (x, y, w, h) = rect;
    for row in y..y.saturating_add(h).min(HEIGHT) {
        for column in x..x.saturating_add(w).min(width) {
            let offset = (usize::from(row) * usize::from(width) + usize::from(column)) * 4;
            pixels[offset..offset + 3].copy_from_slice(&color);
        }
    }
}

fn blit(pixels: &mut [u8], width: u16, text: &RasterizedText, x: u16, y: u16, right: u16) {
    for row in 0..text.height.min(usize::from(HEIGHT.saturating_sub(y))) {
        for column in 0..text
            .width
            .min(usize::from(right.min(width).saturating_sub(x)))
        {
            let alpha = u16::from(text.alpha[row * text.width + column]);
            let offset =
                ((usize::from(y) + row) * usize::from(width) + usize::from(x) + column) * 4;
            for channel in &mut pixels[offset..offset + 3] {
                *channel = ((u16::from(*channel) * (255 - alpha) + 230 * alpha) / 255) as u8;
            }
        }
    }
}

fn format_time(seconds: u64) -> String {
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

fn run_control(command: PlaybackCommand) {
    thread::spawn(move || {
        if let Err(error) = control_cmus(command) {
            tracing::warn!(%error, "control panel playback command failed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_controls_use_icons_and_only_highlight_on_hover() {
        let mut pixels = [0_u8, 0, 0, 255].repeat(usize::from(WIDTH) * usize::from(HEIGHT));
        let (_, x) = button_positions(WIDTH)[1];
        let cx = x + BUTTON_WIDTH / 2;
        let cy = BUTTON_Y + BUTTON_HEIGHT / 2;
        let sample = |pixels: &[u8], x: u16, y: u16| -> [u8; 3] {
            let index = (usize::from(y) * usize::from(WIDTH) + usize::from(x)) * 4;
            pixels[index..index + 3].try_into().unwrap()
        };
        draw_transport(&mut pixels, WIDTH, Button::Toggle, x, false, true);
        assert_eq!(sample(&pixels, cx - 4, cy), [245, 245, 245]);
        assert_eq!(sample(&pixels, cx, cy), [0, 0, 0]);
        assert_eq!(sample(&pixels, x + 1, cy), [0, 0, 0]);
        draw_transport(&mut pixels, WIDTH, Button::Toggle, x, true, false);
        assert_eq!(sample(&pixels, cx, cy), [245, 245, 245]);
        assert_eq!(sample(&pixels, x + 1, cy), [29, 96, 72]);
    }

    #[test]
    fn footer_rows_and_slider_hit_targets_remain_separate() {
        assert!(matches!(
            button_at((40.0, f64::from(WEB_Y)), WIDTH),
            Some(Button::Web)
        ));
        assert!(matches!(
            button_at((40.0, f64::from(QUIT_Y)), WIDTH),
            Some(Button::Quit)
        ));
        let row = ProgressRow::new(WIDTH, 40, 40);
        assert!(row.contains((f64::from(row.track_x + 10), f64::from(PROGRESS_Y))));
        assert!(!row.contains((f64::from(row.elapsed_x), f64::from(PROGRESS_Y))));
        assert!(!row.contains((f64::from(row.track_x + 10), f64::from(BUTTON_Y))));
    }

    #[test]
    #[ignore = "requires installed fonts; generates offline style previews in /tmp/opencode"]
    fn render_style_preview() {
        let line = "A song title - Artist · Playing";
        let font = load_font(&format!("{line} 0123456789: Start Web Quit"))
            .expect("installed preview font");
        let elapsed = rasterize_text(&font, "00:42");
        let duration = rasterize_text(&font, "03:18");
        let row = ProgressRow::new(WIDTH, duration.width as u16, duration.width as u16);
        for (name, status, hovered) in [
            ("playing", "playing", None),
            ("paused", "paused", Some(Button::Toggle)),
        ] {
            let line = format!(
                "A song title - Artist · {}",
                if status == "playing" {
                    "Playing"
                } else {
                    "Paused"
                }
            );
            let title = rasterize_text(&font, &line);
            let playback = PlaybackSnapshot {
                line,
                status: status.into(),
                position: 42,
                duration: 198,
                ..Default::default()
            };
            let pixels = render_panel(
                &font,
                Locale::En,
                &playback,
                &elapsed,
                &duration,
                WIDTH,
                row,
                hovered,
                &title,
                Duration::ZERO,
            );
            image::RgbaImage::from_raw(u32::from(WIDTH), u32::from(HEIGHT), pixels)
                .unwrap()
                .save(format!("/tmp/opencode/ctlyrics-tray-{name}.png"))
                .unwrap();
        }
    }

    fn press() -> PointerEventKind {
        PointerEventKind::Press {
            time: 1,
            button: BTN_LEFT,
            serial: 1,
        }
    }

    #[test]
    fn menu_backdrop_is_transparent_and_only_the_menu_is_painted() {
        let viewport = (800, 600);
        let origin = (100, 100);
        let pixels: Vec<_> = [10, 20, 30, 255].repeat(usize::from(WIDTH) * usize::from(HEIGHT));
        let mut canvas = vec![255; 800 * 600 * 4];
        paint_menu(&mut canvas, &pixels, viewport, origin, WIDTH, true);
        assert_eq!(&canvas[..4], &[0, 0, 0, 0]);
        let inside = (100 * 800 + 100) * 4;
        assert_eq!(&canvas[inside..inside + 4], &[30, 20, 10, 255]);
        let outside = (100 * 800 + 99) * 4;
        assert_eq!(&canvas[outside..outside + 4], &[0, 0, 0, 0]);
        paint_menu(&mut canvas, &pixels, viewport, origin, WIDTH, false);
        assert_eq!(&canvas[outside..outside + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn clicking_outside_dismisses_the_menu_without_seeking() {
        let playback = PlaybackInfo::new(Locale::En);
        playback.update("Song".into(), "Song", "Lyric", "playing", 20, 200);
        let mut panel = ControlPanel::new(Locale::En, playback, ExitSignal::new());
        panel.handle_pointer(
            press(),
            (f64::from(panel.row.track_x), f64::from(PROGRESS_Y)),
        );
        assert!(matches!(
            panel.handle_pointer(press(), (-1.0, f64::from(PROGRESS_Y))),
            Some(PanelAction::Close)
        ));
        assert!(panel.drag.is_none());
    }

    #[test]
    fn context_menu_position_is_clamped_and_flips_above_a_bottom_tray() {
        assert_eq!(
            menu_origin(Some((1900, 20)), (1920, 1080), (WIDTH, HEIGHT)),
            (1920 - WIDTH, 24)
        );
        assert_eq!(
            menu_origin(Some((100, 1060)), (1920, 1080), (WIDTH, HEIGHT)),
            (100, 1060 - HEIGHT - 4)
        );
        assert_eq!(
            menu_origin(Some((-100, -100)), (1920, 1080), (WIDTH, HEIGHT)),
            (0, 0)
        );
    }

    #[test]
    fn full_context_menu_has_web_and_quit_targets() {
        let mut panel =
            ControlPanel::new(Locale::En, PlaybackInfo::new(Locale::En), ExitSignal::new());
        let web = (f64::from(PADDING + 10), f64::from(WEB_Y + 10));
        panel.handle_pointer(press(), web);
        assert!(matches!(
            panel.handle_pointer(release(), web),
            Some(PanelAction::Web)
        ));
        let quit = (f64::from(WIDTH - PADDING - 10), f64::from(QUIT_Y + 10));
        panel.handle_pointer(press(), quit);
        assert!(matches!(
            panel.handle_pointer(release(), quit),
            Some(PanelAction::Quit)
        ));
    }

    fn release() -> PointerEventKind {
        PointerEventKind::Release {
            time: 2,
            button: BTN_LEFT,
            serial: 2,
        }
    }

    #[test]
    fn drag_previews_without_seeking_until_release() {
        let playback = PlaybackInfo::new(Locale::En);
        playback.update("Song".into(), "Song", "Lyric", "playing", 20, 200);
        let mut panel = ControlPanel::new(Locale::En, playback, ExitSignal::new());
        let start = f64::from(panel.row.track_x);
        let middle = start + f64::from(panel.row.track_width) / 2.0;
        assert!(
            panel
                .handle_pointer(press(), (start, f64::from(PROGRESS_Y)))
                .is_none()
        );
        assert!(
            panel
                .handle_pointer(
                    PointerEventKind::Motion { time: 2 },
                    (middle, f64::from(PROGRESS_Y))
                )
                .is_none()
        );
        assert_eq!(panel.drag.as_ref().unwrap().position, 100);
        assert!(matches!(
            panel.handle_pointer(release(), (middle, f64::from(PROGRESS_Y))),
            Some(PanelAction::Seek(100))
        ));
        assert!(panel.drag.is_none());
        assert_eq!(panel.preview.as_ref().unwrap().0.position, 100);
        assert!(
            panel
                .handle_pointer(release(), (middle, f64::from(PROGRESS_Y)))
                .is_none()
        );
    }

    #[test]
    fn changing_tracks_during_drag_does_not_seek_the_new_track() {
        let playback = PlaybackInfo::new(Locale::En);
        playback.update("Song".into(), "Song", "Lyric", "playing", 20, 200);
        let mut panel = ControlPanel::new(Locale::En, playback.clone(), ExitSignal::new());
        let position = (f64::from(panel.row.track_x + 30), f64::from(PROGRESS_Y));
        panel.handle_pointer(press(), position);
        playback.update(
            "Other song".into(),
            "Other song",
            "Other lyric",
            "playing",
            0,
            180,
        );
        assert!(panel.handle_pointer(release(), position).is_none());
        assert!(panel.drag.is_none());
    }

    #[test]
    fn closing_cancels_an_unfinished_drag() {
        let playback = PlaybackInfo::new(Locale::En);
        playback.update("Song".into(), "Song", "Lyric", "playing", 20, 200);
        let mut panel = ControlPanel::new(Locale::En, playback, ExitSignal::new());
        let position = (f64::from(panel.row.track_x + 30), f64::from(PROGRESS_Y));
        panel.handle_pointer(press(), position);
        panel.close();
        assert!(panel.handle_pointer(release(), position).is_none());
    }

    #[test]
    fn progress_and_times_fill_the_row() {
        let row = ProgressRow::new(WIDTH, 42, 58);
        assert_eq!(row.elapsed_x, PADDING);
        assert_eq!(row.duration_x + 58, WIDTH - PADDING);
        assert_eq!(row.track_x, PADDING + 42 + TIME_GAP);
        assert_eq!(row.track_x + row.track_width + TIME_GAP, row.duration_x);
    }

    #[test]
    fn drag_positions_clamp_and_reach_both_endpoints() {
        let row = ProgressRow::new(WIDTH, 40, 40);
        assert_eq!(row.seek_position(-100.0, 200), 0);
        assert_eq!(row.seek_position(f64::from(row.track_x), 200), 0);
        assert_eq!(
            row.seek_position(
                f64::from(row.track_x) + f64::from(row.track_width) / 2.0,
                200
            ),
            100
        );
        assert_eq!(
            row.seek_position(f64::from(row.track_x + row.track_width), 200),
            200
        );
        assert_eq!(row.seek_position(1000.0, 200), 200);
    }

    #[test]
    fn unknown_duration_does_not_start_a_seek() {
        let mut panel =
            ControlPanel::new(Locale::En, PlaybackInfo::new(Locale::En), ExitSignal::new());
        assert_eq!(panel.row.seek_position(200.0, 0), 0);
        assert!(
            panel
                .handle_pointer(press(), (200.0, f64::from(PROGRESS_Y)))
                .is_none()
        );
        assert!(
            panel
                .handle_pointer(release(), (200.0, f64::from(PROGRESS_Y)))
                .is_none()
        );
        assert!(panel.drag.is_none());
    }

    #[test]
    fn changed_track_invalidates_seek_preview() {
        let mut snapshot = PlaybackSnapshot {
            line: "Song".into(),
            duration: 200,
            ..Default::default()
        };
        let drag = SeekDrag {
            position: 100,
            duration: 200,
            line: snapshot.line.clone(),
        };
        assert!(drag.valid_for(&snapshot));
        snapshot.position = 20;
        assert!(drag.valid_for(&snapshot));
        snapshot.line = "Other song".into();
        assert!(!drag.valid_for(&snapshot));
    }

    #[test]
    fn formats_hour_long_durations() {
        assert_eq!(format_time(90), "01:30");
        assert_eq!(format_time(3661), "1:01:01");
    }
}
