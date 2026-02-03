use gartk_core::{Color, Rect, Theme};
use gartk_render::{Renderer, TextAlign, TextStyle};

use crate::viewer::{FileInfo, ZoomMode};

/// Status bar height in pixels
pub const STATUS_BAR_HEIGHT: u32 = 24;

/// Status bar displaying file info and zoom level
pub struct StatusBar {
    height: u32,
    theme: Theme,
}

impl StatusBar {
    pub fn new(theme: Theme) -> Self {
        Self {
            height: STATUS_BAR_HEIGHT,
            theme,
        }
    }

    #[allow(dead_code)]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Render the status bar
    pub fn render(
        &self,
        renderer: &Renderer,
        width: u32,
        y: u32,
        file_info: Option<&FileInfo>,
        zoom_level: f64,
        zoom_mode: ZoomMode,
        position: Option<(usize, usize)>,
    ) -> anyhow::Result<()> {
        let rect = Rect::new(0, y as i32, width, self.height);

        // Background
        let bg_color = Color::new(0.1, 0.1, 0.12, 0.95);
        renderer.fill_rect(rect, bg_color)?;

        // Separator line at top
        renderer.line(
            0.0,
            y as f64,
            width as f64,
            y as f64,
            Color::new(0.2, 0.2, 0.25, 1.0),
            1.0,
        )?;

        let text_style = TextStyle::new()
            .font_family(&self.theme.font_family)
            .font_size(12.0)
            .color(self.theme.foreground);

        let padding = 8.0;
        // Center text vertically: top-left at y + (height - text_height) / 2
        // Approximate text height ~14px for 12pt font
        let text_y = y as f64 + (self.height as f64 - 14.0) / 2.0;

        // Left side: file info
        if let Some(info) = file_info {
            let left_text = format!(
                "{} - {}x{} - {} - {}",
                info.filename,
                info.width,
                info.height,
                info.format,
                info.format_size()
            );
            renderer.text(&left_text, padding, text_y, &text_style)?;
        }

        // Right side: zoom and position
        let zoom_text = match zoom_mode {
            ZoomMode::Fit => "Fit".to_string(),
            ZoomMode::Fill => "Fill".to_string(),
            ZoomMode::OneToOne => "100%".to_string(),
            ZoomMode::Custom(_) => format!("{:.0}%", zoom_level * 100.0),
        };

        let right_text = if let Some((current, total)) = position {
            format!("{} | {} / {}", zoom_text, current, total)
        } else {
            zoom_text
        };

        let right_style = text_style.align(TextAlign::Right);
        renderer.text(&right_text, width as f64 - padding, text_y, &right_style)?;

        Ok(())
    }
}
