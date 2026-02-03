//! Annotation system for garview.
//!
//! Provides drawing tools for annotating images and documents.

mod canvas;
mod history;
pub mod state;
pub mod tools;

pub use canvas::AnnotationCanvas;
pub use history::History;
pub use state::{AnnotationState, ToolProperties, ToolType};
pub use tools::{create_tool, Tool};

use anyhow::Result;
use gartk_core::InputEvent;

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
        })
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
        self.tool.reset();

        Ok(())
    }

    /// Get the cursor for the current tool.
    pub fn cursor(&self) -> gartk_x11::CursorShape {
        self.tool.cursor()
    }
}
