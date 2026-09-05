use anyhow::{Context, Result};
use image::imageops::FilterType;

const ICON_BYTES: &[u8] = include_bytes!("../../../res/logo_icon.png");

pub fn rgba_icon(width: u32, height: u32) -> Result<Vec<u8>> {
    Ok(image::load_from_memory(ICON_BYTES)
        .context("failed to decode tray icon")?
        .resize_exact(width, height, FilterType::Lanczos3)
        .into_rgba8()
        .into_raw())
}

pub fn argb_icon(size: u32) -> Result<Vec<u8>> {
    let image = rgba_icon(size, size)?;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for pixel in image.as_chunks::<4>().0 {
        pixels.extend_from_slice(&[pixel[3], pixel[0], pixel[1], pixel[2]]);
    }
    Ok(pixels)
}
