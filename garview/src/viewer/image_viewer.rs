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

pub struct ImageViewer {
    /// Current backend
    backend: Option<Box<dyn Backend>>,
    /// Current file path
    current_path: Option<PathBuf>,
    /// Rendered surface (cached)
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
    /// Animation state
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
