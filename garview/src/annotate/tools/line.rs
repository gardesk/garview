//! Line tool - draw straight lines.

use super::Tool;
use crate::annotate::state::ToolProperties;
use cairo::Context;
use gartk_core::{InputEvent, MouseButton, Point};
use gartk_x11::CursorShape;

/// Line drawing tool.
pub struct LineTool {
    start: Option<Point>,
    end: Option<Point>,
    drawing: bool,
}

impl LineTool {
    pub fn new() -> Self {
        Self {
            start: None,
            end: None,
            drawing: false,
        }
    }

    fn draw_line(ctx: &Context, start: Point, end: Point, props: &ToolProperties) {
        ctx.set_source_rgba(
            props.color.r as f64,
            props.color.g as f64,
            props.color.b as f64,
            props.color.a as f64,
        );
        ctx.set_line_width(props.line_width);
        ctx.set_line_cap(cairo::LineCap::Round);

        ctx.move_to(start.x as f64, start.y as f64);
        ctx.line_to(end.x as f64, end.y as f64);
        let _ = ctx.stroke();
    }
}

impl Default for LineTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for LineTool {
    fn handle_event(&mut self, event: &InputEvent, _props: &ToolProperties) -> bool {
        match event {
            InputEvent::MousePress(e) if e.button == Some(MouseButton::Left) => {
                self.start = Some(e.position);
                self.end = Some(e.position);
                self.drawing = true;
                true
            }
            InputEvent::MouseMove(e) if self.drawing => {
                self.end = Some(e.position);
                true
            }
            InputEvent::MouseRelease(e) if e.button == Some(MouseButton::Left) && self.drawing => {
                self.end = Some(e.position);
                self.drawing = false;
                true
            }
            _ => false,
        }
    }

    fn draw_preview(&self, ctx: &Context, props: &ToolProperties) {
        if let (Some(start), Some(end)) = (self.start, self.end) {
            Self::draw_line(ctx, start, end, props);
        }
    }

    fn commit(&self, ctx: &Context, props: &ToolProperties) {
        self.draw_preview(ctx, props);
    }

    fn reset(&mut self) {
        self.start = None;
        self.end = None;
        self.drawing = false;
    }

    fn cursor(&self) -> CursorShape {
        CursorShape::Crosshair
    }

    fn is_drawing(&self) -> bool {
        self.drawing
    }

    fn can_commit(&self) -> bool {
        if let (Some(start), Some(end)) = (self.start, self.end) {
            start.x != end.x || start.y != end.y
        } else {
            false
        }
    }
}
