//! Annotation state machine and core types.

use gartk_core::{Color, Rect};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

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
