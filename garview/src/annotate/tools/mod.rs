//! Annotation tools.
//!
//! Each tool implements the [`Tool`] trait for consistent behavior.

mod arrow;
mod blur;
mod brush;
mod ellipse;
mod highlight;
mod line;
mod rectangle;
mod text;

pub use arrow::ArrowTool;
pub use blur::{box_blur, BlurTool};
pub use brush::BrushTool;
pub use ellipse::EllipseTool;
pub use highlight::HighlightTool;
pub use line::LineTool;
pub use rectangle::RectangleTool;
pub use text::TextTool;

use crate::annotate::state::ToolProperties;
use cairo::Context;
use gartk_core::{InputEvent, Rect};
use gartk_x11::CursorShape;

/// Tool behavior trait.
///
/// All annotation tools implement this trait for consistent handling.
pub trait Tool {
    /// Handle an input event.
    ///
    /// Returns `true` if the preview needs to be redrawn.
    fn handle_event(&mut self, event: &InputEvent, props: &ToolProperties) -> bool;

    /// Draw the current preview to the context.
    fn draw_preview(&self, ctx: &Context, props: &ToolProperties);

    /// Commit the current drawing to the annotations layer.
    fn commit(&self, ctx: &Context, props: &ToolProperties);

    /// Reset the tool state (clear current drawing).
    fn reset(&mut self);

    /// Get the cursor shape for this tool.
    fn cursor(&self) -> CursorShape;

    /// Check if the tool has an active drawing in progress.
    fn is_drawing(&self) -> bool;

    /// Check if the tool is ready to commit (has a valid drawing).
    fn can_commit(&self) -> bool;

    /// Get the bounding box of the current drawing.
    fn bounds(&self) -> Option<Rect>;
}

/// Create a boxed tool instance for the given tool type.
pub fn create_tool(tool_type: super::state::ToolType) -> Box<dyn Tool> {
    use super::state::ToolType;

    match tool_type {
        ToolType::Brush => Box::new(BrushTool::new()),
        ToolType::Line => Box::new(LineTool::new()),
        ToolType::Arrow => Box::new(ArrowTool::new()),
        ToolType::Rectangle => Box::new(RectangleTool::new()),
        ToolType::Ellipse => Box::new(EllipseTool::new()),
        ToolType::Text => Box::new(TextTool::new()),
        ToolType::Blur => Box::new(BlurTool::new()),
        ToolType::Highlight => Box::new(HighlightTool::new()),
    }
}
