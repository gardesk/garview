use super::{ScrollState, ZoomState};
use crate::backend::{backend_for_path, sendable_backend_for_path, Backend, PageSize};
use anyhow::{anyhow, Result};
use gartk_render::Surface;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Loading state for async image loading
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadState {
    /// No image loaded
    Empty,
    /// Image is being decoded in background
    Loading,
    /// Image is ready to display
    Ready,
    /// Loading failed
    Failed,
}

/// Document view mode for multi-page documents
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DocumentViewMode {
    /// Single page at a time
    #[default]
    SinglePage,
    /// Continuous vertical scroll
    Continuous,
    /// Two pages side by side (book view)
    DualPage,
}

/// Result from background loading thread (requires Send)
struct LoadResult {
    backend: Box<dyn Backend + Send>,
    size: PageSize,
}

/// Result from synchronous loading (for non-Send backends like PDF)
struct SyncLoadResult {
    backend: Box<dyn Backend>,
    size: PageSize,
}

/// Gap between pages in continuous mode (pixels)
const PAGE_GAP: f64 = 20.0;

pub struct ImageViewer {
    /// Current backend
    backend: Option<Box<dyn Backend>>,
    /// Current file path
    current_path: Option<PathBuf>,
    /// Rendered surface (cached) - for single page mode
    surface: Option<Surface>,
    /// Surface scale (for cache invalidation)
    surface_scale: f64,
    /// Image dimensions
    image_size: Option<PageSize>,
    /// Zoom state
    pub zoom: ZoomState,
    /// Scroll state
    pub scroll: ScrollState,
    /// Rotation in degrees (0, 90, 180, 270)
    rotation: i32,
    /// Flip horizontal
    flip_h: bool,
    /// Flip vertical
    flip_v: bool,
    /// Animation state / current page
    current_frame: usize,
    last_frame_time: Instant,
    /// Directory listing for prev/next
    directory_files: Vec<PathBuf>,
    current_index: usize,
    /// Async loading state
    load_state: LoadState,
    /// Channel to receive loaded image from background thread
    load_receiver: Option<Receiver<Result<LoadResult>>>,
    /// Handle to background loading thread
    load_handle: Option<JoinHandle<()>>,
    /// Time loading started (for minimum display time)
    load_start: Option<Instant>,
    /// Document view mode (for multi-page documents)
    view_mode: DocumentViewMode,
    /// Page surfaces cache (for continuous mode)
    page_surfaces: std::collections::HashMap<usize, Surface>,
    /// Continuous scroll offset (total y offset from top of document)
    continuous_scroll_y: f64,
}

impl ImageViewer {
    pub fn new() -> Self {
        Self {
            backend: None,
            current_path: None,
            surface: None,
            surface_scale: 0.0,
            image_size: None,
            zoom: ZoomState::new(),
            scroll: ScrollState::new(),
            rotation: 0,
            flip_h: false,
            flip_v: false,
            current_frame: 0,
            last_frame_time: Instant::now(),
            directory_files: Vec::new(),
            current_index: 0,
            load_state: LoadState::Empty,
            load_receiver: None,
            load_handle: None,
            load_start: None,
            view_mode: DocumentViewMode::default(),
            page_surfaces: std::collections::HashMap::new(),
            continuous_scroll_y: 0.0,
        }
    }

    /// Start loading an image asynchronously (or synchronously for PDFs)
    pub fn load(&mut self, path: &Path) -> Result<()> {
        // Cancel any in-progress load
        self.cancel_load();

        // Scan directory for siblings (this is fast)
        self.scan_directory(path);

        // Set loading state
        self.load_state = LoadState::Loading;
        self.load_start = Some(Instant::now());
        self.current_path = Some(path.to_path_buf());

        // Clear current image
        self.backend = None;
        self.surface = None;
        self.image_size = None;
        self.page_surfaces.clear();
        self.continuous_scroll_y = 0.0;

        // Check if this is a PDF (load synchronously since poppler isn't Send)
        let is_pdf = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase() == "pdf")
            .unwrap_or(false);

        if is_pdf {
            // Load PDF synchronously
            match Self::load_sync(path) {
                Ok(result) => {
                    self.backend = Some(result.backend);
                    self.image_size = Some(result.size);
                    self.surface = None;
                    self.surface_scale = 0.0;
                    self.current_frame = 0;
                    self.last_frame_time = Instant::now();
                    self.rotation = 0;
                    self.flip_h = false;
                    self.flip_v = false;
                    self.scroll.reset();
                    self.zoom.zoom_fit();
                    self.load_state = LoadState::Ready;
                }
                Err(e) => {
                    tracing::error!("Failed to load PDF: {}", e);
                    self.load_state = LoadState::Failed;
                }
            }
            return Ok(());
        }

