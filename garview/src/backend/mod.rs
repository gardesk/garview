mod comic;
mod epub;
mod image_backend;
mod pdf;
mod svg;

pub use comic::ComicBackend;
pub use epub::EpubBackend;
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

    /// Does this backend support text search?
    fn supports_search(&self) -> bool {
        false
    }

    /// Search for text on a page, returns rectangles (x1, y1, x2, y2) in page coordinates
    fn search_page(&self, _page: usize, _query: &str) -> Vec<(f64, f64, f64, f64)> {
        Vec::new()
    }

    /// Does this backend support text selection?
    fn supports_text_selection(&self) -> bool {
        false
    }

    /// Get text within a rectangle (x1, y1, x2, y2) in page coordinates
    fn get_text_for_area(&self, _page: usize, _area: (f64, f64, f64, f64)) -> Option<String> {
        None
    }

    /// Get selection region as a list of rectangles (for text-aware highlighting)
    /// Returns Vec<(x, y, width, height)> in image coordinates (Y=0 at top)
    fn get_selection_region(
        &self,
        _page: usize,
        _area: (f64, f64, f64, f64),
        _scale: f64,
    ) -> Vec<(i32, i32, i32, i32)> {
        Vec::new()
    }

    /// Does this backend have a table of contents?
    fn has_toc(&self) -> bool {
        false
    }

    /// Get table of contents entries (title, page, level)
    fn get_toc(&self) -> Vec<(String, usize, usize)> {
        Vec::new()
    }

    /// Does this backend support hyperlinks?
    fn supports_links(&self) -> bool {
        false
    }

    /// Get links on a page as (rect, destination) where destination is either
    /// a page number (internal link) or a URL string (external link)
    fn get_links(&self, _page: usize) -> Vec<((f64, f64, f64, f64), LinkDestination)> {
        Vec::new()
    }
}

/// Link destination type
#[derive(Debug, Clone)]
pub enum LinkDestination {
    /// Internal link to a page number
    Page(usize),
    /// External URI
    Uri(String),
    /// Named destination (not yet resolved)
    Named(String),
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

    // Comic archives
    if ext == "cbz" || ext == "cb7" || ext == "cbt" {
        return Some(Box::new(ComicBackend::new()));
    }

    // Ebooks
    if ext == "epub" {
        return Some(Box::new(EpubBackend::new()));
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

    // Comic archives
    if ext == "cbz" || ext == "cb7" || ext == "cbt" {
        return Some(Box::new(ComicBackend::new()));
    }

    // Ebooks (EpubBackend uses Cairo which is not Send)
    // Skip EPUB in sendable backend

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
