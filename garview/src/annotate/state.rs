//! Annotation state machine and core types.

use gartk_core::{Color, Rect};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Calculate the shortest distance from a point to a line segment.
fn point_to_line_distance(px: i32, py: i32, x1: i32, y1: i32, x2: i32, y2: i32) -> f64 {
    let px = px as f64;
    let py = py as f64;
    let x1 = x1 as f64;
    let y1 = y1 as f64;
    let x2 = x2 as f64;
    let y2 = y2 as f64;

    let dx = x2 - x1;
    let dy = y2 - y1;
    let len_sq = dx * dx + dy * dy;

    if len_sq < 0.0001 {
        // Line segment is essentially a point
        return ((px - x1).powi(2) + (py - y1).powi(2)).sqrt();
    }

    // Project point onto line, clamped to segment
    let t = ((px - x1) * dx + (py - y1) * dy) / len_sq;
    let t = t.clamp(0.0, 1.0);

    // Closest point on segment
    let closest_x = x1 + t * dx;
    let closest_y = y1 + t * dy;

    // Distance to closest point
    ((px - closest_x).powi(2) + (py - closest_y).powi(2)).sqrt()
}

/// Serializable annotation record for JSON persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableAnnotation {
    pub id: u64,
    pub tool: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub color: [f64; 4], // RGBA
    pub page: usize,
    pub timestamp: u64, // Seconds since epoch
    /// Start point for line-based tools (x, y).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_point: Option<(i32, i32)>,
    /// End point for line-based tools (x, y).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_point: Option<(i32, i32)>,
}

impl SerializableAnnotation {
    /// Convert from AnnotationRecord.
    pub fn from_record(record: &AnnotationRecord) -> Self {
        let timestamp = record.timestamp
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            id: record.id,
            tool: record.tool.name().to_string(),
            x: record.bounds.x,
            y: record.bounds.y,
            width: record.bounds.width,
            height: record.bounds.height,
            color: [record.color.r, record.color.g, record.color.b, record.color.a],
            page: record.page,
            timestamp,
            start_point: record.start_point,
            end_point: record.end_point,
        }
    }

    /// Convert to AnnotationRecord.
    pub fn to_record(&self) -> AnnotationRecord {
        let tool = ToolType::from_name(&self.tool).unwrap_or_default();
        let bounds = Rect::new(self.x, self.y, self.width, self.height);
        let color = Color::new(self.color[0], self.color[1], self.color[2], self.color[3]);
        let timestamp = UNIX_EPOCH + std::time::Duration::from_secs(self.timestamp);
        AnnotationRecord {
            id: self.id,
            tool,
            bounds,
            color,
            page: self.page,
            timestamp,
            start_point: self.start_point,
            end_point: self.end_point,
        }
    }
}

/// Record of a committed annotation.
#[derive(Debug, Clone)]
pub struct AnnotationRecord {
    /// Unique identifier.
    pub id: u64,
    /// Tool used.
    pub tool: ToolType,
    /// Bounding box in image coordinates.
    pub bounds: Rect,
    /// Color used.
    pub color: Color,
    /// Page number (for multi-page documents).
    pub page: usize,
    /// When this annotation was created.
    pub timestamp: SystemTime,
    /// Start point for line-based tools (Arrow, Line).
    pub start_point: Option<(i32, i32)>,
    /// End point for line-based tools (Arrow, Line).
    pub end_point: Option<(i32, i32)>,
}

impl AnnotationRecord {
    /// Create a new annotation record.
    pub fn new(id: u64, tool: ToolType, bounds: Rect, color: Color, page: usize) -> Self {
        Self {
            id,
            tool,
            bounds,
            color,
            page,
            timestamp: SystemTime::now(),
            start_point: None,
            end_point: None,
        }
    }

    /// Create a new annotation record with line endpoints.
    pub fn new_with_endpoints(
        id: u64,
        tool: ToolType,
        bounds: Rect,
        color: Color,
        page: usize,
        start: (i32, i32),
        end: (i32, i32),
    ) -> Self {
        Self {
            id,
            tool,
            bounds,
            color,
            page,
            timestamp: SystemTime::now(),
            start_point: Some(start),
            end_point: Some(end),
        }
    }

