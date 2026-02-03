//! Highlight tool - semi-transparent marker.

use super::Tool;
use crate::annotate::state::ToolProperties;
use cairo::Context;
use gartk_core::{InputEvent, MouseButton, Point};
use gartk_x11::CursorShape;

/// Highlight tool (semi-transparent freehand).
pub struct HighlightTool {
    points: Vec<Point>,
    drawing: bool,
}

impl HighlightTool {
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            drawing: false,
        }
    }

    fn draw_highlight(ctx: &Context, points: &[Point], props: &ToolProperties) {
        if points.len() < 2 {
            return;
        }

        // Use semi-transparent yellow by default, or color with reduced alpha
        ctx.set_source_rgba(
            props.color.r as f64,
            props.color.g as f64,
            props.color.b as f64,
            0.4, // Fixed 40% opacity for highlight effect
        );
        ctx.set_line_width(props.line_width * 4.0); // Wider stroke for highlight
        ctx.set_line_cap(cairo::LineCap::Round);
        ctx.set_line_join(cairo::LineJoin::Round);

        ctx.move_to(points[0].x as f64, points[0].y as f64);
        for point in points.iter().skip(1) {
            ctx.line_to(point.x as f64, point.y as f64);
        }
        let _ = ctx.stroke();
    }
}

impl Default for HighlightTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for HighlightTool {
    fn handle_event(&mut self, event: &InputEvent, _props: &ToolProperties) -> bool {
        match event {
            InputEvent::MousePress(e) if e.button == Some(MouseButton::Left) => {
                self.points.clear();
                self.points.push(e.position);
                self.drawing = true;
                true
            }
            InputEvent::MouseMove(e) if self.drawing => {
                self.points.push(e.position);
                true
            }
            InputEvent::MouseRelease(e) if e.button == Some(MouseButton::Left) && self.drawing => {
                self.points.push(e.position);
                self.drawing = false;
                true
            }
            _ => false,
        }
    }

    fn draw_preview(&self, ctx: &Context, props: &ToolProperties) {
        Self::draw_highlight(ctx, &self.points, props);
    }

    fn commit(&self, ctx: &Context, props: &ToolProperties) {
        self.draw_preview(ctx, props);
    }

    fn reset(&mut self) {
        self.points.clear();
        self.drawing = false;
    }

    fn cursor(&self) -> CursorShape {
        CursorShape::Crosshair
    }

    fn is_drawing(&self) -> bool {
        self.drawing
    }

    fn can_commit(&self) -> bool {
        self.points.len() >= 2
    }
}
