//! Arrow tool - draw arrows with configurable heads.

use super::Tool;
use crate::annotate::state::ToolProperties;
use cairo::Context;
use gartk_core::{InputEvent, MouseButton, Point};
use gartk_x11::CursorShape;
use std::f64::consts::PI;

/// Arrow drawing tool.
pub struct ArrowTool {
    start: Option<Point>,
    end: Option<Point>,
    drawing: bool,
}

impl ArrowTool {
    pub fn new() -> Self {
        Self {
            start: None,
            end: None,
            drawing: false,
        }
    }

    fn draw_arrow(ctx: &Context, start: Point, end: Point, props: &ToolProperties) {
        let x1 = start.x as f64;
        let y1 = start.y as f64;
        let x2 = end.x as f64;
        let y2 = end.y as f64;

        ctx.set_source_rgba(
            props.color.r as f64,
            props.color.g as f64,
            props.color.b as f64,
            props.color.a as f64,
        );
        ctx.set_line_width(props.line_width);
        ctx.set_line_cap(cairo::LineCap::Round);
        ctx.set_line_join(cairo::LineJoin::Round);

        // Draw main line
        ctx.move_to(x1, y1);
        ctx.line_to(x2, y2);
        let _ = ctx.stroke();

        // Calculate arrowhead
        let angle = (y2 - y1).atan2(x2 - x1);
        let head_length = props.line_width * 4.0;
        let head_angle = PI / 6.0; // 30 degrees

        let ax1 = x2 - head_length * (angle + head_angle).cos();
        let ay1 = y2 - head_length * (angle + head_angle).sin();
        let ax2 = x2 - head_length * (angle - head_angle).cos();
        let ay2 = y2 - head_length * (angle - head_angle).sin();

        // Draw arrowhead
        ctx.move_to(ax1, ay1);
        ctx.line_to(x2, y2);
        ctx.line_to(ax2, ay2);
        let _ = ctx.stroke();
    }
}

impl Default for ArrowTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ArrowTool {
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
            Self::draw_arrow(ctx, start, end, props);
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
            let dx = (start.x - end.x).abs();
            let dy = (start.y - end.y).abs();
            dx > 5 || dy > 5
        } else {
            false
        }
    }
}
