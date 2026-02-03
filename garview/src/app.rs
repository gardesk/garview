use anyhow::{Context, Result};
use gartk_core::{InputEvent, Key, MouseButton, Theme};
use gartk_render::{copy_surface_to_window, Renderer};
use gartk_x11::{Connection, EventLoop, EventLoopConfig, Window, WindowConfig};
use std::path::Path;
use x11rb::protocol::xproto::{self, ConnectionExt, EventMask};

use crate::config::Config;
use crate::ui::{StatusBar, STATUS_BAR_HEIGHT};
use crate::viewer::{ImageViewer, LoadState};

/// Main application state
pub struct App {
    window: Window,
    renderer: Renderer,
    gc: u32,
    viewer: ImageViewer,
    statusbar: StatusBar,
    #[allow(dead_code)]
    config: Config,
    fullscreen: bool,
    needs_redraw: bool,
}

impl App {
    pub fn new(path: Option<String>, fullscreen: bool) -> Result<Self> {
        // Load configuration
        let config = Config::load().unwrap_or_else(|e| {
            tracing::warn!("Failed to load config: {}, using defaults", e);
            Config::default()
        });

        let conn = Connection::connect(None).context("Failed to connect to X11")?;

        let window_config = WindowConfig::new()
            .title("garview")
            .class("garview")
            .size(1280, 720);

        let window = Window::create(conn.clone(), window_config).context("Failed to create window")?;

        // Create GC for rendering
        let gc = conn.generate_id()?;
        conn.inner().create_gc(gc, window.id(), &Default::default())?;
        conn.flush()?;

        let size = window.size();
        let theme = Theme::dark();
        let renderer =
            Renderer::with_theme(size.width, size.height, theme.clone())
                .context("Failed to create renderer")?;

        let statusbar = StatusBar::new(theme);
        let mut viewer = ImageViewer::new();

        // Apply default zoom from config
        match config.general.default_zoom.as_str() {
            "fit" => viewer.zoom.zoom_fit(),
            "fill" => viewer.zoom.zoom_fill(),
            "1:1" | "100%" => viewer.zoom.zoom_one_to_one(),
            _ => viewer.zoom.zoom_fit(),
        }

        // Load initial file if provided
        if let Some(ref path_str) = path {
            let path = Path::new(path_str);
            if path.is_file() {
                if let Err(e) = viewer.load(path) {
                    tracing::error!("Failed to load {}: {}", path.display(), e);
                }
            } else if path.is_dir() {
                // TODO: Gallery mode - load first image in directory
                tracing::info!("Directory mode not yet implemented");
            }
        }

        Ok(Self {
            window,
            renderer,
            gc,
            viewer,
            statusbar,
            config,
            fullscreen,
            needs_redraw: true,
        })
    }

    /// Run the main event loop
    pub fn run(&mut self) -> Result<()> {
        let event_config = EventLoopConfig {
            fps: 60,
            continuous_redraw: false,
        };

        let mut event_loop =
            EventLoop::new(&self.window, event_config).context("Failed to create event loop")?;

        event_loop.run(|event_loop, event| {
            // Handle event (errors are logged, not propagated)
            let should_continue = match self.handle_event(event) {
                Ok(cont) => cont,
                Err(e) => {
                    tracing::error!("Event handling error: {}", e);
                    true // Continue despite error
                }
            };

            // Poll async image loading
            if self.viewer.poll_load() {
                self.needs_redraw = true;
            }

            // Request continuous redraw while loading (for spinner animation)
            if self.viewer.load_state() == LoadState::Loading {
                event_loop.request_redraw();
            }

            // Tick animation
            if self.viewer.is_animated() && self.viewer.tick_animation() {
                event_loop.request_redraw();
            }

            // Render if needed
            if event_loop.needs_redraw() || self.needs_redraw {
                if let Err(e) = self.render() {
                    tracing::error!("Render error: {}", e);
                }
                event_loop.redraw_done();
                self.needs_redraw = false;
            }

            Ok(should_continue)
        })?;

        Ok(())
    }

