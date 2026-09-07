use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, point};

pub(super) const DEFAULT_FONT_SIZE: u16 = 14;
const MIN_FONT_SIZE: u16 = 10;
const MAX_FONT_SIZE: u16 = 48;
const FONT_SIZE_STEP: u16 = 2;
const PADDING: usize = 14;
const MAX_WIDTH: u16 = 520;
const SCROLL_GAP: usize = 48;

pub(super) struct RasterizedText {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) alpha: Vec<u8>,
}

pub(super) struct LyricBubbleContent {
    horizontal: RasterizedText,
    vertical: RasterizedText,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LyricOrientation {
    Horizontal,
    Vertical,
}

impl LyricBubbleContent {
    pub(super) fn new(font: &FontVec, text: &str, font_size: u16) -> Self {
        Self {
            horizontal: rasterize_text_at_size(font, text, font_size),
            vertical: rasterize_vertical_text(font, text, font_size),
        }
    }

    fn text(&self, orientation: LyricOrientation) -> &RasterizedText {
        match orientation {
            LyricOrientation::Horizontal => &self.horizontal,
            LyricOrientation::Vertical => &self.vertical,
        }
    }
}

pub(super) fn load_font_size() -> u16 {
    load_font_size_from(&crate::paths::get().bubble_font_size_file())
}

pub(super) fn save_font_size(font_size: u16) -> io::Result<()> {
    let path = crate::paths::get().bubble_font_size_file();
    save_font_size_to(&path, font_size)
}

fn save_font_size_to(path: &Path, font_size: u16) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, font_size.to_string())
}

pub(super) fn adjusted_font_size(font_size: u16, increase: bool) -> u16 {
    if increase {
        font_size.saturating_add(FONT_SIZE_STEP).min(MAX_FONT_SIZE)
    } else {
        font_size.saturating_sub(FONT_SIZE_STEP).max(MIN_FONT_SIZE)
    }
}

fn load_font_size_from(path: &Path) -> u16 {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|value| (MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(value))
        .unwrap_or(DEFAULT_FONT_SIZE)
}

pub(super) fn load_font(sample: &str) -> Option<FontVec> {
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
        if let Ok(font) = FontVec::try_from_vec_and_index(data, collection_index) {
            return Some(font);
        }
    }
    None
}

pub(super) fn font_supports(font: &FontVec, text: &str) -> bool {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .all(|character| font.glyph_id(character) != GlyphId(0))
}

pub(super) fn rasterize_text(font: &FontVec, text: &str) -> RasterizedText {
    rasterize_text_at_size(font, text, DEFAULT_FONT_SIZE)
}

fn rasterize_text_at_size(font: &FontVec, text: &str, font_size: u16) -> RasterizedText {
    let scale = PxScale::from(f32::from(font_size));
    let scaled = font.as_scaled(scale);
    let mut caret = 0.0;
    let mut previous = None;
    let mut glyphs = Vec::new();
    let mut min_x = 0.0_f32;
    let mut min_y = 0.0_f32;
    let mut max_x = 0.0_f32;
    let mut max_y = 0.0_f32;

    for character in text.chars().filter(|character| *character != '\r') {
        if character == '\n' {
            break;
        }
        let id = scaled.glyph_id(character);
        if let Some(previous) = previous {
            caret += scaled.kern(previous, id);
        }
        let glyph = id.with_scale_and_position(scale, point(caret, scaled.ascent()));
        caret += scaled.h_advance(id);
        previous = Some(id);
        if let Some(outlined) = scaled.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            min_x = min_x.min(bounds.min.x);
            min_y = min_y.min(bounds.min.y);
            max_x = max_x.max(bounds.max.x);
            max_y = max_y.max(bounds.max.y);
            glyphs.push(outlined);
        }
    }

    max_x = max_x.max(caret);
    let width = (max_x.ceil() - min_x.floor()).max(0.0) as usize;
    let height = (max_y.ceil() - min_y.floor()).max(0.0) as usize;
    let mut alpha = vec![0; width * height];
    for glyph in glyphs {
        let bounds = glyph.px_bounds();
        let offset_x = (bounds.min.x - min_x.floor()).max(0.0) as usize;
        let offset_y = (bounds.min.y - min_y.floor()).max(0.0) as usize;
        glyph.draw(|x, y, coverage| {
            let destination = (offset_y + y as usize) * width + offset_x + x as usize;
            let coverage = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
            alpha[destination] = alpha[destination].max(coverage);
        });
    }
    RasterizedText {
        width,
        height,
        alpha,
    }
}

