//! Image input utilities — read, validate, and encode image files for the Messages API.
//!
//! Supports PNG, JPEG, GIF, and WebP. Images are read from disk or fetched from URLs,
//! validated by magic bytes, and base64-encoded for inclusion in
//! user messages as `ContentBlock::Image`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use base64::Engine;

use crate::message::{ContentBlock, ImageSource};

/// Maximum image file size (20 MB, matching Claude API limits).
pub const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024;

/// Supported image MIME types.
const SUPPORTED_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Detect MIME type from file extension.
fn mime_from_extension(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

/// Detect MIME type from file magic bytes (first 12 bytes).
fn mime_from_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 4 {
        return None;
    }
    // PNG: 89 50 4E 47
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return Some("image/png");
    }
    // JPEG: FF D8 FF
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    // GIF: GIF87a or GIF89a
    if bytes.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    // WebP: RIFF....WEBP
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Read an image file and return a `ContentBlock::Image` with base64-encoded data.
///
/// Validates: file exists, size ≤ 20 MB, supported image type (by extension + magic bytes).
pub fn read_image_file(path: &Path) -> Result<ContentBlock> {
    // Check file exists
    if !path.exists() {
        bail!("Image file not found: {}", path.display());
    }

    // Check file size
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("Cannot read image file: {}", path.display()))?;
    if metadata.len() > MAX_IMAGE_SIZE {
        bail!(
            "Image file too large: {} bytes (max {} MB)",
            metadata.len(),
            MAX_IMAGE_SIZE / (1024 * 1024)
        );
    }
    if metadata.len() == 0 {
        bail!("Image file is empty: {}", path.display());
    }

    // Read file
    let data = std::fs::read(path)
        .with_context(|| format!("Failed to read image file: {}", path.display()))?;

    // Detect MIME type — prefer magic bytes, fallback to extension
    let mime_magic = mime_from_magic(&data);
    let mime_ext = mime_from_extension(path);
    let media_type = mime_magic.or(mime_ext).ok_or_else(|| {
        anyhow::anyhow!(
            "Unsupported image type: {}. Supported: PNG, JPEG, GIF, WebP",
            path.display()
        )
    })?;

    // Validate that it's a supported type
    if !SUPPORTED_TYPES.contains(&media_type) {
        bail!("Unsupported image type: {media_type}. Supported: PNG, JPEG, GIF, WebP");
    }

    // Base64 encode
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    Ok(ContentBlock::Image {
        source: ImageSource {
            media_type: media_type.to_string(),
            data: encoded,
        },
    })
}

/// Parse `@path` references from user input, returning (cleaned text, image blocks, url refs).
///
/// Lines starting with `@` followed by a file path are treated as image attachments.
/// Lines starting with `@http://` or `@https://` are treated as URL image references
/// (returned separately for async fetching in the caller).
///
/// Returns: `(cleaned_text, file_image_blocks, pending_urls)`
pub fn extract_image_refs(input: &str) -> (String, Vec<ContentBlock>, Vec<String>) {
    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    let mut urls = Vec::new();

    for line in input.lines() {
        let trimmed = line.trim();
        // Check for @path pattern — must be a plausible image file path or URL
        if let Some(path_str) = trimmed.strip_prefix('@') {
            let path_str = path_str.trim();
            if !path_str.is_empty() {
                // URL reference: @http:// or @https://
                if path_str.starts_with("http://") || path_str.starts_with("https://") {
                    urls.push(path_str.to_string());
                    continue;
                }
                // File reference
                let path = Path::new(path_str);
                if is_image_extension(path) {
                    match read_image_file(path) {
                        Ok(block) => {
                            images.push(block);
                            continue; // don't include this line in text
                        }
                        Err(e) => {
                            // Keep the line as text and add an error note
                            text_parts.push(format!("[Image error: {e}]"));
                            continue;
                        }
                    }
                }
            }
        }
        text_parts.push(line.to_string());
    }

    let text = text_parts.join("\n").trim().to_string();
    (text, images, urls)
}

