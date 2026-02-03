use crate::cache::{ThumbnailCache, THUMBNAIL_SIZE};
use anyhow::Result;
use gartk_render::{Renderer, Surface};
use std::path::{Path, PathBuf};

/// Supported image extensions for gallery
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "svg", "ico", "avif",
];

/// Sort order for gallery
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SortOrder {
    #[default]
    Name,
    NameDesc,
    Date,
    DateDesc,
    Size,
    SizeDesc,
}

/// Gallery view state
pub struct GalleryView {
    /// Directory being viewed
    directory: Option<PathBuf>,
    /// List of image files
    files: Vec<FileEntry>,
    /// Thumbnail cache
    cache: ThumbnailCache,
    /// Current selection index
    selection: usize,
    /// Scroll offset (in pixels)
    scroll_offset: f64,
    /// Grid columns (calculated based on viewport width)
    columns: usize,
    /// Thumbnail size including padding
    cell_size: u32,
    /// Current sort order
    sort_order: SortOrder,
    /// Thumbnail surfaces (rendered from cache data)
    surfaces: std::collections::HashMap<PathBuf, Surface>,
}

/// File entry with metadata
#[derive(Debug, Clone)]
struct FileEntry {
    path: PathBuf,
    name: String,
    size: u64,
    modified: std::time::SystemTime,
}

impl GalleryView {
    /// Create a new gallery view
    pub fn new() -> Result<Self> {
        let cache = ThumbnailCache::new(THUMBNAIL_SIZE)?;

        Ok(Self {
            directory: None,
            files: Vec::new(),
            cache,
            selection: 0,
            scroll_offset: 0.0,
            columns: 4,
            cell_size: THUMBNAIL_SIZE + 16, // thumbnail + padding
            sort_order: SortOrder::default(),
            surfaces: std::collections::HashMap::new(),
        })
    }

    /// Open a directory
    pub fn open(&mut self, path: &Path) -> Result<()> {
        if !path.is_dir() {
            return Err(anyhow::anyhow!("Not a directory"));
        }

        self.directory = Some(path.to_path_buf());
        self.files.clear();
        self.surfaces.clear();
        self.selection = 0;
        self.scroll_offset = 0.0;

        // Scan directory
        self.scan_directory(path)?;

        // Apply current sort
        self.apply_sort();

        // Request thumbnails for visible items
        self.request_visible_thumbnails(0, 800); // Initial viewport

        Ok(())
    }