pub(super) fn render_lyric_bubble(
    content: &LyricBubbleContent,
    orientation: LyricOrientation,
    elapsed: Duration,
    screen_width: u16,
    screen_height: u16,
) -> (Vec<u8>, u16, u16) {
    let text = content.text(orientation);
    let padding = PADDING * 2;
    let (width, height) = match orientation {
        LyricOrientation::Horizontal => {
            let limit = usize::from(MAX_WIDTH.min(screen_width.saturating_sub(16))).max(1);
            (
                (text.width + padding).min(limit).max(limit.min(80)),
                (text.height + padding).max(42),
            )
        }
        LyricOrientation::Vertical => {
            let limit = usize::from(screen_height.saturating_sub(16)).max(1);
            (
                (text.width + padding).max(42),
                (text.height + padding).min(limit).max(limit.min(80)),
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
    fill_rect(&mut pixels, width, height, 0, 0, width, 1, [70, 230, 158]);
    fill_rect(
        &mut pixels,
        width,
        height,
        0,
        height as i16 - 1,
        width,
        1,
        [70, 230, 158],
    );
    fill_rect(&mut pixels, width, height, 0, 0, 1, height, [70, 230, 158]);
    fill_rect(
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

fn rasterize_vertical_text(font: &FontVec, text: &str, font_size: u16) -> RasterizedText {
    let glyphs: Vec<_> = text
        .chars()
        .filter(|character| !matches!(character, '\r' | '\n'))
        .map(|character| rasterize_text_at_size(font, &character.to_string(), font_size))
        .collect();
    let cell_height = glyphs
        .iter()
        .map(|glyph| glyph.height)
        .max()
        .unwrap_or(0)
        .max(usize::from(font_size));
    stack_vertical_glyphs(&glyphs, cell_height)
}

fn stack_vertical_glyphs(glyphs: &[RasterizedText], cell_height: usize) -> RasterizedText {
    let width = glyphs.iter().map(|glyph| glyph.width).max().unwrap_or(0);
    let height = glyphs.len() * cell_height;
    let mut alpha = vec![0; width * height];
    for (index, glyph) in glyphs.iter().enumerate() {
        let offset_x = width.saturating_sub(glyph.width) / 2;
        let offset_y = index * cell_height + cell_height.saturating_sub(glyph.height) / 2;
        for y in 0..glyph.height {
            for x in 0..glyph.width {
                let destination = (offset_y + y) * width + offset_x + x;
                alpha[destination] = alpha[destination].max(glyph.alpha[y * glyph.width + x]);
            }
        }
    }
    RasterizedText {
        width,
        height,
        alpha,
    }
}

fn draw_horizontal_lyric(
    pixels: &mut [u8],
    width: u16,
    height: u16,
    text: &RasterizedText,
    elapsed: Duration,
) {
    let viewport = usize::from(width).saturating_sub(PADDING * 2);
    let overflow = text.width > viewport;
    let cycle = text.width + SCROLL_GAP;
    let scroll = if overflow {
        (elapsed.as_millis() as usize / 35) % cycle
    } else {
        0
    };
    let left = if overflow {
        PADDING
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
            blend_pixel(
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
    let viewport = usize::from(height).saturating_sub(PADDING * 2);
    let overflow = text.height > viewport;
    let cycle = text.height + SCROLL_GAP;
    let scroll = if overflow {
        (elapsed.as_millis() as usize / 35) % cycle
    } else {
        0
    };
    let top = if overflow {
        PADDING
    } else {
        (usize::from(height).saturating_sub(text.height)) / 2
    };
    let left = (usize::from(width).saturating_sub(text.width)) / 2;
    for destination_y in 0..viewport {
        let source_y = if overflow {
            (scroll + destination_y) % cycle
        } else {
            destination_y
        };
        if source_y >= text.height {
            continue;
        }
        for source_x in 0..text.width {
            blend_pixel(
                pixels,
                width,
                left + source_x,
                top + destination_y,
                text.alpha[source_y * text.width + source_x],
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect(
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

fn blend_pixel(pixels: &mut [u8], width: u16, x: usize, y: usize, alpha: u8) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_file() -> std::path::PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ctlyrics-font-size-{id}"))
    }

    #[test]
    fn font_size_is_adjusted_within_bounds() {
        assert_eq!(adjusted_font_size(DEFAULT_FONT_SIZE, true), 16);
        assert_eq!(adjusted_font_size(DEFAULT_FONT_SIZE, false), 12);
        assert_eq!(adjusted_font_size(MAX_FONT_SIZE, true), MAX_FONT_SIZE);
        assert_eq!(adjusted_font_size(MIN_FONT_SIZE, false), MIN_FONT_SIZE);
    }

    #[test]
    fn persisted_font_size_rejects_invalid_values() {
        let path = temporary_file();
        assert_eq!(load_font_size_from(&path), DEFAULT_FONT_SIZE);

        save_font_size_to(&path, 32).unwrap();
        assert_eq!(load_font_size_from(&path), 32);

        fs::write(&path, "100").unwrap();
        assert_eq!(load_font_size_from(&path), DEFAULT_FONT_SIZE);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn vertical_glyphs_are_stacked_upright_from_top_to_bottom() {
        let glyphs = [
            RasterizedText {
                width: 2,
                height: 2,
                alpha: vec![255, 0, 0, 128],
            },
            RasterizedText {
                width: 1,
                height: 1,
                alpha: vec![64],
            },
        ];

        let text = stack_vertical_glyphs(&glyphs, 3);

        assert_eq!((text.width, text.height), (2, 6));
        assert_eq!(&text.alpha[0..4], &[255, 0, 0, 128]);
        assert_eq!(&text.alpha[8..10], &[64, 0]);
    }

    #[test]
    fn vertical_drawing_does_not_rotate_the_cached_text() {
        let text = RasterizedText {
            width: 2,
            height: 3,
            alpha: vec![255, 0, 0, 0, 0, 255],
        };
        let width = 32;
        let height = 31;
        let mut pixels = vec![0; usize::from(width) * usize::from(height) * 4];

        draw_vertical_lyric(&mut pixels, width, height, &text, Duration::ZERO);

        let pixel = |x: usize, y: usize| {
            let offset = (y * usize::from(width) + x) * 4;
            &pixels[offset..offset + 3]
        };
        assert_eq!(pixel(15, 14), &[245, 245, 245]);
        assert_eq!(pixel(16, 16), &[245, 245, 245]);
        assert_eq!(pixel(16, 14), &[0, 0, 0]);
        assert_eq!(pixel(15, 16), &[0, 0, 0]);
    }
}