    fn handle_event(&mut self, event: InputEvent) -> Result<bool> {
        match event {
            InputEvent::CloseRequested => return Ok(false),

            InputEvent::Resize { width, height } => {
                self.renderer.resize(width, height)?;
                self.window.set_size(width, height);
                self.needs_redraw = true;
            }

            InputEvent::Expose => {
                self.needs_redraw = true;
            }

            InputEvent::Key(key_event) if key_event.pressed => {
                match key_event.key {
                    Key::Escape | Key::Char('q') => return Ok(false),

                    // Zoom
                    Key::Char('+') | Key::Char('=') => {
                        self.viewer.zoom.zoom_in();
                        self.needs_redraw = true;
                    }
                    Key::Char('-') => {
                        self.viewer.zoom.zoom_out();
                        self.needs_redraw = true;
                    }
                    Key::Char('0') => {
                        self.viewer.zoom.zoom_one_to_one();
                        self.needs_redraw = true;
                    }
                    Key::Char('f') => {
                        self.viewer.zoom.zoom_fit();
                        self.needs_redraw = true;
                    }
                    Key::Char('F') => {
                        self.viewer.zoom.zoom_fill();
                        self.needs_redraw = true;
                    }

                    // Fullscreen
                    Key::F11 => {
                        if let Err(e) = self.toggle_fullscreen() {
                            tracing::error!("Failed to toggle fullscreen: {}", e);
                        }
                    }

                    // Navigation
                    Key::Right | Key::Char('n') | Key::Space => {
                        if let Err(e) = self.viewer.next_image() {
                            tracing::error!("Failed to load next image: {}", e);
                        }
                        self.needs_redraw = true;
                    }
                    Key::Left | Key::Char('p') => {
                        if let Err(e) = self.viewer.prev_image() {
                            tracing::error!("Failed to load previous image: {}", e);
                        }
                        self.needs_redraw = true;
                    }

                    // Rotation
                    Key::Char('r') => {
                        self.viewer.rotate_cw();
                        self.needs_redraw = true;
                    }
                    Key::Char('R') => {
                        self.viewer.rotate_ccw();
                        self.needs_redraw = true;
                    }

                    // Flip
                    Key::Char('h') => {
                        self.viewer.flip_horizontal();
                        self.needs_redraw = true;
                    }
                    Key::Char('v') => {
                        self.viewer.flip_vertical();
                        self.needs_redraw = true;
                    }

                    // Pan with arrow keys when zoomed
                    Key::Up if key_event.modifiers.is_empty() => {
                        self.viewer.scroll.pan(0.0, -50.0);
                        self.needs_redraw = true;
                    }
                    Key::Down if key_event.modifiers.is_empty() => {
                        self.viewer.scroll.pan(0.0, 50.0);
                        self.needs_redraw = true;
                    }

                    _ => {}
                }
            }

            InputEvent::MousePress(mouse_event) => {
                if mouse_event.button == Some(MouseButton::Left) {
                    self.viewer.scroll.start_drag(mouse_event.position);
                }
            }

            InputEvent::MouseRelease(mouse_event) => {
                if mouse_event.button == Some(MouseButton::Left) {
                    self.viewer.scroll.end_drag();
                }
            }

            InputEvent::MouseMove(mouse_event) => {
                if self.viewer.scroll.is_dragging() {
                    self.viewer.scroll.update_drag(mouse_event.position);
                    self.needs_redraw = true;
                }
            }

            InputEvent::Scroll(scroll_event) => {
                // Zoom with scroll wheel
                if scroll_event.modifiers.ctrl {
                    let factor = if scroll_event.delta_y < 0 { 1.1 } else { 0.9 };
                    let size = self.renderer.size();
                    let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);
                    self.viewer.zoom_at_point(
                        factor,
                        scroll_event.position.x as f64,
                        scroll_event.position.y as f64,
                        size.width as f64,
                        viewport_height as f64,
                    );
                } else {
                    // Pan
                    self.viewer.scroll.pan(
                        scroll_event.delta_x as f64 * 30.0,
                        scroll_event.delta_y as f64 * 30.0,
                    );
                }
                self.needs_redraw = true;
            }

            InputEvent::Idle => {
                // Animation tick happens in run()
            }

            _ => {}
        }

