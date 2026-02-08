//! Annotation system for garview.
//!
//! Provides drawing tools for annotating images and documents.

mod canvas;
mod history;
pub mod state;
pub mod tools;

pub use canvas::AnnotationCanvas;
pub use history::History;
pub use state::{AnnotationRecord, AnnotationState, SerializableAnnotation, ToolType};
pub use tools::{box_blur, create_tool, Tool};

use anyhow::{Context, Result};
use gartk_core::{InputEvent, Rect};
use std::path::Path;

/// Check if a line segment intersects with a rectangle.
fn line_intersects_rect(x1: i32, y1: i32, x2: i32, y2: i32, rect: &Rect) -> bool {
    let rx = rect.x as f64;
    let ry = rect.y as f64;
    let rw = rect.width as f64;
    let rh = rect.height as f64;

    // Check if either endpoint is inside the rect
    let x1f = x1 as f64;
    let y1f = y1 as f64;
    let x2f = x2 as f64;
    let y2f = y2 as f64;

    if (x1f >= rx && x1f <= rx + rw && y1f >= ry && y1f <= ry + rh)
        || (x2f >= rx && x2f <= rx + rw && y2f >= ry && y2f <= ry + rh)
    {
        return true;
    }

    // Check if line intersects any of the rectangle's edges
    let edges = [
        (rx, ry, rx + rw, ry),           // top
        (rx, ry + rh, rx + rw, ry + rh), // bottom
        (rx, ry, rx, ry + rh),           // left
        (rx + rw, ry, rx + rw, ry + rh), // right
    ];

    for (ex1, ey1, ex2, ey2) in edges {
        if segments_intersect(x1f, y1f, x2f, y2f, ex1, ey1, ex2, ey2) {
            return true;
        }
    }

    false
}