/// Check if a path has a recognized image extension.
pub fn is_image_extension(path: &Path) -> bool {
    mime_from_extension(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn mime_from_extension_known() {
        assert_eq!(mime_from_extension(Path::new("photo.png")), Some("image/png"));
        assert_eq!(mime_from_extension(Path::new("photo.jpg")), Some("image/jpeg"));
        assert_eq!(mime_from_extension(Path::new("photo.jpeg")), Some("image/jpeg"));
        assert_eq!(mime_from_extension(Path::new("anim.gif")), Some("image/gif"));
        assert_eq!(mime_from_extension(Path::new("pic.webp")), Some("image/webp"));
        assert_eq!(mime_from_extension(Path::new("PHOTO.PNG")), Some("image/png"));
    }

    #[test]
    fn mime_from_extension_unknown() {
        assert_eq!(mime_from_extension(Path::new("doc.txt")), None);
        assert_eq!(mime_from_extension(Path::new("file")), None);
        assert_eq!(mime_from_extension(Path::new("data.bmp")), None);
    }

    #[test]
    fn mime_from_magic_png() {
        let bytes = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(mime_from_magic(&bytes), Some("image/png"));
    }

    #[test]
    fn mime_from_magic_jpeg() {
        let bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        assert_eq!(mime_from_magic(&bytes), Some("image/jpeg"));
    }

    #[test]
    fn mime_from_magic_gif() {
        assert_eq!(mime_from_magic(b"GIF89a"), Some("image/gif"));
        assert_eq!(mime_from_magic(b"GIF87a"), Some("image/gif"));
    }

    #[test]
    fn mime_from_magic_webp() {
        let bytes = b"RIFF\x00\x00\x00\x00WEBP";
        assert_eq!(mime_from_magic(bytes), Some("image/webp"));
    }

    #[test]
    fn mime_from_magic_unknown() {
        assert_eq!(mime_from_magic(&[0x00, 0x01, 0x02, 0x03]), None);
        assert_eq!(mime_from_magic(&[0x00]), None);
    }

    #[test]
    fn read_image_file_png() {
        // Create a minimal valid PNG
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        let mut f = std::fs::File::create(&path).unwrap();
        // PNG header + minimal IHDR (not a real renderable image, but valid magic)
        f.write_all(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00]).unwrap();
        drop(f);

        let block = read_image_file(&path).unwrap();
        match &block {
            ContentBlock::Image { source } => {
                assert_eq!(source.media_type, "image/png");
                assert!(!source.data.is_empty());
            }
            _ => panic!("Expected Image block"),
        }
    }

    #[test]
    fn read_image_file_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jpg");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        drop(f);

        let block = read_image_file(&path).unwrap();
        match &block {
            ContentBlock::Image { source } => {
                assert_eq!(source.media_type, "image/jpeg");
            }
            _ => panic!("Expected Image block"),
        }
    }

    #[test]
    fn read_image_not_found() {
        let err = read_image_file(Path::new("/nonexistent/image.png")).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn read_image_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.png");
        std::fs::File::create(&path).unwrap();

        let err = read_image_file(&path).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn read_image_unsupported_type() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bmp");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"BM\x00\x00\x00\x00").unwrap();
        drop(f);

        let err = read_image_file(&path).unwrap_err();
        assert!(err.to_string().contains("Unsupported"));
    }

    #[test]
    fn is_image_extension_check() {
        assert!(is_image_extension(Path::new("photo.png")));
        assert!(is_image_extension(Path::new("photo.JPG")));
        assert!(is_image_extension(Path::new("anim.gif")));
        assert!(is_image_extension(Path::new("pic.webp")));
        assert!(!is_image_extension(Path::new("doc.txt")));
        assert!(!is_image_extension(Path::new("file")));
    }

    #[test]
    fn extract_image_refs_no_images() {
        let (text, images, urls) = extract_image_refs("Hello, how are you?");
        assert_eq!(text, "Hello, how are you?");
        assert!(images.is_empty());
        assert!(urls.is_empty());
    }

    #[test]
    fn extract_image_refs_with_at_but_not_image() {
        let (text, images, urls) = extract_image_refs("@mention someone");
        assert_eq!(text, "@mention someone");
        assert!(images.is_empty());
        assert!(urls.is_empty());
    }

    #[test]
    fn extract_image_refs_file_not_found() {
        let (text, images, urls) = extract_image_refs("@/nonexistent/photo.png\nHello");
        assert!(text.contains("Image error"));
        assert!(text.contains("Hello"));
        assert!(images.is_empty());
        assert!(urls.is_empty());
    }

    #[test]
    fn extract_image_refs_with_valid_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00]).unwrap();
        drop(f);

        let input = format!("describe this image\n@{}", path.display());
        let (text, images, urls) = extract_image_refs(&input);
        assert_eq!(text, "describe this image");
        assert_eq!(images.len(), 1);
        assert!(urls.is_empty());
        match &images[0] {
            ContentBlock::Image { source } => {
                assert_eq!(source.media_type, "image/png");
            }
            _ => panic!("Expected Image block"),
        }
    }

    #[test]
    fn extract_image_refs_url() {
        let (text, images, urls) = extract_image_refs("check this:\n@https://example.com/photo.png");
        assert_eq!(text, "check this:");
        assert!(images.is_empty());
        assert_eq!(urls, vec!["https://example.com/photo.png"]);
    }

    #[test]
    fn extract_image_refs_mixed_url_and_file() {
        let (text, images, urls) =
            extract_image_refs("hello\n@https://example.com/img.jpg\n@/nonexistent.png\nworld");
        assert!(text.contains("hello"));
        assert!(text.contains("world"));
        assert_eq!(urls, vec!["https://example.com/img.jpg"]);
        // /nonexistent.png should produce an error in text (file not found)
        assert!(text.contains("Image error") || images.is_empty());
    }

    #[test]
    fn image_source_serde_roundtrip() {
        let source = ImageSource {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        };
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["media_type"], "image/png");
        assert_eq!(json["data"], "iVBORw0KGgo=");
        let back: ImageSource = serde_json::from_value(json).unwrap();
        assert_eq!(back.media_type, "image/png");
    }

    #[test]
    fn content_block_image_serde_roundtrip() {
        let block = ContentBlock::Image {
            source: ImageSource {
                media_type: "image/jpeg".into(),
                data: "/9j/4AAQ".into(),
            },
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["source"]["media_type"], "image/jpeg");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ContentBlock::Image { source } if source.media_type == "image/jpeg"));
    }
}