        Ok(true)
    }

    /// Toggle fullscreen mode via EWMH _NET_WM_STATE
    fn toggle_fullscreen(&mut self) -> Result<()> {
        let conn = self.window.connection();
        let atoms = self.window.atoms();

        // _NET_WM_STATE client message: action 2 = toggle
        let event = xproto::ClientMessageEvent::new(
            32,
            self.window.id(),
            atoms.net_wm_state,
            [
                2, // action: toggle
                atoms.net_wm_state_fullscreen,
                0,
                1, // source: application
                0,
            ],
        );

        conn.inner().send_event(
            false,
            conn.root(),
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            event,
        )?;
        conn.flush()?;

        self.fullscreen = !self.fullscreen;
        Ok(())
    }

    fn render(&mut self) -> Result<()> {
        let size = self.renderer.size();
        let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);

        // Clear background
        self.renderer.clear()?;

        // Calculate zoom level based on image and viewport size
        if let Some((img_w, img_h)) = self.viewer.effective_size() {
            self.viewer
                .zoom
                .update(img_w, img_h, size.width as f64, viewport_height as f64);

            let zoom = self.viewer.zoom.level;
            let scaled_w = img_w * zoom;
            let scaled_h = img_h * zoom;

            // Clamp scroll
            self.viewer.scroll.clamp(
                scaled_w,
                scaled_h,
                size.width as f64,
                viewport_height as f64,
            );

            // Get transform state BEFORE rendering (to avoid borrow conflicts)
            let rotation = self.viewer.rotation();
            let flip_h = self.viewer.is_flipped_h();
            let flip_v = self.viewer.is_flipped_v();
            let offset_x = self.viewer.scroll.offset_x;
            let offset_y = self.viewer.scroll.offset_y;

            // Render the image
            if let Ok(image_surface) = self.viewer.render(zoom) {
                // Get actual surface dimensions (may differ from target if zoomed out)
                let surface_w = image_surface.width() as f64;
                let _surface_h = image_surface.height() as f64;

                // Calculate display scale (Cairo will scale the surface to fit)
                let display_scale = scaled_w / surface_w;

                // Calculate position (centered if smaller than viewport)
                let x = if scaled_w < size.width as f64 {
                    (size.width as f64 - scaled_w) / 2.0
                } else {
                    -offset_x
                };

                let y = if scaled_h < viewport_height as f64 {
                    (viewport_height as f64 - scaled_h) / 2.0
                } else {
                    -offset_y
                };

                // Draw image using Cairo
                let ctx = self.renderer.context()?;

                ctx.save()?;
                ctx.translate(x, y);

                // Apply rotation and flip around center
                if rotation != 0 || flip_h || flip_v {
                    ctx.translate(scaled_w / 2.0, scaled_h / 2.0);

                    if rotation != 0 {
                        ctx.rotate(rotation as f64 * std::f64::consts::PI / 180.0);
                    }
                    if flip_h {
                        ctx.scale(-1.0, 1.0);
                    }
                    if flip_v {
                        ctx.scale(1.0, -1.0);
                    }

                    ctx.translate(-scaled_w / 2.0, -scaled_h / 2.0);
                }

                // Scale surface to display size
                ctx.scale(display_scale, display_scale);

                // Paint the image surface with good filtering
                ctx.set_source_surface(image_surface.cairo_surface(), 0.0, 0.0)?;
                // Use GOOD filter for smooth scaling
                ctx.source().set_filter(gartk_render::cairo::Filter::Bilinear);
                ctx.paint()?;

                ctx.restore()?;
            }
        } else {
            // Show loading indicator or placeholder
            let ctx = self.renderer.context()?;
            ctx.select_font_face(
                "sans-serif",
                gartk_render::cairo::FontSlant::Normal,
                gartk_render::cairo::FontWeight::Normal,
            );

            match self.viewer.load_state() {
                LoadState::Loading => {
                    // Draw loading spinner
                    let center_x = size.width as f64 / 2.0;
                    let center_y = viewport_height as f64 / 2.0;
                    let radius = 30.0;

                    // Animate spinner based on elapsed time
                    let elapsed = self.viewer.load_elapsed().unwrap_or_default();
                    let angle = (elapsed.as_millis() as f64 / 100.0) % (2.0 * std::f64::consts::PI);

                    // Draw spinning arc
                    ctx.set_source_rgb(0.6, 0.6, 0.6);
                    ctx.set_line_width(4.0);
                    ctx.arc(center_x, center_y, radius, angle, angle + 1.5 * std::f64::consts::PI);
                    ctx.stroke()?;

                    // Draw "Loading..." text below spinner
                    ctx.set_font_size(14.0);
                    ctx.set_source_rgb(0.5, 0.5, 0.5);
                    let text = "Loading...";
                    let extents = ctx.text_extents(text)?;
                    ctx.move_to(
                        center_x - extents.width() / 2.0,
                        center_y + radius + 30.0,
                    );
                    ctx.show_text(text)?;
                }
                LoadState::Failed => {
                    ctx.set_source_rgb(0.8, 0.3, 0.3);
                    ctx.set_font_size(20.0);
                    let text = "Failed to load image";
                    let extents = ctx.text_extents(text)?;
                    ctx.move_to(
                        (size.width as f64 - extents.width()) / 2.0,
                        (viewport_height as f64 + extents.height()) / 2.0,
                    );
                    ctx.show_text(text)?;
                }
                _ => {
                    // Empty state - show placeholder
                    ctx.set_source_rgb(0.5, 0.5, 0.5);
                    ctx.set_font_size(20.0);
                    let text = "No image loaded. Open a file or drag and drop.";
                    let extents = ctx.text_extents(text)?;
                    ctx.move_to(
                        (size.width as f64 - extents.width()) / 2.0,
                        (viewport_height as f64 + extents.height()) / 2.0,
                    );
                    ctx.show_text(text)?;
                }
            }
        }

        // Render status bar
        self.statusbar.render(
            &self.renderer,
            size.width,
            viewport_height,
            self.viewer.file_info().as_ref(),
            self.viewer.zoom.level,
            self.viewer.zoom.mode,
            self.viewer.directory_position(),
        )?;

        // Copy to window
        self.renderer.flush();
        copy_surface_to_window(self.renderer.surface_mut(), &self.window, self.gc, 0, 0)?;

        Ok(())
    }
}
