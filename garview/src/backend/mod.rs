mod image_backend;
mod pdf;
mod svg;

pub use image_backend::ImageBackend;
pub use pdf::PdfBackend;
pub use svg::SvgBackend;

use anyhow::Result;
use std::path::Path;
use std::time::Duration;

/// Rendered page/frame data
#[allow(dead_code)]
pub struct RenderedPage {
    /// RGBA pixel data
    pub data: Vec<u8>,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Page/frame index (for multi-page documents)
    pub index: usize,
}

/// Page dimensions
#[derive(Debug, Clone, Copy)]
pub struct PageSize {
    pub width: f64,
    pub height: f64,
}

/// Backend trait for format-specific rendering
#[allow(dead_code)]
pub trait Backend {
    /// Get the format name (used for status bar display)
    fn format_name(&self) -> &'static str;

    /// Get supported extensions (used for format detection)
    fn extensions(&self) -> &'static [&'static str];

    /// Open a file
    fn open(&mut self, path: &Path) -> Result<()>;

    /// Close the current file
    fn close(&mut self);

    /// Is a file currently open?
    fn is_open(&self) -> bool;

    /// Get total number of pages/frames
    fn page_count(&self) -> usize;

    /// Get size of a specific page
    fn page_size(&self, page: usize) -> Result<PageSize>;

    /// Render a page at given scale
    fn render_page(&mut self, page: usize, scale: f64) -> Result<RenderedPage>;

    /// Is this format animated?
    fn is_animated(&self) -> bool {
        false
    }

    /// Get frame delay (for animated formats)
    fn frame_delay(&self, _frame: usize) -> Option<Duration> {
        None
    }
}

/// Detect backend for a file based on extension
pub fn backend_for_path(path: &Path) -> Option<Box<dyn Backend>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())?;

    // PDF
    if ext == "pdf" {
        return Some(Box::new(PdfBackend::new()));
    }

    // SVG
    if ext == "svg" || ext == "svgz" {
        return Some(Box::new(SvgBackend::new()));
    }

    // Images
    let image_exts = [
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "ico", "avif", "qoi", "ppm",
        "pgm", "pbm", "tga", "dds", "exr", "ff", "apng",
    ];

    if image_exts.contains(&ext.as_str()) {
        return Some(Box::new(ImageBackend::new()));
    }

    None
}

/// Detect backend for a file, returning only Send-safe backends
/// Used for async loading in background threads
pub fn sendable_backend_for_path(path: &Path) -> Option<Box<dyn Backend + Send>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())?;

    // PDF is not Send - skip it here
    if ext == "pdf" {
        return None;
    }

    // SVG
    if ext == "svg" || ext == "svgz" {
        return Some(Box::new(SvgBackend::new()));
    }

    // Images
    let image_exts = [
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "ico", "avif", "qoi", "ppm",
        "pgm", "pbm", "tga", "dds", "exr", "ff", "apng",
    ];

    if image_exts.contains(&ext.as_str()) {
        return Some(Box::new(ImageBackend::new()));
    }

    None
}
