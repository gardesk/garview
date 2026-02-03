//! Annotation canvas with layered surfaces.
//!
//! The canvas manages three layers:
//! 1. Background: Original image (immutable)
//! 2. Annotations: Committed drawings
//! 3. Preview: Current tool preview (uncommitted)

use anyhow::{Context, Result};
use gartk_core::Size;
use gartk_render::Surface;

/// Annotation canvas with layered rendering.
pub struct AnnotationCanvas {
    /// Original image (immutable background).
    background: Surface,
    /// Committed annotations layer.
    annotations: Surface,
    /// Live preview of current tool.
    preview: Surface,
    /// Composited output for display.
    composite: Surface,
    /// Canvas dimensions.
    size: Size,
}

impl AnnotationCanvas {
    /// Create a new annotation canvas from image data.
    pub fn new(image_data: &[u8], width: u32, height: u32) -> Result<Self> {
        let size = Size::new(width, height);

        // Create background surface from image data
        let background = Surface::from_rgba(image_data, width, height)
            .context("Failed to create background surface")?;

        // Create transparent annotation layer
        let annotations = Surface::new(width, height)
            .context("Failed to create annotation surface")?;

        // Create transparent preview layer
        let preview = Surface::new(width, height)
            .context("Failed to create preview surface")?;

        // Create composite surface for display
        let composite = Surface::new(width, height)
            .context("Failed to create composite surface")?;

        Ok(Self {
            background,
            annotations,
            preview,
            composite,
            size,
        })
    }

    /// Get canvas dimensions.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Get width.
    pub fn width(&self) -> u32 {
        self.size.width
    }

    /// Get height.
    pub fn height(&self) -> u32 {
        self.size.height
    }

    /// Get the annotations surface for drawing.
    pub fn annotations_surface(&self) -> &Surface {
        &self.annotations
    }

    /// Get the preview surface for drawing.
    pub fn preview_surface(&self) -> &Surface {
        &self.preview
    }

    /// Get the composite surface for display.
    pub fn composite_surface(&self) -> &Surface {
        &self.composite
    }

    /// Get mutable reference to composite surface.
    pub fn composite_surface_mut(&mut self) -> &mut Surface {
        &mut self.composite
    }

    /// Clear the preview layer.
    pub fn clear_preview(&self) -> Result<()> {
        let ctx = self.preview.context()?;
        ctx.set_operator(cairo::Operator::Clear);
        ctx.paint()?;
        ctx.set_operator(cairo::Operator::Over);
        Ok(())
    }

    /// Commit preview to annotations layer.
    pub fn commit_preview(&self) -> Result<()> {
        // Paint preview onto annotations
        let ctx = self.annotations.context()?;
        ctx.set_source_surface(self.preview.cairo_surface(), 0.0, 0.0)?;
        ctx.paint()?;

        // Clear preview
        self.clear_preview()?;

        Ok(())
    }

    /// Render all layers to composite surface.
    pub fn render(&self) -> Result<()> {
        let ctx = self.composite.context()?;

        // Clear composite
        ctx.set_operator(cairo::Operator::Source);

        // Draw background
        ctx.set_source_surface(self.background.cairo_surface(), 0.0, 0.0)?;
        ctx.paint()?;

        // Draw annotations with alpha blending
        ctx.set_operator(cairo::Operator::Over);
        ctx.set_source_surface(self.annotations.cairo_surface(), 0.0, 0.0)?;
        ctx.paint()?;

        // Draw preview
        ctx.set_source_surface(self.preview.cairo_surface(), 0.0, 0.0)?;
        ctx.paint()?;

        Ok(())
    }

    /// Export the final composited image as RGBA data.
    pub fn export(&mut self) -> Result<Vec<u8>> {
        // Render final composite
        self.render()?;

        // Get pixel data from composite surface
        self.composite
            .to_rgba()
            .context("Failed to export canvas data")
    }

    /// Get a snapshot of the annotations layer for undo.
    pub fn snapshot_annotations(&mut self) -> Result<Vec<u8>> {
        self.annotations
            .to_rgba()
            .context("Failed to snapshot annotations")
    }

    /// Restore annotations layer from snapshot.
    pub fn restore_annotations(&self, data: &[u8]) -> Result<()> {
        let ctx = self.annotations.context()?;

        // Clear current annotations
        ctx.set_operator(cairo::Operator::Clear);
        ctx.paint()?;

        // Create temporary surface from snapshot data
        let temp = Surface::from_rgba(data, self.size.width, self.size.height)?;

        // Paint snapshot onto annotations
        ctx.set_operator(cairo::Operator::Source);
        ctx.set_source_surface(temp.cairo_surface(), 0.0, 0.0)?;
        ctx.paint()?;

        ctx.set_operator(cairo::Operator::Over);
        Ok(())
    }

    /// Clear all annotations.
    pub fn clear_annotations(&self) -> Result<()> {
        let ctx = self.annotations.context()?;
        ctx.set_operator(cairo::Operator::Clear);
        ctx.paint()?;
        ctx.set_operator(cairo::Operator::Over);
        Ok(())
    }
}
