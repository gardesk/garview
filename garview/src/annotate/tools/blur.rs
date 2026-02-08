//! Blur tool - blur rectangular regions.

use super::Tool;
use crate::annotate::state::ToolProperties;
use cairo::Context;
use gartk_core::{InputEvent, MouseButton, Point, Rect};
use gartk_x11::CursorShape;

/// Blur selection tool.
///
/// This tool selects a rectangular region that will be blurred.
/// The actual blur operation is handled by AnnotationManager since
/// it requires access to the underlying image data.
pub struct BlurTool {
    start: Option<Point>,
    end: Option<Point>,
    drawing: bool,
}

impl BlurTool {
    pub fn new() -> Self {
        Self {
            start: None,
            end: None,
            drawing: false,
        }
    }

    /// Get the blur region bounds.
    pub fn blur_bounds(&self) -> Option<(i32, i32, u32, u32)> {
        if let (Some(start), Some(end)) = (self.start, self.end) {
            let x = start.x.min(end.x);
            let y = start.y.min(end.y);
            let w = (start.x - end.x).abs() as u32;
            let h = (start.y - end.y).abs() as u32;
            if w > 2 && h > 2 {
                return Some((x, y, w, h));
            }
        }
        None
    }

    fn draw_selection(ctx: &Context, start: Point, end: Point) {
        let x = start.x.min(end.x) as f64;
        let y = start.y.min(end.y) as f64;
        let w = (start.x - end.x).abs() as f64;
        let h = (start.y - end.y).abs() as f64;

        // Draw dashed selection rectangle (no fill)
        ctx.set_source_rgba(0.2, 0.2, 0.2, 0.8);
        ctx.set_line_width(2.0);
        ctx.set_dash(&[5.0, 5.0], 0.0);
        ctx.rectangle(x, y, w, h);
        let _ = ctx.stroke();

        // Reset dash
        ctx.set_dash(&[], 0.0);
    }
}

impl Default for BlurTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for BlurTool {
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

    fn draw_preview(&self, ctx: &Context, _props: &ToolProperties) {
        if let (Some(start), Some(end)) = (self.start, self.end) {
            Self::draw_selection(ctx, start, end);
        }
    }

    fn commit(&self, _ctx: &Context, _props: &ToolProperties) {
        // Actual blur is handled by AnnotationManager
        // This is a no-op because we need access to image data
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
        self.blur_bounds().is_some()
    }

    fn bounds(&self) -> Option<Rect> {
        if let (Some(start), Some(end)) = (self.start, self.end) {
            let x = start.x.min(end.x);
            let y = start.y.min(end.y);
            let w = (start.x - end.x).abs() as u32;
            let h = (start.y - end.y).abs() as u32;
            Some(Rect::new(x, y, w.max(1), h.max(1)))
        } else {
            None
        }
    }
}

/// Apply a box blur to RGBA pixel data in-place.
///
/// This is a simple box blur that averages pixels within a radius.
/// For larger radii, multiple passes create a decent approximation of Gaussian blur.
pub fn box_blur(data: &mut [u8], width: u32, height: u32, radius: usize) {
    if radius == 0 || width == 0 || height == 0 {
        return;
    }

    let w = width as usize;
    let h = height as usize;

    // Multiple passes for better blur quality
    let passes = 3;
    let pass_radius = (radius / passes).max(1);

    for _ in 0..passes {
        // Horizontal pass
        let mut temp = data.to_vec();
        for y in 0..h {
            for x in 0..w {
                let mut r_sum: u32 = 0;
                let mut g_sum: u32 = 0;
                let mut b_sum: u32 = 0;
                let mut a_sum: u32 = 0;
                let mut count: u32 = 0;

                let x_start = x.saturating_sub(pass_radius);
                let x_end = (x + pass_radius + 1).min(w);

                for sx in x_start..x_end {
                    let idx = (y * w + sx) * 4;
                    r_sum += data[idx] as u32;
                    g_sum += data[idx + 1] as u32;
                    b_sum += data[idx + 2] as u32;
                    a_sum += data[idx + 3] as u32;
                    count += 1;
                }

                let idx = (y * w + x) * 4;
                temp[idx] = (r_sum / count) as u8;
                temp[idx + 1] = (g_sum / count) as u8;
                temp[idx + 2] = (b_sum / count) as u8;
                temp[idx + 3] = (a_sum / count) as u8;
            }
        }

        // Vertical pass
        for y in 0..h {
            for x in 0..w {
                let mut r_sum: u32 = 0;
                let mut g_sum: u32 = 0;
                let mut b_sum: u32 = 0;
                let mut a_sum: u32 = 0;
                let mut count: u32 = 0;

                let y_start = y.saturating_sub(pass_radius);
                let y_end = (y + pass_radius + 1).min(h);

                for sy in y_start..y_end {
                    let idx = (sy * w + x) * 4;
                    r_sum += temp[idx] as u32;
                    g_sum += temp[idx + 1] as u32;
                    b_sum += temp[idx + 2] as u32;
                    a_sum += temp[idx + 3] as u32;
                    count += 1;
                }

                let idx = (y * w + x) * 4;
                data[idx] = (r_sum / count) as u8;
                data[idx + 1] = (g_sum / count) as u8;
                data[idx + 2] = (b_sum / count) as u8;
                data[idx + 3] = (a_sum / count) as u8;
            }
        }
    }
}
