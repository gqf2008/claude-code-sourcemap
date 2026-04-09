//! Screenshot capture using the `screenshots` crate.
//!
//! Supports capturing the primary screen or a specific display.
//! Returns PNG-encoded images as base64 strings.

use base64::Engine;
use image::ImageEncoder;
use tracing::debug;

/// Capture a screenshot of the primary display.
///
/// Returns the screenshot as a base64-encoded PNG string.
pub fn capture_screen() -> anyhow::Result<ScreenshotResult> {
    let screens = screenshots::Screen::all()
        .map_err(|e| anyhow::anyhow!("Failed to enumerate screens: {e}"))?;

    let screen = screens.into_iter().next()
        .ok_or_else(|| anyhow::anyhow!("No displays found"))?;

    debug!(
        display_id = screen.display_info.id,
        width = screen.display_info.width,
        height = screen.display_info.height,
        "Capturing screenshot"
    );

    let image = screen.capture()
        .map_err(|e| anyhow::anyhow!("Screenshot capture failed: {e}"))?;

    let width = image.width();
    let height = image.height();
    let png_bytes = encode_png(&image)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);

    Ok(ScreenshotResult {
        base64_png: b64,
        width,
        height,
    })
}

/// Capture a specific region of the screen.
pub fn capture_region(x: i32, y: i32, width: u32, height: u32) -> anyhow::Result<ScreenshotResult> {
    let screens = screenshots::Screen::all()
        .map_err(|e| anyhow::anyhow!("Failed to enumerate screens: {e}"))?;

    let screen = screens.into_iter().next()
        .ok_or_else(|| anyhow::anyhow!("No displays found"))?;

    debug!(x, y, width, height, "Capturing screen region");

    let image = screen.capture_area(x, y, width, height)
        .map_err(|e| anyhow::anyhow!("Region capture failed: {e}"))?;

    let actual_w = image.width();
    let actual_h = image.height();
    let png_bytes = encode_png(&image)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);

    Ok(ScreenshotResult {
        base64_png: b64,
        width: actual_w,
        height: actual_h,
    })
}

/// Encode an RGBA image buffer to PNG bytes.
fn encode_png(image: &image::RgbaImage) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    encoder.write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ColorType::Rgba8,
    ).map_err(|e| anyhow::anyhow!("PNG encoding failed: {e}"))?;
    Ok(buf)
}

/// List available displays.
pub fn list_displays() -> anyhow::Result<Vec<DisplayInfo>> {
    let screens = screenshots::Screen::all()
        .map_err(|e| anyhow::anyhow!("Failed to enumerate screens: {e}"))?;

    Ok(screens.iter().map(|s| DisplayInfo {
        id: s.display_info.id,
        x: s.display_info.x,
        y: s.display_info.y,
        width: s.display_info.width,
        height: s.display_info.height,
        is_primary: s.display_info.is_primary,
        scale_factor: s.display_info.scale_factor,
    }).collect())
}

/// Result of a screenshot capture.
pub struct ScreenshotResult {
    /// Base64-encoded PNG data.
    pub base64_png: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Information about a display.
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub scale_factor: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_displays_works() {
        // This test requires a display, skip in headless CI
        match list_displays() {
            Ok(displays) => {
                assert!(!displays.is_empty(), "Should find at least one display");
                let primary = displays.iter().find(|d| d.is_primary);
                if let Some(p) = primary {
                    assert!(p.width > 0);
                    assert!(p.height > 0);
                }
            }
            Err(e) => {
                eprintln!("list_displays failed (headless?): {e}");
            }
        }
    }
}