        // Spawn background thread to load image
        let path_owned = path.to_path_buf();
        let (tx, rx): (Sender<Result<LoadResult>>, Receiver<Result<LoadResult>>) = mpsc::channel();

        let handle = thread::spawn(move || {
            let result = Self::load_in_background(&path_owned);
            let _ = tx.send(result);
        });

        self.load_receiver = Some(rx);
        self.load_handle = Some(handle);

        Ok(())
    }

    /// Background loading function (runs in separate thread)
    /// Only used for Send backends (images, SVG)
    fn load_in_background(path: &Path) -> Result<LoadResult> {
        let mut backend =
            sendable_backend_for_path(path).ok_or_else(|| anyhow!("Unsupported file format"))?;

        backend.open(path)?;
        let size = backend.page_size(0)?;

        Ok(LoadResult { backend, size })
    }

    /// Synchronous loading function (for non-Send backends like PDF)
    fn load_sync(path: &Path) -> Result<SyncLoadResult> {
        let mut backend =
            backend_for_path(path).ok_or_else(|| anyhow!("Unsupported file format"))?;

        backend.open(path)?;
        let size = backend.page_size(0)?;

        Ok(SyncLoadResult { backend, size })
    }

    /// Cancel any in-progress load
    fn cancel_load(&mut self) {
        self.load_receiver = None;
        if let Some(handle) = self.load_handle.take() {
            // Don't wait for thread, just drop the handle
            drop(handle);
        }
    }

    /// Poll for load completion - call this every frame
    /// Returns true if state changed (needs redraw)
    pub fn poll_load(&mut self) -> bool {
        if self.load_state != LoadState::Loading {
            return false;
        }

        let receiver = match self.load_receiver.as_ref() {
            Some(rx) => rx,
            None => return false,
        };

        // Non-blocking check for result
        match receiver.try_recv() {
            Ok(Ok(result)) => {
                // Success! Apply the loaded image
                self.backend = Some(result.backend);
                self.image_size = Some(result.size);
                self.surface = None;
                self.surface_scale = 0.0;
                self.current_frame = 0;
                self.last_frame_time = Instant::now();
                self.rotation = 0;
                self.flip_h = false;
                self.flip_v = false;
                self.scroll.reset();
                self.zoom.zoom_fit();
                self.load_state = LoadState::Ready;
                self.load_receiver = None;
                self.load_handle = None;
                true
            }
            Ok(Err(e)) => {
                // Loading failed
                tracing::error!("Failed to load image: {}", e);
                self.load_state = LoadState::Failed;
                self.load_receiver = None;
                self.load_handle = None;
                true
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Still loading
                false
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Thread crashed or was cancelled
                self.load_state = LoadState::Failed;
                self.load_receiver = None;
                self.load_handle = None;
                true
            }
        }
    }

    /// Get the current loading state
    pub fn load_state(&self) -> LoadState {
        self.load_state
    }

    /// Get loading progress hint (time elapsed)
    pub fn load_elapsed(&self) -> Option<Duration> {
        self.load_start.map(|t| t.elapsed())
    }

    fn scan_directory(&mut self, path: &Path) {
        let parent = match path.parent() {
            Some(p) => p,
            None => return,
        };

        let entries = match std::fs::read_dir(parent) {
            Ok(e) => e,
            Err(_) => return,
        };

        let supported_exts = [
            "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "svg", "ico", "avif",
        ];

        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| supported_exts.contains(&e.to_lowercase().as_str()))
                    .unwrap_or(false)
            })
            .collect();

        files.sort();

        self.current_index = files.iter().position(|p| p == path).unwrap_or(0);
        self.directory_files = files;
    }

    #[allow(dead_code)]
    pub fn current_path(&self) -> Option<&Path> {
        self.current_path.as_deref()
    }

    #[allow(dead_code)]
    pub fn image_size(&self) -> Option<PageSize> {
        self.image_size
    }

    /// Get the effective image size after rotation
    pub fn effective_size(&self) -> Option<(f64, f64)> {
        self.image_size.map(|s| {
            if self.rotation == 90 || self.rotation == 270 {
                (s.height, s.width)
            } else {
                (s.width, s.height)
            }
        })
    }

    /// Zoom at a specific screen point, keeping that point stationary
    pub fn zoom_at_point(
        &mut self,
        factor: f64,
        screen_x: f64,
        screen_y: f64,
        viewport_width: f64,
        viewport_height: f64,
    ) {
        let Some((img_w, img_h)) = self.effective_size() else {
            return;
        };

        let old_zoom = self.zoom.level;
        let scaled_w = img_w * old_zoom;
        let scaled_h = img_h * old_zoom;

        // Calculate image coordinate under mouse
        // If image is centered (smaller than viewport), adjust for centering offset
        let img_x = if scaled_w < viewport_width {
            screen_x - (viewport_width - scaled_w) / 2.0
        } else {
            screen_x + self.scroll.offset_x
        };

        let img_y = if scaled_h < viewport_height {
            screen_y - (viewport_height - scaled_h) / 2.0
        } else {
            screen_y + self.scroll.offset_y
        };

        // Apply zoom
        self.zoom.zoom_at_point(factor, 0.0, 0.0);
        let new_zoom = self.zoom.level;

        // Calculate new scroll offset to keep the same image point under mouse
        let zoom_ratio = new_zoom / old_zoom;
        let new_img_x = img_x * zoom_ratio;
        let new_img_y = img_y * zoom_ratio;

        // New offset = new image coord - screen coord
        let new_scaled_w = img_w * new_zoom;
        let new_scaled_h = img_h * new_zoom;

        if new_scaled_w > viewport_width {
            self.scroll.offset_x = new_img_x - screen_x;
        }
        if new_scaled_h > viewport_height {
            self.scroll.offset_y = new_img_y - screen_y;
        }
    }

    pub fn is_animated(&self) -> bool {
        self.backend.as_ref().map(|b| b.is_animated()).unwrap_or(false)
    }

    #[allow(dead_code)]
    pub fn frame_count(&self) -> usize {
        self.backend.as_ref().map(|b| b.page_count()).unwrap_or(1)
    }

    /// Advance animation frame if needed, returns true if frame changed
    pub fn tick_animation(&mut self) -> bool {
        let backend = match self.backend.as_ref() {
            Some(b) if b.is_animated() => b,
            _ => return false,
        };

        let delay = backend
            .frame_delay(self.current_frame)
            .unwrap_or(Duration::from_millis(100));

        if self.last_frame_time.elapsed() >= delay {
            self.current_frame = (self.current_frame + 1) % backend.page_count();
            self.last_frame_time = Instant::now();
            self.surface = None; // Invalidate cache
            true
        } else {
            false
        }
    }

    /// Render to a surface at the given scale
    pub fn render(&mut self, scale: f64) -> Result<&Surface> {
        if self.load_state != LoadState::Ready {
            return Err(anyhow!("No image ready"));
        }

        // Always render at native resolution (1.0) - Cairo handles all scaling
        // This avoids expensive resize operations entirely
        // Quality is fine since at high zoom you're looking at individual pixels anyway
        let render_scale = 1.0;

        // Check if we need to re-render
        let needs_render = self.surface.is_none() || (self.surface_scale - render_scale).abs() > 0.001;

        if needs_render {
            tracing::debug!(
                "Re-rendering surface: scale={:.2}, render_scale={:.2}, cached_scale={:.2}",
                scale, render_scale, self.surface_scale
            );
            let backend = self.backend.as_mut().ok_or_else(|| anyhow!("No image loaded"))?;
            let page = backend.render_page(self.current_frame, render_scale)?;
            let surface = Surface::from_rgba(&page.data, page.width, page.height)?;

            self.surface = Some(surface);
            self.surface_scale = render_scale;
        }

        self.surface.as_ref().ok_or_else(|| anyhow!("No surface"))
    }

    /// Get the current render scale
    #[allow(dead_code)]
    pub fn current_scale(&self) -> f64 {
        self.surface_scale
    }

    // Navigation
    pub fn next_image(&mut self) -> Result<bool> {
        if self.directory_files.is_empty() {
            return Ok(false);
        }

        let next_index = (self.current_index + 1) % self.directory_files.len();
        if next_index != self.current_index {
            let path = self.directory_files[next_index].clone();
            self.current_index = next_index;
            self.load(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn prev_image(&mut self) -> Result<bool> {
        if self.directory_files.is_empty() {
            return Ok(false);
        }

        let prev_index = if self.current_index == 0 {
            self.directory_files.len() - 1
        } else {
            self.current_index - 1
        };

        if prev_index != self.current_index {
            let path = self.directory_files[prev_index].clone();
            self.current_index = prev_index;
            self.load(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    // Rotation
    pub fn rotate_cw(&mut self) {
        self.rotation = (self.rotation + 90) % 360;
        self.surface = None;
    }

    pub fn rotate_ccw(&mut self) {
        self.rotation = (self.rotation + 270) % 360;
        self.surface = None;
    }

    pub fn rotation(&self) -> i32 {
        self.rotation
    }

    // Flip
    pub fn flip_horizontal(&mut self) {
        self.flip_h = !self.flip_h;
        self.surface = None;
    }

    pub fn flip_vertical(&mut self) {
        self.flip_v = !self.flip_v;
        self.surface = None;
    }

    pub fn is_flipped_h(&self) -> bool {
        self.flip_h
    }

    pub fn is_flipped_v(&self) -> bool {
        self.flip_v
    }

    // Info
    pub fn file_info(&self) -> Option<FileInfo> {
        let path = self.current_path.as_ref()?;
        let size = self.image_size?;

        let metadata = std::fs::metadata(path).ok()?;
        let file_size = metadata.len();

        let format = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_uppercase())
            .unwrap_or_else(|| "Unknown".to_string());

        Some(FileInfo {
            filename: path.file_name()?.to_string_lossy().to_string(),
            width: size.width as u32,
            height: size.height as u32,
            file_size,
            format,
        })
    }

    pub fn directory_position(&self) -> Option<(usize, usize)> {
        if self.directory_files.is_empty() {
            None
        } else {
            Some((self.current_index + 1, self.directory_files.len()))
        }
    }

    // Page navigation (for multi-page documents like PDF)
    /// Get total page count
    pub fn page_count(&self) -> usize {
        self.backend.as_ref().map(|b| b.page_count()).unwrap_or(1)
    }

    /// Get current page (0-indexed)
    pub fn current_page(&self) -> usize {
        self.current_frame
    }

    /// Check if document has multiple pages
    pub fn is_multipage(&self) -> bool {
        self.page_count() > 1 && !self.is_animated()
    }

    /// Go to specific page (0-indexed)
    pub fn goto_page(&mut self, page: usize) -> bool {
        let count = self.page_count();
        if page >= count {
            return false;
        }
        if page != self.current_frame {
            self.current_frame = page;
            self.surface = None; // Invalidate cache
            // Update image size for the new page
            if let Some(backend) = self.backend.as_ref() {
                if let Ok(size) = backend.page_size(page) {
                    self.image_size = Some(size);
                }
            }
            true
        } else {
            false
        }
    }

    /// Go to next page
    pub fn next_page(&mut self) -> bool {
        let count = self.page_count();
        if count <= 1 || self.is_animated() {
            return false;
        }
        if self.current_frame + 1 < count {
            self.goto_page(self.current_frame + 1)
        } else {
            false
        }
    }

    /// Go to previous page
    pub fn prev_page(&mut self) -> bool {
        if self.page_count() <= 1 || self.is_animated() {
            return false;
        }
        if self.current_frame > 0 {
            self.goto_page(self.current_frame - 1)
        } else {
            false
        }
    }

    /// Go to first page
    pub fn first_page(&mut self) -> bool {
        if self.page_count() <= 1 || self.is_animated() {
            return false;
        }
        self.goto_page(0)
    }

    /// Go to last page
    pub fn last_page(&mut self) -> bool {
        let count = self.page_count();
        if count <= 1 || self.is_animated() {
            return false;
        }
        self.goto_page(count - 1)
    }

    /// Get page position for status bar display
    pub fn page_position(&self) -> Option<(usize, usize)> {
        if self.is_multipage() {
            Some((self.current_frame + 1, self.page_count()))
        } else {
            None
        }
    }

    /// Get page size for a specific page
    pub fn page_size_for(&self, page: usize) -> Option<crate::backend::PageSize> {
        self.backend.as_ref()?.page_size(page).ok()
    }

    // View mode methods (for multi-page documents)

    /// Get current view mode
    pub fn view_mode(&self) -> DocumentViewMode {
        self.view_mode
    }

    /// Set view mode
    pub fn set_view_mode(&mut self, mode: DocumentViewMode) {
        if self.view_mode != mode {
            self.view_mode = mode;
            self.page_surfaces.clear();
            // When switching to continuous mode, scroll to current page
            if mode == DocumentViewMode::Continuous {
                self.scroll_to_page_continuous(self.current_frame);
            }
        }
    }

    /// Cycle through view modes
    pub fn cycle_view_mode(&mut self) {
        if !self.is_multipage() {
            return;
        }
        let new_mode = match self.view_mode {
            DocumentViewMode::SinglePage => DocumentViewMode::Continuous,
            DocumentViewMode::Continuous => DocumentViewMode::DualPage,
            DocumentViewMode::DualPage => DocumentViewMode::SinglePage,
        };
        self.set_view_mode(new_mode);
    }

    /// Calculate total document height for continuous scroll mode
    pub fn total_document_height(&self, zoom: f64) -> f64 {
        let count = self.page_count();
        if count == 0 {
            return 0.0;
        }

        let backend = match self.backend.as_ref() {
            Some(b) => b,
            None => return 0.0,
        };

        let mut total = 0.0;
        for i in 0..count {
            if let Ok(size) = backend.page_size(i) {
                total += size.height * zoom;
            }
            if i < count - 1 {
                total += PAGE_GAP;
            }
        }
        total
    }

    /// Get page Y position in continuous scroll mode
    fn page_y_position(&self, page: usize, zoom: f64) -> f64 {
        let backend = match self.backend.as_ref() {
            Some(b) => b,
            None => return 0.0,
        };

        let mut y = 0.0;
        for i in 0..page {
            if let Ok(size) = backend.page_size(i) {
                y += size.height * zoom + PAGE_GAP;
            }
        }
        y
    }

    /// Scroll to a page in continuous mode
    fn scroll_to_page_continuous(&mut self, page: usize) {
        self.continuous_scroll_y = self.page_y_position(page, self.zoom.level);
    }

    /// Get which page is visible at current scroll position (continuous mode)
    fn page_at_scroll(&self, zoom: f64) -> usize {
        let backend = match self.backend.as_ref() {
            Some(b) => b,
            None => return 0,
        };

        let mut y = 0.0;
        for i in 0..self.page_count() {
            if let Ok(size) = backend.page_size(i) {
                let page_height = size.height * zoom;
                if self.continuous_scroll_y < y + page_height {
                    return i;
                }
                y += page_height + PAGE_GAP;
            }
        }
        self.page_count().saturating_sub(1)
    }

    /// Get visible pages in continuous mode (returns (start_page, end_page))
    pub fn visible_pages(&self, viewport_height: f64, zoom: f64) -> (usize, usize) {
        let backend = match self.backend.as_ref() {
            Some(b) => b,
            None => return (0, 0),
        };

        let mut y = 0.0;
        let mut start_page = None;
        let mut end_page = 0;

        for i in 0..self.page_count() {
            if let Ok(size) = backend.page_size(i) {
                let page_height = size.height * zoom;
                let page_top = y;
                let page_bottom = y + page_height;

                // Check if page is visible
                if page_bottom > self.continuous_scroll_y
                    && page_top < self.continuous_scroll_y + viewport_height
                {
                    if start_page.is_none() {
                        start_page = Some(i);
                    }
                    end_page = i;
                }

                y += page_height + PAGE_GAP;

                // Stop if we're past the viewport
                if page_top > self.continuous_scroll_y + viewport_height {
                    break;
                }
            }
        }

        (start_page.unwrap_or(0), end_page)
    }

    /// Scroll in continuous mode
    pub fn scroll_continuous(&mut self, delta_y: f64, viewport_height: f64) {
        let total = self.total_document_height(self.zoom.level);
        let max_scroll = (total - viewport_height).max(0.0);

        self.continuous_scroll_y = (self.continuous_scroll_y + delta_y).clamp(0.0, max_scroll);

        // Update current page based on scroll position
        self.current_frame = self.page_at_scroll(self.zoom.level);
    }

    /// Get continuous scroll offset
    pub fn continuous_scroll_y(&self) -> f64 {
        self.continuous_scroll_y
    }

    /// Render a specific page (used for continuous mode)
    pub fn render_page_surface(&mut self, page: usize) -> Result<&Surface> {
        if self.load_state != LoadState::Ready {
            return Err(anyhow!("No image ready"));
        }

        // Check cache
        if self.page_surfaces.contains_key(&page) {
            return self.page_surfaces.get(&page).ok_or_else(|| anyhow!("Cache error"));
        }

        // Render the page
        let backend = self.backend.as_mut().ok_or_else(|| anyhow!("No backend"))?;
        let rendered = backend.render_page(page, 1.0)?;
        let surface = Surface::from_rgba(&rendered.data, rendered.width, rendered.height)?;

        self.page_surfaces.insert(page, surface);
        self.page_surfaces.get(&page).ok_or_else(|| anyhow!("Cache error"))
    }
}

impl Drop for ImageViewer {
    fn drop(&mut self) {
        self.cancel_load();
    }
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub file_size: u64,
    pub format: String,
}

impl FileInfo {
    pub fn format_size(&self) -> String {
        if self.file_size < 1024 {
            format!("{} B", self.file_size)
        } else if self.file_size < 1024 * 1024 {
            format!("{:.1} KB", self.file_size as f64 / 1024.0)
        } else {
            format!("{:.1} MB", self.file_size as f64 / (1024.0 * 1024.0))
        }
    }
}
