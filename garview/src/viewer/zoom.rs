/// Zoom mode for the viewer
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ZoomMode {
    /// Fit entire image in viewport
    Fit,
    /// Fill viewport (may crop)
    Fill,
    /// 100% zoom (1:1 pixels)
    OneToOne,
    /// Custom zoom level
    Custom(f64),
}

/// Zoom state management
pub struct ZoomState {
    pub mode: ZoomMode,
    pub level: f64,
    pub min_level: f64,
    pub max_level: f64,
}

impl Default for ZoomState {
    fn default() -> Self {
        Self {
            mode: ZoomMode::Fit,
            level: 1.0,
            min_level: 0.05,
            max_level: 20.0,
        }
    }
}

impl ZoomState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Calculate the effective zoom level for given image and viewport sizes
    pub fn calculate_level(&self, image_width: f64, image_height: f64, viewport_width: f64, viewport_height: f64) -> f64 {
        match self.mode {
            ZoomMode::Fit => {
                let scale_x = viewport_width / image_width;
                let scale_y = viewport_height / image_height;
                scale_x.min(scale_y)
            }
            ZoomMode::Fill => {
                let scale_x = viewport_width / image_width;
                let scale_y = viewport_height / image_height;
                scale_x.max(scale_y)
            }
            ZoomMode::OneToOne => 1.0,
            ZoomMode::Custom(level) => level,
        }
    }

    /// Update the actual zoom level based on mode and dimensions
    pub fn update(&mut self, image_width: f64, image_height: f64, viewport_width: f64, viewport_height: f64) {
        self.level = self.calculate_level(image_width, image_height, viewport_width, viewport_height);
        self.level = self.level.clamp(self.min_level, self.max_level);
    }

    /// Zoom in by a step
    pub fn zoom_in(&mut self) {
        let new_level = (self.level * 1.25).min(self.max_level);
        self.mode = ZoomMode::Custom(new_level);
        self.level = new_level;
    }

    /// Zoom out by a step
    pub fn zoom_out(&mut self) {
        let new_level = (self.level / 1.25).max(self.min_level);
        self.mode = ZoomMode::Custom(new_level);
        self.level = new_level;
    }

    /// Set zoom to fit
    pub fn zoom_fit(&mut self) {
        self.mode = ZoomMode::Fit;
    }

    /// Set zoom to 100%
    pub fn zoom_one_to_one(&mut self) {
        self.mode = ZoomMode::OneToOne;
        self.level = 1.0;
    }

    /// Set a specific zoom level
    pub fn set_level(&mut self, level: f64) {
        let level = level.clamp(self.min_level, self.max_level);
        self.mode = ZoomMode::Custom(level);
        self.level = level;
    }

    /// Zoom at a specific point (for mouse wheel zoom)
    pub fn zoom_at_point(&mut self, factor: f64, _point_x: f64, _point_y: f64) {
        let new_level = (self.level * factor).clamp(self.min_level, self.max_level);
        self.mode = ZoomMode::Custom(new_level);
        self.level = new_level;
    }
}