/// Check if two line segments intersect.
fn segments_intersect(
    x1: f64, y1: f64, x2: f64, y2: f64,
    x3: f64, y3: f64, x4: f64, y4: f64,
) -> bool {
    let d = (x1 - x2) * (y3 - y4) - (y1 - y2) * (x3 - x4);
    if d.abs() < 0.0001 {
        return false; // Parallel
    }

    let t = ((x1 - x3) * (y3 - y4) - (y1 - y3) * (x3 - x4)) / d;
    let u = -((x1 - x2) * (y1 - y3) - (y1 - y2) * (x1 - x3)) / d;

    t >= 0.0 && t <= 1.0 && u >= 0.0 && u <= 1.0
}

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
    /// Redo stack for annotation records.
    annotations_redo: Vec<AnnotationRecord>,
    /// Next annotation ID.
    next_id: u64,
    /// Current page (for multi-page documents).
    current_page: usize,
    /// Currently selected annotation IDs (for deletion).
    selected_annotations: Vec<u64>,
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
            annotations_redo: Vec::new(),
            next_id: 1,
            current_page: 0,
            selected_annotations: Vec::new(),
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

    /// Hit-test annotations at a point, return topmost matching record.
    /// Uses geometry-aware hit-testing for line-based tools.
    pub fn annotation_at(&self, x: i32, y: i32, page: usize) -> Option<&AnnotationRecord> {
        const HIT_TOLERANCE: f64 = 8.0; // pixels
        // Iterate in reverse (topmost/newest first) for correct z-order
        self.annotations
            .iter()
            .rev()
            .filter(|a| a.page == page)
            .find(|a| a.contains_point(x, y, HIT_TOLERANCE))
    }

    /// Find annotations that intersect with a rectangle (rubber band selection).
    /// Returns all intersecting annotations (topmost first).
    pub fn annotations_in_rect(&self, rect: Rect, page: usize) -> Vec<&AnnotationRecord> {
        // Check if any point of the annotation is within the selection rect
        // For line-based tools, check if the line intersects the rect
        self.annotations
            .iter()
            .rev()
            .filter(|a| a.page == page)
            .filter(|a| {
                // First check bounding box intersection
                if !a.bounds.intersects(rect) {
                    return false;
                }
                // For line-based tools, also check if line actually passes through rect
                match a.tool {
                    ToolType::Arrow | ToolType::Line => {
                        if let (Some((x1, y1)), Some((x2, y2))) = (a.start_point, a.end_point) {
                            // Check if line segment intersects rectangle
                            line_intersects_rect(x1, y1, x2, y2, &rect)
                        } else {
                            true // Fallback to bounds check
                        }
                    }
                    _ => true, // Other tools use bounds
                }
            })
            .collect()
    }

    /// Find a single annotation that intersects with a rectangle.
    /// Returns the topmost intersecting annotation.
    pub fn annotation_in_rect(&self, rect: Rect, page: usize) -> Option<&AnnotationRecord> {
        self.annotations_in_rect(rect, page).into_iter().next()
    }

    /// Select an annotation by ID (clears other selections).
    pub fn select(&mut self, id: Option<u64>) {
        self.selected_annotations.clear();
        if let Some(id) = id {
            self.selected_annotations.push(id);
        }
    }

    /// Select multiple annotations by ID (clears previous selections).
    pub fn select_multiple(&mut self, ids: Vec<u64>) {
        self.selected_annotations = ids;
    }

    /// Add an annotation to selection (for Ctrl+click multi-select).
    pub fn add_to_selection(&mut self, id: u64) {
        if !self.selected_annotations.contains(&id) {
            self.selected_annotations.push(id);
        }
    }

    /// Remove an annotation from selection.
    pub fn remove_from_selection(&mut self, id: u64) {
        self.selected_annotations.retain(|&x| x != id);
    }

    /// Toggle an annotation's selection state.
    /// Returns true if the annotation is now selected, false if deselected.
    pub fn toggle_selection(&mut self, id: u64) -> bool {
        if self.selected_annotations.contains(&id) {
            self.selected_annotations.retain(|&x| x != id);
            false
        } else {
            self.selected_annotations.push(id);
            true
        }
    }

    /// Get selected annotation IDs.
    pub fn selected(&self) -> &[u64] {
        &self.selected_annotations
    }

    /// Check if any annotation is selected.
    pub fn has_selection(&self) -> bool {
        !self.selected_annotations.is_empty()
    }

    /// Check if a specific annotation is selected.
    pub fn is_selected(&self, id: u64) -> bool {
        self.selected_annotations.contains(&id)
    }

    /// Get the first selected annotation record (for backwards compatibility).
    pub fn selected_record(&self) -> Option<&AnnotationRecord> {
        let id = self.selected_annotations.first()?;
        self.annotations.iter().find(|a| a.id == *id)
    }

    /// Get all selected annotation records.
    pub fn selected_records(&self) -> Vec<&AnnotationRecord> {
        self.selected_annotations
            .iter()
            .filter_map(|id| self.annotations.iter().find(|a| a.id == *id))
            .collect()
    }

    /// Delete the selected annotation(s).
    pub fn delete_selected(&mut self) -> Result<bool> {
        if self.selected_annotations.is_empty() {
            return Ok(false);
        }

        // Save current state for undo
        let current = self.canvas.snapshot_annotations()?;
        self.history.push(current);

        // Collect annotations to delete (in reverse order to avoid index shifting issues)
        let mut indices_to_remove: Vec<usize> = self.selected_annotations
            .iter()
            .filter_map(|id| self.annotations.iter().position(|a| a.id == *id))
            .collect();
        indices_to_remove.sort_by(|a, b| b.cmp(a)); // Sort descending

        // Erase each annotation region and remove from records
        for idx in &indices_to_remove {
            let bounds = self.annotations[*idx].bounds;
            let id = self.annotations[*idx].id;
            self.canvas.erase_region(bounds.x, bounds.y, bounds.width, bounds.height)?;
            tracing::info!("Deleted annotation {} at {:?}", id, bounds);
            self.annotations.remove(*idx);
        }

        self.annotations_redo.clear();
        let count = self.selected_annotations.len();
        self.selected_annotations.clear();
        self.modified = true;

        tracing::info!("Deleted {} annotation(s)", count);

        Ok(true)
    }

    /// Get the sidecar annotation PNG file path for a given file.
    pub fn annotation_path(file_path: &Path) -> std::path::PathBuf {
        let mut path = file_path.to_path_buf();
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".annotations.png");
        path.set_file_name(name);
        path
    }

    /// Get the sidecar annotation JSON file path for a given file.
    pub fn annotation_json_path(file_path: &Path) -> std::path::PathBuf {
        let mut path = file_path.to_path_buf();
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".annotations.json");
        path.set_file_name(name);
        path
    }

    /// Check if annotations exist for a file.
    pub fn has_annotations(file_path: &Path) -> bool {
        Self::annotation_path(file_path).exists()
    }

    /// Load existing annotations from sidecar file.
    /// Returns (RGBA data, width, height) if annotations exist.
    pub fn load_annotations(file_path: &Path) -> Result<Option<(Vec<u8>, u32, u32)>> {
        let ann_path = Self::annotation_path(file_path);
        if !ann_path.exists() {
            return Ok(None);
        }

        let img = image::open(&ann_path)
            .with_context(|| format!("Failed to load annotations from {:?}", ann_path))?;
        let rgba = img.to_rgba8();
        let width = rgba.width();
        let height = rgba.height();
        Ok(Some((rgba.into_raw(), width, height)))
    }

    /// Load annotation records from JSON sidecar file.
    pub fn load_annotation_records(file_path: &Path) -> Result<Vec<AnnotationRecord>> {
        let json_path = Self::annotation_json_path(file_path);
        if !json_path.exists() {
            tracing::debug!("No annotation JSON file at {:?}", json_path);
            return Ok(Vec::new());
        }

        tracing::debug!("Loading annotation records from {:?}", json_path);
        let content = std::fs::read_to_string(&json_path)
            .with_context(|| format!("Failed to read {:?}", json_path))?;
        let records: Vec<state::SerializableAnnotation> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {:?}", json_path))?;
        tracing::debug!("Parsed {} annotation records", records.len());
        Ok(records.into_iter().map(|r| r.to_record()).collect())
    }

    /// Restore annotation records from loaded data.
    pub fn restore_records(&mut self, records: Vec<AnnotationRecord>) {
        // Find the max ID to continue from
        let max_id = records.iter().map(|r| r.id).max().unwrap_or(0);
        self.next_id = max_id + 1;
        self.annotations = records;
    }

    /// Save annotations to sidecar files (PNG + JSON).
    pub fn save_annotations(&mut self, file_path: &Path) -> Result<()> {
        let ann_path = Self::annotation_path(file_path);
        let json_path = Self::annotation_json_path(file_path);
        let data = self.canvas.snapshot_annotations()?;
        let width = self.canvas.width();
        let height = self.canvas.height();

        // Check if annotations are empty (all transparent)
        let is_empty = data.chunks(4).all(|px| px[3] == 0);
        if is_empty {
            // Delete sidecar files if they exist and annotations are empty
            if ann_path.exists() {
                std::fs::remove_file(&ann_path)?;
            }
            if json_path.exists() {
                std::fs::remove_file(&json_path)?;
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

        // Save annotation records as JSON
        let records: Vec<state::SerializableAnnotation> = self.annotations
            .iter()
            .map(state::SerializableAnnotation::from_record)
            .collect();
        let json = serde_json::to_string_pretty(&records)
            .context("Failed to serialize annotation records")?;
        std::fs::write(&json_path, json)
            .with_context(|| format!("Failed to write {:?}", json_path))?;

        self.modified = false;
        tracing::info!("Saved annotations to {:?} and {:?}", ann_path, json_path);
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
        if !self.tool.is_drawing() {
            if self.tool.can_commit() {
                self.commit_current()?;
            } else {
                // Tool finished but can't commit (e.g., too small) - reset it
                self.tool.reset();
                self.canvas.clear_preview()?;
            }
        }

        Ok(needs_redraw)
    }

    /// Commit the current tool drawing.
    pub fn commit_current(&mut self) -> Result<()> {
        // Clear redo stack on new action
        self.annotations_redo.clear();

        // Record annotation metadata before committing
        if let Some(bounds) = self.tool.bounds() {
            let record = if let Some((start, end)) = self.tool.endpoints() {
                // Line-based tool with endpoints
                AnnotationRecord::new_with_endpoints(
                    self.next_id,
                    self.state.current_tool,
                    bounds,
                    self.state.properties.color,
                    self.current_page,
                    start,
                    end,
                )
            } else {
                // Other tools use simple bounding box
                AnnotationRecord::new(
                    self.next_id,
                    self.state.current_tool,
                    bounds,
                    self.state.properties.color,
                    self.current_page,
                )
            };
            self.annotations.push(record);
            self.next_id += 1;
        }

        // Save snapshot for undo
        let snapshot = self.canvas.snapshot_annotations()?;
        self.history.push(snapshot);

        // Handle blur tool specially - needs to read and modify pixels
        if self.state.current_tool == ToolType::Blur {
            tracing::debug!("Blur tool commit - checking bounds");
            if let Some(bounds) = self.tool.bounds() {
                tracing::debug!("Blur bounds: {:?}", bounds);
                // Clamp bounds to canvas with saturating subtraction to avoid overflow
                let x = bounds.x.max(0);
                let y = bounds.y.max(0);
                let canvas_w = self.canvas.width();
                let canvas_h = self.canvas.height();
                let w = bounds.width.min(canvas_w.saturating_sub(x as u32));
                let h = bounds.height.min(canvas_h.saturating_sub(y as u32));
                tracing::debug!("Clamped blur region: x={}, y={}, w={}, h={} (canvas {}x{})", x, y, w, h, canvas_w, canvas_h);

                if w > 0 && h > 0 {
                    // Get region pixels (background + existing annotations)
                    let mut pixels = self.canvas.get_region_for_blur(x, y, w, h)?;
                    tracing::debug!("Got {} pixels for blur region", pixels.len());

                    // Apply blur
                    box_blur(&mut pixels, w, h, self.state.properties.blur_radius);
                    tracing::debug!("Applied blur with radius {}", self.state.properties.blur_radius);

                    // Paint blurred pixels back
                    self.canvas.paint_blurred_region(&pixels, x, y, w, h)?;
                    tracing::info!("Blur applied to region {}x{} at ({}, {})", w, h, x, y);
                } else {
                    tracing::warn!("Blur region too small after clamping: {}x{}", w, h);
                }
            } else {
                tracing::warn!("Blur tool has no bounds");
            }
        } else {
            // Commit preview to annotations for other tools
            if let Ok(ctx) = self.canvas.annotations_surface().context() {
                self.tool.commit(&ctx, &self.state.properties);
            }
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
            // Move the last annotation record to redo stack
            if let Some(record) = self.annotations.pop() {
                self.annotations_redo.push(record);
            }
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
            // Restore the annotation record from redo stack
            if let Some(record) = self.annotations_redo.pop() {
                self.annotations.push(record);
            }
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
        // Clear both stacks (can't properly undo/redo records for bulk clear)
        self.annotations.clear();
        self.annotations_redo.clear();
        self.tool.reset();
        self.modified = true;

        Ok(())
    }

    /// Get the cursor for the current tool.
    pub fn cursor(&self) -> gartk_x11::CursorShape {
        self.tool.cursor()
    }
}
