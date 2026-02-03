//! Ellipse tool - draw ellipses/circles.

use super::Tool;
use crate::annotate::state::ToolProperties;
use cairo::Context;
use gartk_core::{InputEvent, MouseButton, Point};
use gartk_x11::CursorShape;
use std::f64::consts::PI;

/// Ellipse drawing tool.
pub struct EllipseTool {
    start: Option<Point>,
    end: Option<Point>,
    drawing: bool,
}

impl EllipseTool {
    pub fn new() -> Self {
        Self {
            start: None,
            end: None,
            drawing: false,
        }
    }

    fn draw_ellipse(ctx: &Context, start: Point, end: Point, props: &ToolProperties) {
        let x = start.x.min(end.x) as f64;
        let y = start.y.min(end.y) as f64;
        let w = (start.x - end.x).abs() as f64;
        let h = (start.y - end.y).abs() as f64;

        if w < 1.0 || h < 1.0 {
            return;
        }

        let cx = x + w / 2.0;
        let cy = y + h / 2.0;

        ctx.set_source_rgba(
            props.color.r as f64,
            props.color.g as f64,
            props.color.b as f64,
            props.color.a as f64,
        );

        ctx.save().ok();
        ctx.translate(cx, cy);
        ctx.scale(w / 2.0, h / 2.0);
        ctx.arc(0.0, 0.0, 1.0, 0.0, 2.0 * PI);
        ctx.restore().ok();

        if props.fill {
            let _ = ctx.fill();
        } else {
            ctx.set_line_width(props.line_width);
            let _ = ctx.stroke();
        }
    }
}

impl Default for EllipseTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for EllipseTool {
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
            Self::draw_ellipse(ctx, start, end, props);
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
            (start.x - end.x).abs() > 2 && (start.y - end.y).abs() > 2
        } else {
            false
        }
    }
}
