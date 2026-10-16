use std::cell::RefCell;
use std::thread;
use std::time::{Duration, Instant};

use super::{
    PlaybackInfo, PlaybackSnapshot,
    bubble::{
        LyricBubbleContent, LyricOrientation, RasterizedText, load_font, rasterize_text_at_size,
        render_lyric_bubble,
    },
};
use crate::cmus::{PlaybackCommand, control_cmus, seek_cmus};

const MIN_WIDTH: u16 = 84;
const HEADER_HEIGHT: u16 = 18;
const TITLE_FONT_SIZE: u16 = 12;
const TITLE_PADDING: u16 = 8;
const TITLE_SCROLL_GAP: usize = 32;
const FOOTER_HEIGHT: u16 = 34;
const BUTTON_WIDTH: u16 = 22;
const BUTTON_HEIGHT: u16 = 18;
const LEAVE_GRACE: Duration = Duration::from_millis(140);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BubbleRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl BubbleRect {
    fn contains(self, (x, y): (f64, f64)) -> bool {
        x >= f64::from(self.x)
            && x < f64::from(self.x + self.width)
            && y >= f64::from(self.y)
            && y < f64::from(self.y + self.height)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct BubbleLayout {
    pub width: u16,
    pub height: u16,
    pub lyric: BubbleRect,
}

impl BubbleLayout {
    pub(super) fn from_size(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            lyric: BubbleRect {
                x: 0,
                y: HEADER_HEIGHT,
                width,
                height: height.saturating_sub(HEADER_HEIGHT + FOOTER_HEIGHT),
            },
        }
    }

    fn progress(self) -> BubbleRect {
        BubbleRect {
            x: 8,
            y: self.lyric.y + self.lyric.height + 2,
            width: self.width.saturating_sub(16),
            height: 10,
        }
    }

    fn button_rect(self, target: Target) -> BubbleRect {
        let center = self.width / 2;
        let x = match target {
            Target::Previous => center.saturating_sub(BUTTON_WIDTH + BUTTON_WIDTH / 2),
            Target::Next => center + BUTTON_WIDTH / 2,
            _ => center.saturating_sub(BUTTON_WIDTH / 2),
        };
        BubbleRect {
            x,
            y: self.lyric.y + self.lyric.height + 12,
            width: BUTTON_WIDTH,
            height: BUTTON_HEIGHT,
        }
    }

    fn target(self, point: (f64, f64)) -> Option<Target> {
        if self.progress().contains(point) {
            return Some(Target::Seek);
        }
        [Target::Previous, Target::Toggle, Target::Next]
            .into_iter()
            .find(|target| self.button_rect(*target).contains(point))
    }

    fn seek_position(self, x: f64, duration: u64) -> u64 {
        let track = self.progress();
        if track.width == 0 {
            return 0;
        }
        (((x - f64::from(track.x)) / f64::from(track.width)).clamp(0.0, 1.0) * duration as f64)
            .round() as u64
    }

    pub(super) fn input_rects(self, expanded: bool) -> Vec<BubbleRect> {
        let mut rects = vec![self.lyric];
        if expanded {
            rects.push(BubbleRect {
                x: 0,
                y: 0,
                width: self.width,
                height: self.lyric.y,
            });
            rects.push(BubbleRect {
                x: 0,
                y: self.lyric.y + self.lyric.height,
                width: self.width,
                height: FOOTER_HEIGHT,
            });
        }
        rects
    }
}

pub(super) struct BubbleFrame {
    pub pixels: Vec<u8>,
    pub layout: BubbleLayout,
    pub expanded: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Previous,
    Toggle,
    Next,
    Seek,
}

pub(super) enum BubbleAction {
    Previous,
    Toggle,
    Next,
    Seek(u64),
}

struct Seek {
    position: u64,
    duration: u64,
    line: String,
}

struct CachedTitle {
    title: String,
    text: Option<RasterizedText>,
    started_at: Instant,
}

impl Seek {
    fn valid(&self, playback: &PlaybackSnapshot) -> bool {
        self.duration == playback.duration && self.line == playback.line
    }
}

pub(super) struct BubbleControls {
    playback: PlaybackInfo,
    hovered: bool,
    leave_deadline: Option<Instant>,
    hovered_target: Option<Target>,
    pressed: Option<Target>,
    press_layout: Option<BubbleLayout>,
    lyric_drag_layout: Option<BubbleLayout>,
    seek: Option<Seek>,
    preview: Option<(Seek, Instant)>,
    title: RefCell<Option<CachedTitle>>,
    hover_started: Instant,
}

impl BubbleControls {
    pub(super) fn new(playback: PlaybackInfo) -> Self {
        Self {
            playback,
            hovered: false,
            leave_deadline: None,
            hovered_target: None,
            pressed: None,
            press_layout: None,
            lyric_drag_layout: None,
            seek: None,
            preview: None,
            title: RefCell::new(None),
            hover_started: Instant::now(),
        }
    }

    pub(super) fn enter(&mut self) {
        let now = Instant::now();
        if !self.expanded_at(now, false) {
            self.hover_started = now;
        }
        self.hovered = true;
        self.leave_deadline = None;
    }

    pub(super) fn leave(&mut self) {
        self.hovered = false;
        self.hovered_target = None;
        self.leave_deadline = Some(Instant::now() + LEAVE_GRACE);
    }

    pub(super) fn cancel(&mut self) {
        self.hovered = false;
        self.leave_deadline = None;
        self.hovered_target = None;
        self.pressed = None;
        self.press_layout = None;
        self.lyric_drag_layout = None;
        self.seek = None;
        self.preview = None;
    }

    fn expanded_at(&self, now: Instant, dragging_lyric: bool) -> bool {
        self.hovered
            || self.pressed.is_some()
            || dragging_lyric
            || self.leave_deadline.is_some_and(|deadline| now < deadline)
    }

    pub(super) fn pressed(&self) -> bool {
        self.pressed.is_some()
    }

    pub(super) fn start_lyric_drag(&mut self, layout: BubbleLayout) {
        self.lyric_drag_layout = Some(layout);
    }

    pub(super) fn finish_lyric_drag(&mut self) {
        self.lyric_drag_layout = None;
    }

    pub(super) fn press(&mut self, point: (f64, f64), layout: BubbleLayout) -> bool {
        if !self.expanded_at(Instant::now(), false) {
            return false;
        }
        let Some(target) = layout.target(point) else {
            return false;
        };
        self.pressed = Some(target);
        self.press_layout = Some(layout);
        self.hovered_target = Some(target);
        if target == Target::Seek {
            let playback = self.playback.snapshot();
            self.preview = None;
            if playback.duration > 0 {
                self.seek = Some(Seek {
                    position: layout.seek_position(point.0, playback.duration),
                    duration: playback.duration,
                    line: playback.line,
                });
            }
        }
        true
    }

    pub(super) fn motion(&mut self, point: (f64, f64), layout: BubbleLayout) {
        let layout = self.press_layout.unwrap_or(layout);
        self.hovered_target = if self.pressed == Some(Target::Seek) {
            Some(Target::Seek)
        } else {
            layout.target(point)
        };
        if let Some(seek) = self.seek.as_mut() {
            seek.position = layout.seek_position(point.0, seek.duration);
        }
    }

    pub(super) fn release(
        &mut self,
        point: (f64, f64),
        layout: BubbleLayout,
    ) -> Option<BubbleAction> {
        let target = self.pressed.take()?;
        let layout = self.press_layout.take().unwrap_or(layout);
        if target == Target::Seek {
            let mut seek = self.seek.take()?;
            if !seek.valid(&self.playback.snapshot()) {
                return None;
            }
            seek.position = layout.seek_position(point.0, seek.duration);
            let position = seek.position;
            self.preview = Some((seek, Instant::now()));
            return Some(BubbleAction::Seek(position));
        }
        if layout.target(point) != Some(target) {
            return None;
        }
        match target {
            Target::Previous => Some(BubbleAction::Previous),
            Target::Toggle => Some(BubbleAction::Toggle),
            Target::Next => Some(BubbleAction::Next),
            Target::Seek => None,
        }
    }

    pub(super) fn render(
        &self,
        content: &LyricBubbleContent,
        orientation: LyricOrientation,
        elapsed: Duration,
        screen: (u16, u16),
        dragging_lyric: bool,
    ) -> BubbleFrame {
        let (body, body_width, body_height) = render_lyric_bubble(
            content,
            orientation,
            elapsed,
            screen.0,
            screen.1.saturating_sub(HEADER_HEIGHT + FOOTER_HEIGHT),
        );
        let width = body_width
            .max(MIN_WIDTH)
            .min(screen.0.saturating_sub(16).max(1));
        let body_height = body_height.min(
            screen
                .1
                .saturating_sub(HEADER_HEIGHT + FOOTER_HEIGHT)
                .max(1),
        );
        let height = body_height.saturating_add(HEADER_HEIGHT + FOOTER_HEIGHT);
        let lyric = BubbleRect {
            x: width.saturating_sub(body_width) / 2,
            y: HEADER_HEIGHT,
            width: body_width.min(width),
            height: body_height,
        };
        let layout = self
            .press_layout
            .or(self.lyric_drag_layout)
            .unwrap_or(BubbleLayout {
                width,
                height,
                lyric,
            });
        let (width, height, lyric) = (layout.width, layout.height, layout.lyric);
        let mut pixels = vec![0; usize::from(width) * usize::from(height) * 4];
        for row in 0..usize::from(body_height.min(lyric.height)) {
            let source = row * usize::from(body_width) * 4;
            let target =
                ((row + usize::from(lyric.y)) * usize::from(width) + usize::from(lyric.x)) * 4;
            let len = usize::from(lyric.width.min(body_width)) * 4;
            pixels[target..target + len].copy_from_slice(&body[source..source + len]);
        }
        let now = Instant::now();
        let expanded = self.expanded_at(now, dragging_lyric);
        if expanded {
            let mut playback = self.playback.snapshot();
            self.draw_title(&mut pixels, layout, &playback.title, now);
            if let Some(seek) = &self.seek {
                if seek.valid(&playback) {
                    playback.position = seek.position;
                }
            } else if let Some((seek, since)) = &self.preview
                && seek.valid(&playback)
                && playback.position.abs_diff(seek.position) > 1
                && since.elapsed() < Duration::from_secs(1)
            {
                playback.position = seek.position;
            }
            draw_footer(&mut pixels, layout, &playback, self.hovered_target);
        }
        BubbleFrame {
            pixels,
            layout,
            expanded,
        }
    }

    fn draw_title(&self, pixels: &mut [u8], layout: BubbleLayout, title: &str, now: Instant) {
        let mut cache = self.title.borrow_mut();
        if cache.as_ref().is_none_or(|cached| cached.title != title) {
            *cache = Some(CachedTitle {
                title: title.to_string(),
                text: load_font(title)
                    .map(|font| rasterize_text_at_size(&font, title, TITLE_FONT_SIZE)),
                started_at: now,
            });
        }
        let cached = cache.as_ref().expect("title cache initialized");
        let elapsed = now.saturating_duration_since(cached.started_at.max(self.hover_started));
        draw_header(pixels, layout, cached.text.as_ref(), elapsed);
    }
}

pub(super) fn dispatch(action: BubbleAction) {
    thread::spawn(move || {
        let result = match action {
            BubbleAction::Previous => control_cmus(PlaybackCommand::Previous),
            BubbleAction::Toggle => control_cmus(PlaybackCommand::TogglePause),
            BubbleAction::Next => control_cmus(PlaybackCommand::Next),
            BubbleAction::Seek(position) => seek_cmus(position),
        };
        if let Err(error) = result {
            tracing::warn!(%error, "lyric hover control failed");
        }
    });
}

fn fill(pixels: &mut [u8], layout: BubbleLayout, rect: BubbleRect, color: [u8; 3]) {
    for y in rect.y..rect.y.saturating_add(rect.height).min(layout.height) {
        for x in rect.x..rect.x.saturating_add(rect.width).min(layout.width) {
            let offset = (usize::from(y) * usize::from(layout.width) + usize::from(x)) * 4;
            pixels[offset..offset + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
        }
    }
}

fn rect(x: u16, y: u16, width: u16, height: u16) -> BubbleRect {
    BubbleRect {
        x,
        y,
        width,
        height,
    }
}

fn draw_header(
    pixels: &mut [u8],
    layout: BubbleLayout,
    title: Option<&RasterizedText>,
    elapsed: Duration,
) {
    let height = layout.lyric.y.saturating_sub(2);
    fill(
        pixels,
        layout,
        rect(0, 0, layout.width, height),
        [10, 16, 20],
    );
    let Some(text) = title else {
        return;
    };
    let viewport = usize::from(layout.width.saturating_sub(TITLE_PADDING * 2));
    let overflow = text.width > viewport;
    let cycle = text.width + TITLE_SCROLL_GAP;
    let scroll = if overflow {
        (elapsed.as_millis().saturating_sub(800) / 45) as usize % cycle
    } else {
        0
    };
    let left = if overflow {
        usize::from(TITLE_PADDING)
    } else {
        usize::from(layout.width).saturating_sub(text.width) / 2
    };
    let top = usize::from(height).saturating_sub(text.height) / 2;
    for y in 0..text.height.min(usize::from(height)) {
        for x in 0..viewport.min(text.width) {
            let source_x = if overflow { (scroll + x) % cycle } else { x };
            if source_x >= text.width {
                continue;
            }
            let alpha = u16::from(text.alpha[y * text.width + source_x]);
            let target = ((top + y) * usize::from(layout.width) + left + x) * 4;
            for (channel, color) in pixels[target..target + 3]
                .iter_mut()
                .zip([175_u16, 190, 184])
            {
                *channel = ((u16::from(*channel) * (255 - alpha) + color * alpha) / 255) as u8;
            }
        }
    }
}

fn draw_footer(
    pixels: &mut [u8],
    layout: BubbleLayout,
    playback: &PlaybackSnapshot,
    hovered: Option<Target>,
) {
    fill(
        pixels,
        layout,
        rect(
            0,
            layout.lyric.y + layout.lyric.height + 2,
            layout.width,
            FOOTER_HEIGHT - 4,
        ),
        [10, 16, 20],
    );
    let track = layout.progress();
    fill(
        pixels,
        layout,
        rect(track.x, track.y + 3, track.width, 3),
        [65, 75, 78],
    );
    let filled = super::progress_offset(playback.position, playback.duration, track.width);
    fill(
        pixels,
        layout,
        rect(track.x, track.y + 3, filled, 3),
        [70, 230, 158],
    );
    if playback.duration > 0 {
        fill(
            pixels,
            layout,
            rect(
                track.x + filled.min(track.width.saturating_sub(2)),
                track.y + 1,
                2,
                7,
            ),
            [230, 245, 240],
        );
    }
    for target in [Target::Previous, Target::Toggle, Target::Next] {
        let button = layout.button_rect(target);
        if hovered == Some(target) {
            fill(pixels, layout, button, [29, 96, 72]);
        }
        let cx = button.x + button.width / 2;
        let cy = button.y + button.height / 2;
        let color = [230, 240, 235];
        match target {
            Target::Previous => {
                fill(
                    pixels,
                    layout,
                    rect(cx.saturating_sub(7), cy - 5, 1, 11),
                    color,
                );
                triangle(pixels, layout, cx + 1, cy, false, color);
            }
            Target::Toggle if playback.status == "playing" => {
                fill(
                    pixels,
                    layout,
                    rect(cx.saturating_sub(4), cy - 5, 2, 11),
                    color,
                );
                fill(pixels, layout, rect(cx + 2, cy - 5, 2, 11), color);
            }
            Target::Toggle => triangle(pixels, layout, cx, cy, true, color),
            Target::Next => {
                triangle(pixels, layout, cx.saturating_sub(1), cy, true, color);
                fill(pixels, layout, rect(cx + 6, cy - 5, 1, 11), color);
            }
            _ => {}
        }
    }
}

fn triangle(
    pixels: &mut [u8],
    layout: BubbleLayout,
    cx: u16,
    cy: u16,
    right: bool,
    color: [u8; 3],
) {
    for step in 0..6 {
        let half = 5 - step;
        let x = if right {
            cx.saturating_sub(3) + step
        } else {
            (cx + 3).saturating_sub(step)
        };
        fill(pixels, layout, rect(x, cy - half, 1, half * 2 + 1), color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Locale;

    fn controls(duration: u64) -> BubbleControls {
        let playback = PlaybackInfo::new(Locale::En);
        playback.update("Song".into(), "Song", "Lyric", "playing", 20, duration);
        let controls = BubbleControls::new(playback);
        *controls.title.borrow_mut() = Some(CachedTitle {
            title: "Song".into(),
            text: Some(RasterizedText {
                width: 24,
                height: 9,
                alpha: vec![255; 24 * 9],
            }),
            started_at: Instant::now(),
        });
        controls
    }

    fn frame(controls: &BubbleControls) -> BubbleFrame {
        controls.render(
            &LyricBubbleContent::fixture(24, 14),
            LyricOrientation::Horizontal,
            Duration::ZERO,
            (1920, 1080),
            false,
        )
    }

    #[test]
    fn hover_reveals_title_and_footer_without_resizing_or_moving_the_lyric() {
        let mut controls = controls(200);
        let hidden = frame(&controls);
        assert!(!hidden.expanded);
        assert_eq!(hidden.layout.input_rects(false).len(), 1);
        let lyric_offset =
            usize::from(hidden.layout.lyric.y) * usize::from(hidden.layout.width) * 4;
        let footer_offset = usize::from(hidden.layout.lyric.y + hidden.layout.lyric.height)
            * usize::from(hidden.layout.width)
            * 4;
        assert!(hidden.pixels[..lyric_offset].iter().all(|byte| *byte == 0));
        assert!(hidden.pixels[footer_offset..].iter().all(|byte| *byte == 0));
        controls.enter();
        let shown = frame(&controls);
        assert!(shown.expanded);
        assert_eq!(
            (hidden.layout.width, hidden.layout.height),
            (shown.layout.width, shown.layout.height)
        );
        assert_eq!(hidden.layout.lyric, shown.layout.lyric);
        assert_eq!(
            &hidden.pixels[lyric_offset..footer_offset],
            &shown.pixels[lyric_offset..footer_offset]
        );
        assert_eq!(shown.layout.input_rects(true).len(), 3);
        assert!(shown.pixels[..lyric_offset].iter().any(|byte| *byte != 0));
        assert!(shown.pixels[footer_offset..].iter().any(|byte| *byte != 0));
    }

    #[test]
    fn vertical_lyrics_remain_narrow_with_transparent_side_margins() {
        let controls = controls(200);
        let frame = controls.render(
            &LyricBubbleContent::fixture(8, 14),
            LyricOrientation::Vertical,
            Duration::ZERO,
            (1920, 1080),
            false,
        );
        assert_eq!(frame.layout.width, MIN_WIDTH);
        assert!(frame.layout.lyric.width < frame.layout.width);
        assert!(frame.layout.lyric.x > 0);
        assert_eq!(&frame.pixels[..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn header_is_hoverable_only_while_expanded_and_has_no_playback_action() {
        let mut controls = controls(200);
        let layout = frame(&controls).layout;
        let point = (f64::from(layout.width / 2), 8.0);
        assert!(
            !layout
                .input_rects(false)
                .iter()
                .any(|rect| rect.contains(point))
        );
        assert!(
            layout
                .input_rects(true)
                .iter()
                .any(|rect| rect.contains(point))
        );
        controls.enter();
        assert!(!controls.press(point, layout));
        assert!(!controls.pressed());
        controls.leave();
        controls.leave_deadline = Some(Instant::now() - Duration::from_millis(1));
        let hidden = frame(&controls);
        assert!(!hidden.expanded);
        let header_end = usize::from(layout.lyric.y) * usize::from(layout.width) * 4;
        assert!(hidden.pixels[..header_end].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn title_cache_survives_lyric_updates_but_refreshes_on_track_change() {
        let mut controls = controls(200);
        controls.enter();
        let started_at = Instant::now() - Duration::from_secs(2);
        controls.title.borrow_mut().as_mut().unwrap().started_at = started_at;
        let initial = frame(&controls);
        controls
            .playback
            .update("Song".into(), "Song", "Next lyric", "paused", 21, 200);
        let updated = frame(&controls);
        assert_eq!(
            controls.title.borrow().as_ref().unwrap().started_at,
            started_at
        );
        let header_end =
            usize::from(initial.layout.lyric.y) * usize::from(initial.layout.width) * 4;
        assert_eq!(&initial.pixels[..header_end], &updated.pixels[..header_end]);
        controls.playback.update(
            "Other song - Artist · paused".into(),
            "Other song",
            "Next lyric",
            "paused",
            0,
            180,
        );
        let changed = frame(&controls);
        let cache = controls.title.borrow();
        assert_eq!(cache.as_ref().unwrap().title, "Other song");
        assert!(cache.as_ref().unwrap().started_at > started_at);
        assert_eq!(initial.layout.lyric, changed.layout.lyric);
    }

    #[test]
    fn long_title_scrolls_in_one_small_row_without_touching_lyrics_or_controls() {
        let mut controls = controls(200);
        controls.enter();
        let layout = frame(&controls).layout;
        let text = RasterizedText {
            width: 300,
            height: 9,
            alpha: (0..300 * 9)
                .map(|index| if index % 7 < 3 { 255 } else { 0 })
                .collect(),
        };
        let mut first = vec![0; usize::from(layout.width) * usize::from(layout.height) * 4];
        let mut scrolled = first.clone();
        draw_header(&mut first, layout, Some(&text), Duration::ZERO);
        draw_header(&mut scrolled, layout, Some(&text), Duration::from_secs(3));
        let header_end = usize::from(layout.lyric.y) * usize::from(layout.width) * 4;
        assert_ne!(&first[..header_end], &scrolled[..header_end]);
        assert!(first[header_end..].iter().all(|byte| *byte == 0));
        assert!(scrolled[header_end..].iter().all(|byte| *byte == 0));
        assert!(layout.width < text.width as u16);
    }

    #[test]
    fn leave_grace_and_active_press_prevent_hover_flicker() {
        let mut controls = controls(200);
        controls.enter();
        controls.leave();
        let deadline = controls.leave_deadline.unwrap();
        assert!(controls.expanded_at(deadline - Duration::from_millis(1), false));
        assert!(!controls.expanded_at(deadline + Duration::from_millis(1), false));
        controls.enter();
        let layout = frame(&controls).layout;
        let target = layout.button_rect(Target::Toggle);
        assert!(controls.press((f64::from(target.x + 10), f64::from(target.y + 8)), layout));
        controls.leave();
        assert!(controls.expanded_at(Instant::now() + Duration::from_secs(10), false));
        controls.cancel();
        assert!(!controls.expanded_at(Instant::now(), false));
    }

    #[test]
    fn lyric_press_does_not_trigger_a_playback_control() {
        let mut controls = controls(200);
        controls.enter();
        let layout = frame(&controls).layout;
        assert!(!controls.press(
            (f64::from(layout.lyric.x + 8), f64::from(layout.lyric.y + 8)),
            layout
        ));
        assert!(!controls.pressed());
    }

    #[test]
    fn progress_drag_previews_and_submits_only_once_on_release() {
        let mut controls = controls(200);
        controls.enter();
        let layout = frame(&controls).layout;
        let track = layout.progress();
        let start = (f64::from(track.x), f64::from(track.y + 3));
        let middle = (f64::from(track.x) + f64::from(track.width) / 2.0, start.1);
        assert!(controls.press(start, layout));
        controls.motion(middle, layout);
        assert_eq!(controls.seek.as_ref().unwrap().position, 100);
        assert!(matches!(
            controls.release(middle, layout),
            Some(BubbleAction::Seek(100))
        ));
        assert!(!controls.pressed());
        assert!(controls.release(middle, layout).is_none());
    }

    #[test]
    fn progress_drag_geometry_is_frozen_across_lyric_updates() {
        let mut controls = controls(200);
        controls.enter();
        let layout = frame(&controls).layout;
        let track = layout.progress();
        let middle = (
            f64::from(track.x) + f64::from(track.width) / 2.0,
            f64::from(track.y + 3),
        );
        controls.press(middle, layout);
        let changed = controls.render(
            &LyricBubbleContent::fixture(240, 20),
            LyricOrientation::Horizontal,
            Duration::ZERO,
            (1920, 1080),
            false,
        );
        assert_eq!(
            (changed.layout.width, changed.layout.height),
            (layout.width, layout.height)
        );
        controls.motion(middle, BubbleLayout::from_size(300, 100));
        assert!(matches!(
            controls.release(middle, changed.layout),
            Some(BubbleAction::Seek(100))
        ));
    }

    #[test]
    fn moving_the_lyric_keeps_its_geometry_stable_until_release() {
        let mut controls = controls(200);
        let layout = frame(&controls).layout;
        controls.start_lyric_drag(layout);
        let content = LyricBubbleContent::fixture(240, 20);
        let held = controls.render(
            &content,
            LyricOrientation::Horizontal,
            Duration::ZERO,
            (1920, 1080),
            true,
        );
        assert_eq!(
            (held.layout.width, held.layout.height),
            (layout.width, layout.height)
        );
        controls.finish_lyric_drag();
        let released = controls.render(
            &content,
            LyricOrientation::Horizontal,
            Duration::ZERO,
            (1920, 1080),
            false,
        );
        assert!(released.layout.width > held.layout.width);
    }

    #[test]
    fn unknown_duration_and_changed_tracks_do_not_seek() {
        let mut controls = controls(0);
        controls.enter();
        let layout = frame(&controls).layout;
        let track = layout.progress();
        let point = (f64::from(track.x + 10), f64::from(track.y + 3));
        assert!(controls.press(point, layout));
        assert!(controls.release(point, layout).is_none());
        controls
            .playback
            .update("Song".into(), "Song", "Lyric", "playing", 20, 200);
        controls.press(point, layout);
        controls.playback.update(
            "Other song".into(),
            "Other song",
            "Other lyric",
            "playing",
            0,
            180,
        );
        assert!(controls.release(point, layout).is_none());
    }

    #[test]
    fn transport_buttons_have_distinct_actions() {
        let mut controls = controls(200);
        controls.enter();
        let layout = frame(&controls).layout;
        for target in [Target::Previous, Target::Toggle, Target::Next] {
            let button = layout.button_rect(target);
            let point = (f64::from(button.x + 10), f64::from(button.y + 8));
            controls.press(point, layout);
            assert!(matches!(
                (target, controls.release(point, layout)),
                (Target::Previous, Some(BubbleAction::Previous))
                    | (Target::Toggle, Some(BubbleAction::Toggle))
                    | (Target::Next, Some(BubbleAction::Next))
            ));
        }
    }

    #[test]
    #[ignore = "requires installed fonts; generates offline lyric hover previews"]
    fn render_hover_preview() {
        let text = "这是一句歌词";
        let font = super::super::bubble::load_font(text).expect("installed preview font");
        let content = LyricBubbleContent::new(&font, text, 14);
        let mut controls = controls(200);
        controls.playback.update(
            "小小的歌名 - 歌手 · playing".into(),
            "小小的歌名",
            text,
            "playing",
            20,
            200,
        );
        for expanded in [false, true] {
            if expanded {
                controls.enter();
            }
            let frame = controls.render(
                &content,
                LyricOrientation::Horizontal,
                Duration::ZERO,
                (1920, 1080),
                false,
            );
            image::RgbaImage::from_raw(
                u32::from(frame.layout.width),
                u32::from(frame.layout.height),
                frame.pixels,
            )
            .unwrap()
            .save(format!(
                "/tmp/opencode/ctlyrics-lyric-{}.png",
                if expanded { "hover" } else { "idle" }
            ))
            .unwrap();
        }
    }
}
