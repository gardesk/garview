use super::{ScrollState, ZoomState};
use crate::backend::{backend_for_path, Backend, PageSize};
use anyhow::{anyhow, Result};
use gartk_render::Surface;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Render quality stage
#[derive(Debug, Clone, Copy, PartialEq)]
enum RenderStage {
    /// Fast low-res preview
    Preview,
    /// Full quality final render
    Final,
}

/// Preview scale factor (renders at 1/4 size for speed)
const PREVIEW_SCALE: f64 = 0.25;

pub struct ImageViewer {
    /// Current backend
    backend: Option<Box<dyn Backend>>,
    /// Current file path
    current_path: Option<PathBuf>,
    /// Rendered surface (cached)
    surface: Option<Surface>,
    /// Surface scale (for cache invalidation)
    surface_scale: f64,
    /// Current render stage
    render_stage: RenderStage,
    /// Target scale for final render
    target_scale: f64,
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
}

impl ImageViewer {
    pub fn new() -> Self {
        Self {
            backend: None,
            current_path: None,
            surface: None,
            surface_scale: 0.0,
            render_stage: RenderStage::Preview,
            target_scale: 1.0,
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
        }
    }

    pub fn load(&mut self, path: &Path) -> Result<()> {
        // Get backend for this file type
        let mut backend =
            backend_for_path(path).ok_or_else(|| anyhow!("Unsupported file format"))?;

        backend.open(path)?;

        let size = backend.page_size(0)?;

        // Scan directory for siblings
        self.scan_directory(path);

        self.backend = Some(backend);
        self.current_path = Some(path.to_path_buf());
        self.image_size = Some(size);
        self.surface = None;
        self.surface_scale = 0.0;
        self.render_stage = RenderStage::Preview;
        self.target_scale = 1.0;
        self.current_frame = 0;
        self.last_frame_time = Instant::now();
        self.rotation = 0;
        self.flip_h = false;
        self.flip_v = false;
        self.scroll.reset();
        self.zoom.zoom_fit();

        Ok(())
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

    pub fn current_path(&self) -> Option<&Path> {
        self.current_path.as_deref()
    }

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

    pub fn is_animated(&self) -> bool {
        self.backend.as_ref().map(|b| b.is_animated()).unwrap_or(false)
    }

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
            self.render_stage = RenderStage::Preview;
            true
        } else {
            false
        }
    }

    /// Check if we need to upgrade from preview to final quality
    pub fn needs_quality_upgrade(&self) -> bool {
        self.render_stage == RenderStage::Preview && self.surface.is_some()
    }

    /// Render to a surface at the given scale
    /// Uses two-stage rendering: fast preview first, then final quality
    pub fn render(&mut self, scale: f64) -> Result<&Surface> {
        self.target_scale = scale;

        // Check if scale changed significantly - reset to preview
        if self.surface.is_some() && (self.surface_scale - scale).abs() > 0.1 {
            self.surface = None;
            self.render_stage = RenderStage::Preview;
        }

        let needs_render = self.surface.is_none();
        let needs_upgrade = self.render_stage == RenderStage::Preview
            && self.surface.is_some()
            && (self.surface_scale - scale).abs() > 0.001;

        if needs_render {
            // First render: quick preview at reduced scale
            let preview_scale = (scale * PREVIEW_SCALE).max(0.1);
            let backend = self.backend.as_mut().ok_or_else(|| anyhow!("No image loaded"))?;
            let page = backend.render_page(self.current_frame, preview_scale)?;
            let surface = Surface::from_rgba(&page.data, page.width, page.height)?;

            self.surface = Some(surface);
            self.surface_scale = preview_scale;
            self.render_stage = RenderStage::Preview;
        } else if needs_upgrade {
            // Upgrade to final quality
            let backend = self.backend.as_mut().ok_or_else(|| anyhow!("No image loaded"))?;
            let page = backend.render_page(self.current_frame, scale)?;
            let surface = Surface::from_rgba(&page.data, page.width, page.height)?;

            self.surface = Some(surface);
            self.surface_scale = scale;
            self.render_stage = RenderStage::Final;
        }

        self.surface.as_ref().ok_or_else(|| anyhow!("No surface"))
    }

    /// Get the current render scale (may differ from target during preview)
    pub fn current_scale(&self) -> f64 {
        self.surface_scale
    }

    /// Check if currently showing preview quality
    pub fn is_preview(&self) -> bool {
        self.render_stage == RenderStage::Preview
    }

    // Navigation
    pub fn next_image(&mut self) -> Result<bool> {
        if self.directory_files.is_empty() {
            return Ok(false);
        }

        let next_index = (self.current_index + 1) % self.directory_files.len();
        if next_index != self.current_index {
            let path = self.directory_files[next_index].clone();
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
        self.render_stage = RenderStage::Preview;
    }

    pub fn rotate_ccw(&mut self) {
        self.rotation = (self.rotation + 270) % 360;
        self.surface = None;
        self.render_stage = RenderStage::Preview;
    }

    pub fn rotation(&self) -> i32 {
        self.rotation
    }

    // Flip
    pub fn flip_horizontal(&mut self) {
        self.flip_h = !self.flip_h;
        self.surface = None;
        self.render_stage = RenderStage::Preview;
    }

    pub fn flip_vertical(&mut self) {
        self.flip_v = !self.flip_v;
        self.surface = None;
        self.render_stage = RenderStage::Preview;
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
