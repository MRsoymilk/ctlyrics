use std::thread;
use std::time::{Duration, Instant};

use ab_glyph::FontVec;
use anyhow::Result;
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::reexports::client::{
    QueueHandle,
    protocol::{wl_shm, wl_surface},
};
use smithay_client_toolkit::seat::pointer::{BTN_LEFT, PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::{
    WaylandSurface,
    wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell},
    xdg::{XdgShell, window::WindowDecorations},
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};

use super::{
    PlaybackInfo, PlaybackSnapshot,
    bubble::{RasterizedText, font_supports, load_font, rasterize_text},
    wayland::{ShellSurface, State},
};
use crate::cmus::{PlaybackCommand, control_cmus, seek_cmus};
use crate::i18n::{Locale, tr};

const WIDTH: u16 = 420;
const HEIGHT: u16 = 134;
const PADDING: u16 = 12;
const TIME_GAP: u16 = 8;
const PROGRESS_Y: u16 = 56;
const BUTTON_Y: u16 = 86;
const BUTTON_WIDTH: u16 = 116;
const BUTTON_HEIGHT: u16 = 34;
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
            && (44.0..76.0).contains(&position.1)
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
    Close,
}

enum PanelAction {
    Seek(u64),
    Control(PlaybackCommand),
    Close,
}

pub(super) struct ControlPanel {
    pub(super) shell: Option<ShellSurface>,
    pub(super) configured: bool,
    pub(super) frame_ready: bool,
    pub(super) dirty: bool,
    playback: PlaybackInfo,
    locale: Locale,
    font: Option<FontVec>,
    time_metrics: Option<(u64, u16)>,
    buffer: Option<Buffer>,
    buffer_size: (u16, u16),
    width: u16,
    row: ProgressRow,
    drag: Option<SeekDrag>,
    preview: Option<(SeekDrag, Instant)>,
    pressed: Option<Button>,
    hovered: Option<Button>,
    last_draw: Instant,
}