    /// Scan directory for image files
    fn scan_directory(&mut self, path: &Path) -> Result<()> {
        let entries = std::fs::read_dir(path)?;

        for entry in entries.flatten() {
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());

            if let Some(ext) = ext {
                if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
                    if let Ok(metadata) = entry.metadata() {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();

                        self.files.push(FileEntry {
                            path,
                            name,
                            size: metadata.len(),
                            modified: metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                        });
                    }
                }
            }
        }

        Ok(())
    }

    /// Apply current sort order
    fn apply_sort(&mut self) {
        match self.sort_order {
            SortOrder::Name => self.files.sort_by(|a, b| a.name.cmp(&b.name)),
            SortOrder::NameDesc => self.files.sort_by(|a, b| b.name.cmp(&a.name)),
            SortOrder::Date => self.files.sort_by(|a, b| a.modified.cmp(&b.modified)),
            SortOrder::DateDesc => self.files.sort_by(|a, b| b.modified.cmp(&a.modified)),
            SortOrder::Size => self.files.sort_by(|a, b| a.size.cmp(&b.size)),
            SortOrder::SizeDesc => self.files.sort_by(|a, b| b.size.cmp(&a.size)),
        }
    }

    /// Set sort order
    pub fn set_sort(&mut self, order: SortOrder) {
        if self.sort_order != order {
            self.sort_order = order;
            self.apply_sort();
        }
    }

    /// Get current sort order
    #[allow(dead_code)]
    pub fn sort_order(&self) -> SortOrder {
        self.sort_order
    }

    /// Update grid layout based on viewport width
    pub fn update_layout(&mut self, viewport_width: u32) {
        self.columns = ((viewport_width as usize) / (self.cell_size as usize)).max(1);
    }

    /// Request thumbnails for visible items
    fn request_visible_thumbnails(&mut self, _viewport_y: u32, viewport_height: u32) {
        let start_row = (self.scroll_offset as u32 / self.cell_size) as usize;
        let visible_rows = (viewport_height / self.cell_size) as usize + 2;
        let start_idx = start_row * self.columns;
        let end_idx = ((start_row + visible_rows) * self.columns).min(self.files.len());

        for i in start_idx..end_idx {
            let path = &self.files[i].path;

            // Try to load from disk cache first
            if !self.cache.get(path).is_some() && !self.cache.is_pending(path) {
                if self.cache.is_cached(path) {
                    let _ = self.cache.load_cached(path);
                } else {
                    self.cache.request(path);
                }
            }
        }
    }

    /// Poll for thumbnail updates, returns true if any completed
    pub fn poll_thumbnails(&mut self) -> bool {
        let completed = self.cache.poll();

        // Create surfaces for completed thumbnails
        for path in &completed {
            if let Some(thumb) = self.cache.get(path) {
                if let Ok(surface) = Surface::from_rgba(&thumb.data, thumb.width, thumb.height) {
                    self.surfaces.insert(path.clone(), surface);
                }
            }
        }

        !completed.is_empty()
    }

    /// Render the gallery view
    pub fn render(&mut self, renderer: &Renderer, viewport_height: u32) -> Result<()> {
        let size = renderer.size();
        self.update_layout(size.width);

        // Request thumbnails for visible area
        self.request_visible_thumbnails(0, viewport_height);

        let ctx = renderer.context()?;
        let cell = self.cell_size as f64;
        let thumb_size = THUMBNAIL_SIZE as f64;
        let padding = (cell - thumb_size) / 2.0;

        // Calculate visible range
        let start_row = (self.scroll_offset / cell) as usize;
        let visible_rows = (viewport_height as f64 / cell).ceil() as usize + 1;

        for row in start_row..(start_row + visible_rows) {
            for col in 0..self.columns {
                let idx = row * self.columns + col;
                if idx >= self.files.len() {
                    break;
                }

                let file = &self.files[idx];
                let x = col as f64 * cell + padding;
                let y = row as f64 * cell - self.scroll_offset + padding;

                // Skip if off-screen
                if y + thumb_size < 0.0 || y > viewport_height as f64 {
                    continue;
                }

                // Draw selection highlight
                if idx == self.selection {
                    ctx.set_source_rgb(0.3, 0.5, 0.8);
                    ctx.rectangle(x - 4.0, y - 4.0, thumb_size + 8.0, thumb_size + 8.0);
                    ctx.fill()?;
                }

                // Draw thumbnail or placeholder
                if let Some(surface) = self.surfaces.get(&file.path) {
                    // Center thumbnail in cell
                    let thumb_w = surface.width() as f64;
                    let thumb_h = surface.height() as f64;
                    let offset_x = (thumb_size - thumb_w) / 2.0;
                    let offset_y = (thumb_size - thumb_h) / 2.0;

                    ctx.set_source_surface(surface.cairo_surface(), x + offset_x, y + offset_y)?;
                    ctx.paint()?;
                } else {
                    // Placeholder
                    ctx.set_source_rgb(0.2, 0.2, 0.25);
                    ctx.rectangle(x, y, thumb_size, thumb_size);
                    ctx.fill()?;

                    // Loading indicator if pending
                    if self.cache.is_pending(&file.path) {
                        ctx.set_source_rgb(0.4, 0.4, 0.45);
                        ctx.arc(
                            x + thumb_size / 2.0,
                            y + thumb_size / 2.0,
                            20.0,
                            0.0,
                            std::f64::consts::PI * 1.5,
                        );
                        ctx.stroke()?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Navigate selection
    pub fn select_next(&mut self) {
        if self.selection < self.files.len().saturating_sub(1) {
            self.selection += 1;
            self.ensure_visible();
        }
    }

    pub fn select_prev(&mut self) {
        if self.selection > 0 {
            self.selection -= 1;
            self.ensure_visible();
        }
    }

    pub fn select_down(&mut self) {
        let new_sel = self.selection + self.columns;
        if new_sel < self.files.len() {
            self.selection = new_sel;
            self.ensure_visible();
        }
    }

    pub fn select_up(&mut self) {
        if self.selection >= self.columns {
            self.selection -= self.columns;
            self.ensure_visible();
        }
    }

    /// Ensure selected item is visible
    fn ensure_visible(&mut self) {
        // This will be called after viewport_height is known
        // For now, simple implementation
        let row = self.selection / self.columns;
        let cell = self.cell_size as f64;
        let item_y = row as f64 * cell;

        if item_y < self.scroll_offset {
            self.scroll_offset = item_y;
        }
        // Note: need viewport_height to check bottom visibility
    }

    /// Scroll by delta
    pub fn scroll(&mut self, delta: f64, viewport_height: u32) {
        self.scroll_offset = (self.scroll_offset + delta).max(0.0);

        // Clamp to content height
        let total_rows = (self.files.len() + self.columns - 1) / self.columns;
        let content_height = total_rows as f64 * self.cell_size as f64;
        let max_scroll = (content_height - viewport_height as f64).max(0.0);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }

    /// Get selected file path
    pub fn selected_path(&self) -> Option<&Path> {
        self.files.get(self.selection).map(|f| f.path.as_path())
    }

    /// Get file count
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Get current selection index
    pub fn selection_index(&self) -> usize {
        self.selection
    }

    /// Check if gallery is empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Get directory path
    #[allow(dead_code)]
    pub fn directory(&self) -> Option<&Path> {
        self.directory.as_deref()
    }
}