    /// Check if a point is near this annotation (for selection).
    /// Uses geometry-aware hit-testing for line-based tools.
    pub fn contains_point(&self, x: i32, y: i32, tolerance: f64) -> bool {
        match self.tool {
            ToolType::Arrow | ToolType::Line => {
                // For lines, check distance to line segment
                if let (Some((x1, y1)), Some((x2, y2))) = (self.start_point, self.end_point) {
                    let dist = point_to_line_distance(x, y, x1, y1, x2, y2);
                    dist <= tolerance
                } else {
                    // Fallback to bounding box if no endpoints stored
                    self.bounds.contains_point(gartk_core::Point::new(x, y))
                }
            }
            ToolType::Rectangle => {
                // For rectangles, check if near edges (within tolerance of border)
                let b = &self.bounds;
                let in_bounds = x >= b.x - tolerance as i32
                    && x <= b.x + b.width as i32 + tolerance as i32
                    && y >= b.y - tolerance as i32
                    && y <= b.y + b.height as i32 + tolerance as i32;
                if !in_bounds {
                    return false;
                }
                // Check if near any edge
                let near_left = (x - b.x).abs() <= tolerance as i32;
                let near_right = (x - (b.x + b.width as i32)).abs() <= tolerance as i32;
                let near_top = (y - b.y).abs() <= tolerance as i32;
                let near_bottom = (y - (b.y + b.height as i32)).abs() <= tolerance as i32;
                near_left || near_right || near_top || near_bottom
            }
            ToolType::Ellipse => {
                // For ellipses, check distance to ellipse curve
                let b = &self.bounds;
                let cx = b.x as f64 + b.width as f64 / 2.0;
                let cy = b.y as f64 + b.height as f64 / 2.0;
                let rx = b.width as f64 / 2.0;
                let ry = b.height as f64 / 2.0;
                if rx < 1.0 || ry < 1.0 {
                    return false;
                }
                // Normalized distance from center (1.0 = on ellipse)
                let dx = (x as f64 - cx) / rx;
                let dy = (y as f64 - cy) / ry;
                let dist = (dx * dx + dy * dy).sqrt();
                // Check if near the ellipse curve (not inside or far outside)
                (dist - 1.0).abs() * rx.min(ry) <= tolerance
            }
            // For other tools (Text, Blur, Brush, Highlight), use bounding box
            _ => self.bounds.contains_point(gartk_core::Point::new(x, y)),
        }
    }

    /// Get a short description for display.
    pub fn description(&self) -> String {
        format!("{} on page {}", self.tool.name(), self.page + 1)
    }
}

/// Tool type for annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolType {
    #[default]
    Arrow,
    Brush,
    Line,
    Rectangle,
    Ellipse,
    Text,
    Blur,
    Highlight,
}

impl ToolType {
    /// Get the keyboard shortcut for this tool.
    pub fn shortcut(&self) -> char {
        match self {
            ToolType::Brush => 'b',
            ToolType::Line => 'l',
            ToolType::Arrow => 'a',
            ToolType::Rectangle => 'r',
            ToolType::Ellipse => 'e',
            ToolType::Text => 't',
            ToolType::Blur => 'x',
            ToolType::Highlight => 'h',
        }
    }

    /// Get tool type from keyboard shortcut.
    pub fn from_shortcut(c: char) -> Option<Self> {
        match c.to_ascii_lowercase() {
            'b' => Some(ToolType::Brush),
            'l' => Some(ToolType::Line),
            'a' => Some(ToolType::Arrow),
            'r' => Some(ToolType::Rectangle),
            'e' => Some(ToolType::Ellipse),
            't' => Some(ToolType::Text),
            'x' => Some(ToolType::Blur),
            'h' => Some(ToolType::Highlight),
            _ => None,
        }
    }

