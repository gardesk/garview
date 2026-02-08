//! Text tool - add text annotations.

use super::Tool;
use crate::annotate::state::ToolProperties;
use cairo::Context;
use gartk_core::{InputEvent, Key, MouseButton, Point, Rect};
use gartk_x11::CursorShape;

/// Text annotation tool.
pub struct TextTool {
    /// Position where text is placed
    position: Option<Point>,
    /// Text buffer for input
    text: String,
    /// Whether we're actively typing
    typing: bool,
    /// Cursor position within text
    cursor_pos: usize,
    /// Cursor blink state (for animation)
    cursor_visible: bool,
}

impl TextTool {
    pub fn new() -> Self {
        Self {
            position: None,
            text: String::new(),
            typing: false,
            cursor_pos: 0,
            cursor_visible: true,
        }
    }

    /// Draw text at position with optional cursor
    fn draw_text(ctx: &Context, pos: Point, text: &str, props: &ToolProperties, cursor_pos: Option<usize>) {
        ctx.set_source_rgba(
            props.color.r as f64,
            props.color.g as f64,
            props.color.b as f64,
            props.color.a as f64,
        );

        ctx.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        ctx.set_font_size(props.font_size);

        let x = pos.x as f64;
        let y = pos.y as f64;

        // Draw text
        ctx.move_to(x, y);
        let _ = ctx.show_text(text);

        // Draw cursor if position specified
        if let Some(cursor) = cursor_pos {
            let before_cursor = &text[..text.char_indices().nth(cursor).map(|(i, _)| i).unwrap_or(text.len())];

            // Draw cursor line
            let cursor_x = if let Ok(text_extents) = ctx.text_extents(before_cursor) {
                x + text_extents.x_advance()
            } else {
                x
            };

            if let Ok(font_extents) = ctx.font_extents() {
                ctx.set_line_width(1.5);
                ctx.move_to(cursor_x, y - font_extents.ascent());
                ctx.line_to(cursor_x, y + font_extents.descent());
                let _ = ctx.stroke();
            }
        }
    }
}

impl Default for TextTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for TextTool {
    fn handle_event(&mut self, event: &InputEvent, _props: &ToolProperties) -> bool {
        match event {
            InputEvent::MousePress(e) if e.button == Some(MouseButton::Left) => {
                if self.typing {
                    // Click while typing - commit current text and start new
                    self.typing = false;
                    false // Let commit happen, then we'll reset
                } else {
                    // Start typing at click position
                    self.position = Some(e.position);
                    self.text.clear();
                    self.cursor_pos = 0;
                    self.typing = true;
                    true
                }
            }
            InputEvent::Key(key_event) if key_event.pressed && self.typing => {
                match key_event.key {
                    Key::Return => {
                        // Finish typing - will commit
                        self.typing = false;
                        true
                    }
                    Key::Escape => {
                        // Cancel - clear text so it won't commit
                        self.text.clear();
                        self.position = None;
                        self.typing = false;
                        true
                    }
                    Key::Backspace => {
                        if self.cursor_pos > 0 {
                            let chars: Vec<char> = self.text.chars().collect();
                            self.text = chars[..self.cursor_pos - 1].iter().collect::<String>()
                                + &chars[self.cursor_pos..].iter().collect::<String>();
                            self.cursor_pos -= 1;
                        }
                        true
                    }
                    Key::Delete => {
                        let char_count = self.text.chars().count();
                        if self.cursor_pos < char_count {
                            let chars: Vec<char> = self.text.chars().collect();
                            self.text = chars[..self.cursor_pos].iter().collect::<String>()
                                + &chars[self.cursor_pos + 1..].iter().collect::<String>();
                        }
                        true
                    }
                    Key::Left => {
                        if self.cursor_pos > 0 {
                            self.cursor_pos -= 1;
                        }
                        true
                    }
                    Key::Right => {
                        if self.cursor_pos < self.text.chars().count() {
                            self.cursor_pos += 1;
                        }
                        true
                    }
                    Key::Home => {
                        self.cursor_pos = 0;
                        true
                    }
                    Key::End => {
                        self.cursor_pos = self.text.chars().count();
                        true
                    }
                    Key::Char(c) => {
                        // Insert character at cursor position
                        let chars: Vec<char> = self.text.chars().collect();
                        self.text = chars[..self.cursor_pos].iter().collect::<String>()
                            + &c.to_string()
                            + &chars[self.cursor_pos..].iter().collect::<String>();
                        self.cursor_pos += 1;
                        true
                    }
                    Key::Space => {
                        let chars: Vec<char> = self.text.chars().collect();
                        self.text = chars[..self.cursor_pos].iter().collect::<String>()
                            + " "
                            + &chars[self.cursor_pos..].iter().collect::<String>();
                        self.cursor_pos += 1;
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn draw_preview(&self, ctx: &Context, props: &ToolProperties) {
        if let Some(pos) = self.position {
            // Show text with cursor when typing
            let cursor = if self.typing { Some(self.cursor_pos) } else { None };
            Self::draw_text(ctx, pos, &self.text, props, cursor);
        }
    }

    fn commit(&self, ctx: &Context, props: &ToolProperties) {
        if let Some(pos) = self.position {
            // Commit text without cursor
            Self::draw_text(ctx, pos, &self.text, props, None);
        }
    }

    fn reset(&mut self) {
        self.position = None;
        self.text.clear();
        self.typing = false;
        self.cursor_pos = 0;
    }

    fn cursor(&self) -> CursorShape {
        CursorShape::Text
    }

    fn is_drawing(&self) -> bool {
        self.typing
    }

    fn can_commit(&self) -> bool {
        self.position.is_some() && !self.text.is_empty() && !self.typing
    }

    fn bounds(&self) -> Option<Rect> {
        // Text bounds are approximate - would need Cairo context to measure properly
        if let Some(pos) = self.position {
            let char_width = 10; // Approximate
            let height = 20; // Approximate
            let width = (self.text.chars().count() * char_width) as u32;
            Some(Rect::new(pos.x, pos.y - height as i32, width.max(1), height))
        } else {
            None
        }
    }
}