impl ControlPanel {
    pub(super) fn new(locale: Locale, playback: PlaybackInfo) -> Self {
        Self {
            shell: None,
            configured: false,
            frame_ready: true,
            dirty: true,
            playback,
            locale,
            font: None,
            time_metrics: None,
            buffer: None,
            buffer_size: (0, 0),
            width: WIDTH,
            row: ProgressRow::new(WIDTH, 40, 40),
            drag: None,
            preview: None,
            pressed: None,
            hovered: None,
            last_draw: Instant::now(),
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
        qh: &QueueHandle<State>,
    ) {
        // Reopening the panel must not affect the independent lyric surface.
        if self.shell.is_some() {
            self.dirty = true;
            return;
        }
        let surface = compositor.create_surface(qh);
        if let Some(layer_shell) = layer_shell {
            let layer = layer_shell.create_layer_surface(
                qh,
                surface,
                Layer::Overlay,
                Some("ctlyrics-controls"),
                None,
            );
            layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
            layer.set_margin(16, 16, 0, 0);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_size(u32::from(self.width), u32::from(HEIGHT));
            layer.commit();
            self.shell = Some(ShellSurface::Layer(layer));
        } else if let Some(xdg_shell) = xdg_shell {
            let window = xdg_shell.create_window(surface, WindowDecorations::ServerDefault, qh);
            window.set_title(tr(self.locale, "tray_controls"));
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
        self.configured = false;
    }

    pub(super) fn pointer_event(&mut self, event: &PointerEvent) {
        match self.handle_pointer(event.kind.clone(), event.position) {
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
            None => {}
        }
    }

    fn handle_pointer(
        &mut self,
        kind: PointerEventKind,
        position: (f64, f64),
    ) -> Option<PanelAction> {
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
            "{} 0123456789: X {} {} {} {}",
            playback.line,
            tr(self.locale, "tray_previous"),
            tr(self.locale, "tray_pause"),
            tr(self.locale, "tray_play"),
            tr(self.locale, "tray_next")
        );
        if self
            .font
            .as_ref()
            .is_none_or(|font| !font_supports(font, &sample))
        {
            self.font = load_font(&sample);
            self.time_metrics = None;
        }
        let Some(font) = &self.font else {
            return Ok(());
        };
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
        );
        let shell = self.shell.as_ref().expect("visible control panel");
        let surface = shell.wl_surface().clone();
        if self.buffer_size != (self.width, HEIGHT) {
            shell.set_size(u32::from(self.width), u32::from(HEIGHT));
            self.buffer = None;
            self.buffer_size = (self.width, HEIGHT);
        }
        if self.buffer.is_none() {
            self.buffer = Some(
                pool.create_buffer(
                    i32::from(self.width),
                    i32::from(HEIGHT),
                    i32::from(self.width) * 4,
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
        for (source, destination) in pixels.chunks_exact(4).zip(canvas.chunks_exact_mut(4)) {
            destination.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
        }
        surface.damage_buffer(0, 0, i32::from(self.width), i32::from(HEIGHT));
        surface.frame(qh, surface.clone());
        buffer.attach_to(&surface)?;
        surface.commit();
        self.frame_ready = false;
        self.dirty = false;
        self.last_draw = Instant::now();
        Ok(())
    }
}

fn button_at(position: (f64, f64), width: u16) -> Option<Button> {
    if (f64::from(width - 36)..f64::from(width - PADDING)).contains(&position.0)
        && (8.0..36.0).contains(&position.1)
    {
        return Some(Button::Close);
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
        (Button::Previous, PADDING),
        (Button::Toggle, (width - BUTTON_WIDTH) / 2),
        (Button::Next, width - PADDING - BUTTON_WIDTH),
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
) -> Vec<u8> {
    let mut pixels = vec![0; usize::from(width) * usize::from(HEIGHT) * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[24, 24, 28, 255]);
    }
    blit(
        &mut pixels,
        width,
        &rasterize_text(font, &playback.line),
        PADDING,
        14,
        width - 52,
    );
    blit(
        &mut pixels,
        width,
        &rasterize_text(font, "X"),
        width - 30,
        14,
        width - PADDING,
    );
    blit(
        &mut pixels,
        width,
        elapsed,
        row.elapsed_x,
        48,
        row.track_x - TIME_GAP,
    );
    blit(
        &mut pixels,
        width,
        duration,
        row.duration_x,
        48,
        width - PADDING,
    );
    fill(
        &mut pixels,
        width,
        (row.track_x, PROGRESS_Y, row.track_width, 6),
        [70, 70, 78],
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
        (row.track_x, PROGRESS_Y, filled, 6),
        [70, 230, 158],
    );
    if playback.duration > 0 {
        let knob = row.track_x + filled.min(row.track_width.saturating_sub(5));
        fill(
            &mut pixels,
            width,
            (knob, PROGRESS_Y - 4, 5, 14),
            [235, 255, 246],
        );
    }
    for (button, x) in button_positions(width) {
        let color = if hovered == Some(button) {
            [29, 96, 72]
        } else {
            [44, 44, 52]
        };
        fill(
            &mut pixels,
            width,
            (x, BUTTON_Y, BUTTON_WIDTH, BUTTON_HEIGHT),
            color,
        );
        let label = match button {
            Button::Previous => "tray_previous",
            Button::Toggle if playback.status == "playing" => "tray_pause",
            Button::Toggle => "tray_play",
            Button::Next => "tray_next",
            Button::Close => unreachable!(),
        };
        let text = rasterize_text(font, tr(locale, label));
        let offset = BUTTON_WIDTH.saturating_sub(text.width as u16) / 2;
        blit(
            &mut pixels,
            width,
            &text,
            x + offset,
            BUTTON_Y + 8,
            x + BUTTON_WIDTH,
        );
    }
    pixels
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

    fn press() -> PointerEventKind {
        PointerEventKind::Press {
            time: 1,
            button: BTN_LEFT,
            serial: 1,
        }
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
        playback.update("Song".into(), "Lyric", "playing", 20, 200);
        let mut panel = ControlPanel::new(Locale::En, playback);
        let start = f64::from(panel.row.track_x);
        let middle = start + f64::from(panel.row.track_width) / 2.0;
        assert!(panel.handle_pointer(press(), (start, 56.0)).is_none());
        assert!(
            panel
                .handle_pointer(PointerEventKind::Motion { time: 2 }, (middle, 56.0))
                .is_none()
        );
        assert_eq!(panel.drag.as_ref().unwrap().position, 100);
        assert!(matches!(
            panel.handle_pointer(release(), (middle, 56.0)),
            Some(PanelAction::Seek(100))
        ));
        assert!(panel.drag.is_none());
        assert_eq!(panel.preview.as_ref().unwrap().0.position, 100);
        assert!(panel.handle_pointer(release(), (middle, 56.0)).is_none());
    }

    #[test]
    fn changing_tracks_during_drag_does_not_seek_the_new_track() {
        let playback = PlaybackInfo::new(Locale::En);
        playback.update("Song".into(), "Lyric", "playing", 20, 200);
        let mut panel = ControlPanel::new(Locale::En, playback.clone());
        let position = (f64::from(panel.row.track_x + 30), 56.0);
        panel.handle_pointer(press(), position);
        playback.update("Other song".into(), "Other lyric", "playing", 0, 180);
        assert!(panel.handle_pointer(release(), position).is_none());
        assert!(panel.drag.is_none());
    }

    #[test]
    fn closing_cancels_an_unfinished_drag() {
        let playback = PlaybackInfo::new(Locale::En);
        playback.update("Song".into(), "Lyric", "playing", 20, 200);
        let mut panel = ControlPanel::new(Locale::En, playback);
        let position = (f64::from(panel.row.track_x + 30), 56.0);
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
        let mut panel = ControlPanel::new(Locale::En, PlaybackInfo::new(Locale::En));
        assert_eq!(panel.row.seek_position(200.0, 0), 0);
        assert!(panel.handle_pointer(press(), (200.0, 56.0)).is_none());
        assert!(panel.handle_pointer(release(), (200.0, 56.0)).is_none());
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