    /// Get tool type from name string.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Brush" => Some(ToolType::Brush),
            "Line" => Some(ToolType::Line),
            "Arrow" => Some(ToolType::Arrow),
            "Rectangle" => Some(ToolType::Rectangle),
            "Ellipse" => Some(ToolType::Ellipse),
            "Text" => Some(ToolType::Text),
            "Blur" => Some(ToolType::Blur),
            "Highlight" => Some(ToolType::Highlight),
            _ => None,
        }
    }

    /// Get display name for this tool.
    pub fn name(&self) -> &'static str {
        match self {
            ToolType::Brush => "Brush",
            ToolType::Line => "Line",
            ToolType::Arrow => "Arrow",
            ToolType::Rectangle => "Rectangle",
            ToolType::Ellipse => "Ellipse",
            ToolType::Text => "Text",
            ToolType::Blur => "Blur",
            ToolType::Highlight => "Highlight",
        }
    }

    /// Get all tool types in order.
    pub fn all() -> &'static [ToolType] {
        &[
            ToolType::Brush,
            ToolType::Line,
            ToolType::Arrow,
            ToolType::Rectangle,
            ToolType::Ellipse,
            ToolType::Text,
            ToolType::Blur,
            ToolType::Highlight,
        ]
    }
}

/// Tool properties for drawing.
#[derive(Debug, Clone)]
pub struct ToolProperties {
    /// Stroke/fill color.
    pub color: Color,
    /// Line width in pixels.
    pub line_width: f64,
    /// Whether to fill shapes (vs stroke only).
    pub fill: bool,
    /// Blur radius for blur tool.
    pub blur_radius: usize,
    /// Font size for text tool.
    pub font_size: f64,
}

impl Default for ToolProperties {
    fn default() -> Self {
        Self {
            color: Color::new(1.0, 0.4, 0.0, 1.0), // Orange (#ff6600)
            line_width: 3.0,
            fill: false,
            blur_radius: 15,
            font_size: 16.0,
        }
    }
}

/// Preset colors for quick selection (keys 1-9).
const PRESET_COLORS: [Color; 9] = [
    Color::new(1.0, 0.0, 0.0, 1.0),   // 1: Red
    Color::new(1.0, 0.4, 0.0, 1.0),   // 2: Orange
    Color::new(1.0, 1.0, 0.0, 1.0),   // 3: Yellow
    Color::new(0.0, 1.0, 0.0, 1.0),   // 4: Green
    Color::new(0.0, 1.0, 1.0, 1.0),   // 5: Cyan
    Color::new(0.0, 0.0, 1.0, 1.0),   // 6: Blue
    Color::new(1.0, 0.0, 1.0, 1.0),   // 7: Magenta
    Color::new(1.0, 1.0, 1.0, 1.0),   // 8: White
    Color::new(0.0, 0.0, 0.0, 1.0),   // 9: Black
];

impl ToolProperties {
    /// Preset colors for quick selection (keys 1-9).
    pub fn preset_colors() -> &'static [Color] {
        &PRESET_COLORS
    }

    /// Increase line width.
    pub fn increase_line_width(&mut self) {
        self.line_width = (self.line_width + 1.0).min(50.0);
    }

    /// Decrease line width.
    pub fn decrease_line_width(&mut self) {
        self.line_width = (self.line_width - 1.0).max(1.0);
    }
}

/// Main annotation state.
pub struct AnnotationState {
    /// Current tool.
    pub current_tool: ToolType,
    /// Tool properties.
    pub properties: ToolProperties,
    /// Whether annotation mode is active.
    pub active: bool,
}

impl AnnotationState {
    /// Create new annotation state.
    pub fn new() -> Self {
        Self {
            current_tool: ToolType::Arrow,
            properties: ToolProperties::default(),
            active: false,
        }
    }

    /// Select a tool.
    pub fn select_tool(&mut self, tool: ToolType) {
        self.current_tool = tool;
    }

    /// Set color from preset index (0-8).
    pub fn set_color_preset(&mut self, index: usize) {
        let colors = ToolProperties::preset_colors();
        if index < colors.len() {
            self.properties.color = colors[index];
        }
    }

    /// Toggle fill mode.
    pub fn toggle_fill(&mut self) {
        self.properties.fill = !self.properties.fill;
    }
}

impl Default for AnnotationState {
    fn default() -> Self {
        Self::new()
    }
}
