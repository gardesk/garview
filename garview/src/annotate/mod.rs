//! Annotation system for garview.
//!
//! Provides drawing tools for annotating images and documents.

mod canvas;
mod history;
pub mod state;
pub mod tools;

pub use canvas::AnnotationCanvas;
pub use history::History;
pub use state::{AnnotationRecord, AnnotationState, ToolType};
pub use tools::{create_tool, Tool};

use anyhow::{Context, Result};
use gartk_core::InputEvent;
use std::path::Path;

/// Annotation manager combining canvas, tools, and history.
pub struct AnnotationManager {
    /// The annotation canvas (layered surfaces).
    pub canvas: AnnotationCanvas,
    /// Current tool instance.
    pub tool: Box<dyn Tool>,
    /// Annotation state (tool type, properties).
    pub state: AnnotationState,
    /// Undo/redo history.
    pub history: History,
    /// Whether annotations have been modified since last save.
    pub modified: bool,
    /// List of annotation records for sidebar display.
    annotations: Vec<AnnotationRecord>,
    /// Next annotation ID.
    next_id: u64,
    /// Current page (for multi-page documents).
    current_page: usize,
}

impl AnnotationManager {
    /// Create a new annotation manager for the given image.
    pub fn new(image_data: &[u8], width: u32, height: u32) -> Result<Self> {
        let canvas = AnnotationCanvas::new(image_data, width, height)?;
        let state = AnnotationState::new();
        let tool = create_tool(state.current_tool);
        let history = History::new(50);

        Ok(Self {
            canvas,
            tool,
            state,
            history,
            modified: false,
            annotations: Vec::new(),
            next_id: 1,
            current_page: 0,
        })
    }

    /// Set the current page for new annotations.
    pub fn set_page(&mut self, page: usize) {
        self.current_page = page;
    }

    /// Get the list of annotation records.
    pub fn annotations(&self) -> &[AnnotationRecord] {
        &self.annotations
    }

    /// Get annotations for a specific page.
    pub fn annotations_for_page(&self, page: usize) -> Vec<&AnnotationRecord> {
        self.annotations.iter().filter(|a| a.page == page).collect()
    }

    /// Get number of annotations.
    pub fn annotation_count(&self) -> usize {
        self.annotations.len()
    }

    /// Get the sidecar annotation file path for a given file.
    pub fn annotation_path(file_path: &Path) -> std::path::PathBuf {
        let mut path = file_path.to_path_buf();
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".annotations.png");
        path.set_file_name(name);
        path
    }

    /// Check if annotations exist for a file.
    pub fn has_annotations(file_path: &Path) -> bool {
        Self::annotation_path(file_path).exists()
    }

    /// Load existing annotations from sidecar file.
    pub fn load_annotations(file_path: &Path) -> Result<Option<Vec<u8>>> {
        let ann_path = Self::annotation_path(file_path);
        if !ann_path.exists() {
            return Ok(None);
        }

        let img = image::open(&ann_path)
            .with_context(|| format!("Failed to load annotations from {:?}", ann_path))?;
        let rgba = img.to_rgba8();
        Ok(Some(rgba.into_raw()))
    }

    /// Save annotations to sidecar file.
    pub fn save_annotations(&mut self, file_path: &Path) -> Result<()> {
        let ann_path = Self::annotation_path(file_path);
        let data = self.canvas.snapshot_annotations()?;
        let width = self.canvas.width();
        let height = self.canvas.height();

        // Check if annotations are empty (all transparent)
        let is_empty = data.chunks(4).all(|px| px[3] == 0);
        if is_empty {
            // Delete sidecar file if it exists and annotations are empty
            if ann_path.exists() {
                std::fs::remove_file(&ann_path)?;
            }
            self.modified = false;
            return Ok(());
        }

        // Save as PNG
        image::save_buffer(
            &ann_path,
            &data,
            width,
            height,
            image::ColorType::Rgba8,
        ).with_context(|| format!("Failed to save annotations to {:?}", ann_path))?;

        self.modified = false;
        tracing::info!("Saved annotations to {:?}", ann_path);
        Ok(())
    }

    /// Restore annotations from loaded data.
    pub fn restore_from_data(&self, data: &[u8]) -> Result<()> {
        self.canvas.restore_annotations(data)
    }

    /// Handle an input event.
    ///
    /// Returns `true` if redraw is needed.
    pub fn handle_event(&mut self, event: &InputEvent) -> Result<bool> {
        let needs_redraw = self.tool.handle_event(event, &self.state.properties);

        if needs_redraw {
            // Clear preview and redraw
            self.canvas.clear_preview()?;

            if let Ok(ctx) = self.canvas.preview_surface().context() {
                self.tool.draw_preview(&ctx, &self.state.properties);
            }
        }

        // Check if tool finished drawing (mouse released)
        if !self.tool.is_drawing() && self.tool.can_commit() {
            self.commit_current()?;
        }

        Ok(needs_redraw)
    }

    /// Commit the current tool drawing.
    pub fn commit_current(&mut self) -> Result<()> {
        // Record annotation metadata before committing
        if let Some(bounds) = self.tool.bounds() {
            let record = AnnotationRecord::new(
                self.next_id,
                self.state.current_tool,
                bounds,
                self.state.properties.color,
                self.current_page,
            );
            self.annotations.push(record);
            self.next_id += 1;
        }

        // Save snapshot for undo
        let snapshot = self.canvas.snapshot_annotations()?;
        self.history.push(snapshot);

        // Commit preview to annotations
        if let Ok(ctx) = self.canvas.annotations_surface().context() {
            self.tool.commit(&ctx, &self.state.properties);
        }

        // Clear preview
        self.canvas.clear_preview()?;

        // Reset tool
        self.tool.reset();

        // Mark as modified
        self.modified = true;

        Ok(())
    }

    /// Select a different tool.
    pub fn select_tool(&mut self, tool_type: ToolType) {
        // Reset current tool
        self.tool.reset();

        // Update state and create new tool
        self.state.select_tool(tool_type);
        self.tool = create_tool(tool_type);
    }

    /// Undo the last action.
    pub fn undo(&mut self) -> Result<bool> {
        let current = self.canvas.snapshot_annotations()?;
        if let Some(previous) = self.history.undo(current) {
            self.canvas.restore_annotations(&previous)?;
            // Remove the last annotation record
            self.annotations.pop();
            self.modified = true;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Redo the last undone action.
    pub fn redo(&mut self) -> Result<bool> {
        let current = self.canvas.snapshot_annotations()?;
        if let Some(next) = self.history.redo(current) {
            self.canvas.restore_annotations(&next)?;
            self.modified = true;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Render the canvas (all layers composited).
    pub fn render(&self) -> Result<()> {
        self.canvas.render()
    }

    /// Export the final annotated image.
    pub fn export(&mut self) -> Result<Vec<u8>> {
        self.canvas.export()
    }

    /// Check if undo is available.
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    /// Check if redo is available.
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Clear all annotations.
    pub fn clear_all(&mut self) -> Result<()> {
        // Save snapshot for undo
        let snapshot = self.canvas.snapshot_annotations()?;
        self.history.push(snapshot);

        self.canvas.clear_annotations()?;
        self.annotations.clear();
        self.tool.reset();
        self.modified = true;

        Ok(())
    }

    /// Get the cursor for the current tool.
    pub fn cursor(&self) -> gartk_x11::CursorShape {
        self.tool.cursor()
    }
}
