use gartk_core::Point;

/// Scroll/pan state for the viewer
pub struct ScrollState {
    /// Current scroll offset (in image coordinates)
    pub offset_x: f64,
    pub offset_y: f64,
    /// Drag state
    is_dragging: bool,
    drag_start: Option<Point>,
    drag_offset_start: (f64, f64),
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            offset_x: 0.0,
            offset_y: 0.0,
            is_dragging: false,
            drag_start: None,
            drag_offset_start: (0.0, 0.0),
        }
    }
}

impl ScrollState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset scroll to center
    pub fn reset(&mut self) {
        self.offset_x = 0.0;
        self.offset_y = 0.0;
    }

    /// Pan by a delta
    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.offset_x += dx;
        self.offset_y += dy;
    }

    /// Start a drag operation
    pub fn start_drag(&mut self, pos: Point) {
        self.is_dragging = true;
        self.drag_start = Some(pos);
        self.drag_offset_start = (self.offset_x, self.offset_y);
    }

    /// Update during drag
    pub fn update_drag(&mut self, pos: Point) {
        if let Some(start) = self.drag_start {
            let dx = (pos.x - start.x) as f64;
            let dy = (pos.y - start.y) as f64;
            self.offset_x = self.drag_offset_start.0 - dx;
            self.offset_y = self.drag_offset_start.1 - dy;
        }
    }

    /// End drag operation
    pub fn end_drag(&mut self) {
        self.is_dragging = false;
        self.drag_start = None;
    }

    /// Check if currently dragging
    pub fn is_dragging(&self) -> bool {
        self.is_dragging
    }

    /// Clamp scroll offset to valid range
    pub fn clamp(&mut self, content_width: f64, content_height: f64, viewport_width: f64, viewport_height: f64) {
        // Allow scrolling if content is larger than viewport
        let max_x = (content_width - viewport_width).max(0.0);
        let max_y = (content_height - viewport_height).max(0.0);

        // Center if content is smaller than viewport
        if content_width <= viewport_width {
            self.offset_x = -(viewport_width - content_width) / 2.0;
        } else {
            self.offset_x = self.offset_x.clamp(0.0, max_x);
        }

        if content_height <= viewport_height {
            self.offset_y = -(viewport_height - content_height) / 2.0;
        } else {
            self.offset_y = self.offset_y.clamp(0.0, max_y);
        }
    }
}
