use anyhow::{Context, Result};
use gartk_core::{InputEvent, Key, MouseButton, Theme};
use gartk_render::{copy_surface_to_window, Renderer};
use gartk_x11::{Connection, EventLoop, EventLoopConfig, Window, WindowConfig};
use std::path::Path;
use std::time::{Duration, Instant};
use x11rb::protocol::xproto::{self, ConnectionExt, EventMask};

use crate::config::Config;
use crate::ui::{Sidebar, StatusBar, ThumbnailData, STATUS_BAR_HEIGHT, SIDEBAR_WIDTH};
use crate::backend::LinkDestination;
use crate::viewer::{GalleryView, ImageViewer, LoadState, SortOrder};

/// Current view mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    /// Single image viewer
    Image,
    /// Gallery/thumbnail grid
    Gallery,
}

/// Slideshow state
struct SlideshowState {
    active: bool,
    interval: Duration,
    last_advance: Instant,
}

impl Default for SlideshowState {
    fn default() -> Self {
        Self {
            active: false,
            interval: Duration::from_secs(3),
            last_advance: Instant::now(),
        }
    }
}

/// Main application state
pub struct App {
    window: Window,
    renderer: Renderer,
    gc: u32,
    viewer: ImageViewer,
    gallery: Option<GalleryView>,
    mode: ViewMode,
    slideshow: SlideshowState,
    statusbar: StatusBar,
    sidebar: Sidebar,
    #[allow(dead_code)]
    config: Config,
    fullscreen: bool,
    needs_redraw: bool,
    /// Search mode active
    search_active: bool,
    /// Current search input
    search_input: String,
    /// Text selection in progress
    selecting_text: bool,
    /// Go to page dialog active
    goto_page_active: bool,
    /// Go to page input
    goto_page_input: String,
}

impl App {
    pub fn new(path: Option<String>, fullscreen: bool, slideshow: bool) -> Result<Self> {
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
        let sidebar = Sidebar::new();
        let mut viewer = ImageViewer::new();
        let mut gallery: Option<GalleryView> = None;
        let mut mode = ViewMode::Image;

        // Apply default zoom from config
        match config.general.default_zoom.as_str() {
            "fit" => viewer.zoom.zoom_fit(),
            "fill" => viewer.zoom.zoom_fill(),
            "1:1" | "100%" => viewer.zoom.zoom_one_to_one(),
            _ => viewer.zoom.zoom_fit(),
        }

        // Load initial file or directory if provided
        if let Some(ref path_str) = path {
            let path = Path::new(path_str);
            if path.is_file() {
                if let Err(e) = viewer.load(path) {
                    tracing::error!("Failed to load {}: {}", path.display(), e);
                }
            } else if path.is_dir() {
                // Gallery mode
                match GalleryView::new() {
                    Ok(mut gal) => {
                        if let Err(e) = gal.open(path) {
                            tracing::error!("Failed to open directory: {}", e);
                        } else {
                            mode = ViewMode::Gallery;
                            gallery = Some(gal);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to create gallery: {}", e);
                    }
                }
            }
        }

        let mut slideshow_state = SlideshowState::default();
        if slideshow {
            slideshow_state.active = true;
        }

        Ok(Self {
            window,
            renderer,
            gc,
            viewer,
            gallery,
            mode,
            slideshow: slideshow_state,
            statusbar,
            sidebar,
            config,
            fullscreen,
            needs_redraw: true,
            search_active: false,
            search_input: String::new(),
            selecting_text: false,
            goto_page_active: false,
            goto_page_input: String::new(),
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

            // Poll based on current mode
            match self.mode {
                ViewMode::Image => {
                    // Poll async image loading
                    if self.viewer.poll_load() {
                        self.needs_redraw = true;
                        // Reset slideshow timer on new image
                        if self.slideshow.active {
                            self.slideshow.last_advance = Instant::now();
                        }
                    }

                    // Request continuous redraw while loading (for spinner animation)
                    if self.viewer.load_state() == LoadState::Loading {
                        event_loop.request_redraw();
                    }

                    // Tick animation
                    if self.viewer.is_animated() && self.viewer.tick_animation() {
                        event_loop.request_redraw();
                    }

                    // Slideshow auto-advance
                    if self.slideshow.active
                        && self.viewer.load_state() == LoadState::Ready
                        && self.slideshow.last_advance.elapsed() >= self.slideshow.interval
                    {
                        if let Err(e) = self.viewer.next_image() {
                            tracing::debug!("Slideshow advance: {}", e);
                        }
                        self.slideshow.last_advance = Instant::now();
                        self.needs_redraw = true;
                    }

                    // Request redraw for slideshow timing
                    if self.slideshow.active {
                        event_loop.request_redraw();
                    }
                }
                ViewMode::Gallery => {
                    // Poll thumbnail generation
                    if let Some(ref mut gallery) = self.gallery {
                        if gallery.poll_thumbnails() {
                            self.needs_redraw = true;
                        }
                    }
                }
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
                // Handle search input mode first
                if self.search_active {
                    match key_event.key {
                        Key::Escape => {
                            self.search_active = false;
                            self.viewer.clear_search();
                            self.needs_redraw = true;
                        }
                        Key::Return => {
                            // Execute search
                            let count = self.viewer.search(&self.search_input);
                            tracing::info!("Found {} results for '{}'", count, self.search_input);
                            self.search_active = false;
                            self.needs_redraw = true;
                        }
                        Key::Backspace => {
                            self.search_input.pop();
                            self.needs_redraw = true;
                        }
                        Key::Char(c) => {
                            self.search_input.push(c);
                            self.needs_redraw = true;
                        }
                        _ => {}
                    }
                    return Ok(true);
                }

                // Go to page dialog input
                if self.goto_page_active {
                    match key_event.key {
                        Key::Escape => {
                            self.goto_page_active = false;
                            self.needs_redraw = true;
                        }
                        Key::Return => {
                            // Go to the entered page
                            if let Ok(page_num) = self.goto_page_input.parse::<usize>() {
                                // Convert 1-based user input to 0-based index
                                if page_num > 0 && page_num <= self.viewer.page_count() {
                                    self.viewer.goto_page(page_num - 1);
                                    // Update sidebar if visible
                                    if self.sidebar.visible {
                                        self.sidebar.selected_page = Some(page_num - 1);
                                        let size = self.renderer.size();
                                        let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);
                                        self.sidebar.scroll_to_page(page_num - 1, viewport_height);
                                    }
                                }
                            }
                            self.goto_page_active = false;
                            self.needs_redraw = true;
                        }
                        Key::Backspace => {
                            self.goto_page_input.pop();
                            self.needs_redraw = true;
                        }
                        Key::Char(c) if c.is_ascii_digit() => {
                            self.goto_page_input.push(c);
                            self.needs_redraw = true;
                        }
                        _ => {}
                    }
                    return Ok(true);
                }

                // Global keys
                match key_event.key {
                    Key::Escape => {
                        // Clear selection first, then search, then quit
                        if self.viewer.selection.text.is_some() || self.viewer.selection.active {
                            self.viewer.clear_selection();
                            self.needs_redraw = true;
                        } else if !self.viewer.search.results.is_empty() {
                            self.viewer.clear_search();
                            self.needs_redraw = true;
                        } else {
                            return Ok(false); // Quit
                        }
                    }
                    Key::Char('q') => return Ok(false),
                    // Copy selected text
                    Key::Char('c') if key_event.modifiers.ctrl => {
                        if let Some(text) = self.viewer.selected_text() {
                            if let Err(e) = self.copy_to_clipboard(text) {
                                tracing::error!("Failed to copy to clipboard: {}", e);
                            } else {
                                tracing::info!("Copied {} chars to clipboard", text.len());
                            }
                        }
                        return Ok(true);
                    }
                    Key::F11 => {
                        if let Err(e) = self.toggle_fullscreen() {
                            tracing::error!("Failed to toggle fullscreen: {}", e);
                        }
                        return Ok(true);
                    }
                    // Toggle sidebar
                    Key::F7 => {
                        self.sidebar.toggle();
                        // Update sidebar page count when showing
                        if self.sidebar.visible && self.viewer.is_multipage() {
                            let current_page = self.viewer.current_page();
                            self.sidebar.set_page_count(self.viewer.page_count());
                            self.sidebar.selected_page = Some(current_page);
                            // Scroll to show current page
                            let size = self.renderer.size();
                            let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);
                            self.sidebar.scroll_to_page(current_page, viewport_height);
                            // Generate thumbnails for visible pages
                            self.generate_sidebar_thumbnails();
                            // Populate table of contents if available
                            if self.viewer.has_toc() {
                                let toc_entries = self.viewer.get_toc();
                                let toc: Vec<crate::ui::TocEntry> = toc_entries
                                    .into_iter()
                                    .map(|(title, page, level)| crate::ui::TocEntry {
                                        title,
                                        page,
                                        level,
                                    })
                                    .collect();
                                self.sidebar.set_toc(toc);
                            }
                        }
                        self.needs_redraw = true;
                        return Ok(true);
                    }
                    // Search shortcuts
                    Key::Char('f') if key_event.modifiers.ctrl => {
                        if self.viewer.supports_search() {
                            self.search_active = true;
                            self.search_input.clear();
                            self.needs_redraw = true;
                        }
                        return Ok(true);
                    }
                    // Go to page (Ctrl+G)
                    Key::Char('g') if key_event.modifiers.ctrl => {
                        if self.viewer.is_multipage() {
                            self.goto_page_active = true;
                            self.goto_page_input.clear();
                            self.needs_redraw = true;
                        }
                        return Ok(true);
                    }
                    Key::Tab | Key::Char('g') => {
                        // Toggle between image and gallery mode
                        self.toggle_view_mode();
                        return Ok(true);
                    }
                    Key::F3 | Key::Char('n') if !self.viewer.search.results.is_empty() => {
                        if key_event.modifiers.shift {
                            self.viewer.search_prev();
                        } else {
                            self.viewer.search_next();
                        }
                        self.needs_redraw = true;
                        return Ok(true);
                    }
                    Key::Char('N') if !self.viewer.search.results.is_empty() => {
                        self.viewer.search_prev();
                        self.needs_redraw = true;
                        return Ok(true);
                    }
                    _ => {}
                }

                // Mode-specific keys
                match self.mode {
                    ViewMode::Image => self.handle_image_key(key_event.key, &key_event.modifiers),
                    ViewMode::Gallery => self.handle_gallery_key(key_event.key),
                }
            }

            InputEvent::MousePress(mouse_event) => {
                if self.mode == ViewMode::Image && mouse_event.button == Some(MouseButton::Left) {
                    // Check sidebar click first
                    if self.sidebar.visible {
                        if let Some(page) = self.sidebar.handle_click(
                            mouse_event.position.x as f64,
                            mouse_event.position.y as f64,
                        ) {
                            self.viewer.goto_page(page);
                            self.needs_redraw = true;
                            return Ok(true);
                        }
                        // Click was in sidebar but didn't select a page (tab bar click)
                        if (mouse_event.position.x as u32) < SIDEBAR_WIDTH {
                            self.needs_redraw = true;
                            return Ok(true);
                        }
                    }

                    // Check for link click first (without modifiers)
                    if !mouse_event.modifiers.shift && self.viewer.supports_links() {
                        if let Some((pdf_x, pdf_y)) = self.screen_to_pdf_coords(
                            mouse_event.position.x as f64,
                            mouse_event.position.y as f64,
                        ) {
                            if let Some(dest) = self.viewer.link_at_position(pdf_x, pdf_y) {
                                self.handle_link_click(dest);
                                return Ok(true);
                            }
                        }
                    }

                    // Check if Shift is held for text selection
                    if mouse_event.modifiers.shift && self.viewer.supports_text_selection() {
                        // Start text selection (use image coords, not PDF coords)
                        if let Some((img_x, img_y)) = self.screen_to_image_coords(
                            mouse_event.position.x as f64,
                            mouse_event.position.y as f64,
                        ) {
                            self.viewer.start_selection(img_x, img_y);
                            self.selecting_text = true;
                            self.needs_redraw = true;
                        }
                    } else {
                        // Normal pan/drag
                        self.viewer.scroll.start_drag(mouse_event.position);
                    }
                }
            }

            InputEvent::MouseRelease(mouse_event) => {
                if self.mode == ViewMode::Image && mouse_event.button == Some(MouseButton::Left) {
                    if self.selecting_text {
                        // End text selection
                        if let Some(text) = self.viewer.end_selection() {
                            if !text.trim().is_empty() {
                                tracing::info!("Selected text: {} chars", text.len());
                                // Auto-copy to clipboard
                                if let Err(e) = self.copy_to_clipboard(&text) {
                                    tracing::error!("Failed to copy to clipboard: {}", e);
                                }
                            }
                        }
                        self.selecting_text = false;
                        self.needs_redraw = true;
                    } else {
                        self.viewer.scroll.end_drag();
                    }
                }
            }

            InputEvent::MouseMove(mouse_event) => {
                if self.mode == ViewMode::Image {
                    if self.selecting_text {
                        // Update text selection (use image coords)
                        if let Some((img_x, img_y)) = self.screen_to_image_coords(
                            mouse_event.position.x as f64,
                            mouse_event.position.y as f64,
                        ) {
                            self.viewer.update_selection(img_x, img_y);
                            self.needs_redraw = true;
                        }
                    } else if self.viewer.scroll.is_dragging() {
                        self.viewer.scroll.update_drag(mouse_event.position);
                        self.needs_redraw = true;
                    }
                }
            }

            InputEvent::Scroll(scroll_event) => {
                // Check if scrolling in sidebar
                if self.sidebar.visible && (scroll_event.position.x as u32) < SIDEBAR_WIDTH {
                    let size = self.renderer.size();
                    let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);
                    self.sidebar.scroll(scroll_event.delta_y as f64 * 30.0, viewport_height);
                    self.needs_redraw = true;
                    return Ok(true);
                }

                match self.mode {
                    ViewMode::Image => {
                        let size = self.renderer.size();
                        let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);

                        if scroll_event.modifiers.ctrl {
                            let factor = if scroll_event.delta_y < 0 { 1.1 } else { 0.9 };
                            self.viewer.zoom_at_point(
                                factor,
                                scroll_event.position.x as f64,
                                scroll_event.position.y as f64,
                                size.width as f64,
                                viewport_height as f64,
                            );
                        } else {
                            // Use continuous scroll for continuous view mode
                            if self.viewer.view_mode() == crate::viewer::DocumentViewMode::Continuous {
                                self.viewer.scroll_continuous(
                                    scroll_event.delta_y as f64 * 30.0,
                                    viewport_height as f64,
                                );
                            } else {
                                // Single page mode: check for page navigation at bounds
                                let delta_y = scroll_event.delta_y as f64 * 30.0;
                                if let Some((img_w, img_h)) = self.viewer.effective_size() {
                                    let zoom = self.viewer.zoom.level;
                                    let scaled_h = img_h * zoom;

                                    // Check if scrolling down at bottom → next page
                                    if delta_y > 0.0 && self.viewer.scroll.at_bottom(scaled_h, viewport_height as f64) {
                                        if self.viewer.is_multipage() && self.viewer.next_page() {
                                            self.viewer.scroll.reset();
                                            // Scroll to top of new page
                                            if let Some((_, new_h)) = self.viewer.effective_size() {
                                                let new_scaled_h = new_h * self.viewer.zoom.level;
                                                self.viewer.scroll.clamp(img_w * zoom, new_scaled_h, size.width as f64, viewport_height as f64);
                                            }
                                            // Update sidebar selection
                                            if self.sidebar.visible {
                                                self.sidebar.selected_page = Some(self.viewer.current_page());
                                            }
                                        }
                                    }
                                    // Check if scrolling up at top → previous page
                                    else if delta_y < 0.0 && self.viewer.scroll.at_top(scaled_h, viewport_height as f64) {
                                        if self.viewer.is_multipage() && self.viewer.prev_page() {
                                            // Scroll to bottom of new page
                                            if let Some((new_w, new_h)) = self.viewer.effective_size() {
                                                let new_zoom = self.viewer.zoom.level;
                                                let new_scaled_h = new_h * new_zoom;
                                                let max_y = (new_scaled_h - viewport_height as f64).max(0.0);
                                                self.viewer.scroll.offset_y = max_y;
                                                self.viewer.scroll.clamp(new_w * new_zoom, new_scaled_h, size.width as f64, viewport_height as f64);
                                            }
                                            // Update sidebar selection
                                            if self.sidebar.visible {
                                                self.sidebar.selected_page = Some(self.viewer.current_page());
                                            }
                                        }
                                    } else {
                                        // Normal scroll
                                        self.viewer.scroll.pan(
                                            scroll_event.delta_x as f64 * 30.0,
                                            delta_y,
                                        );
                                    }
                                } else {
                                    // No image loaded, just pan
                                    self.viewer.scroll.pan(
                                        scroll_event.delta_x as f64 * 30.0,
                                        delta_y,
                                    );
                                }
                            }
                        }
                    }
                    ViewMode::Gallery => {
                        if let Some(ref mut gallery) = self.gallery {
                            let size = self.renderer.size();
                            let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);
                            gallery.scroll(scroll_event.delta_y as f64 * 30.0, viewport_height);
                        }
                    }
                }
                self.needs_redraw = true;
            }

            InputEvent::Idle => {}

            _ => {}
        }

        Ok(true)
    }

    fn handle_image_key(&mut self, key: Key, modifiers: &gartk_core::Modifiers) {
        match key {
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

            // Pan with arrow keys when zoomed, navigate pages at bounds
            Key::Up if modifiers.is_empty() => {
                let size = self.renderer.size();
                let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);

                if let Some((img_w, img_h)) = self.viewer.effective_size() {
                    let zoom = self.viewer.zoom.level;
                    let scaled_h = img_h * zoom;

                    // If at top and multipage, go to previous page
                    if self.viewer.scroll.at_top(scaled_h, viewport_height as f64)
                        && self.viewer.is_multipage()
                        && self.viewer.prev_page()
                    {
                        // Scroll to bottom of new page
                        if let Some((new_w, new_h)) = self.viewer.effective_size() {
                            let new_zoom = self.viewer.zoom.level;
                            let new_scaled_h = new_h * new_zoom;
                            let max_y = (new_scaled_h - viewport_height as f64).max(0.0);
                            self.viewer.scroll.offset_y = max_y;
                            self.viewer.scroll.clamp(new_w * new_zoom, new_scaled_h, size.width as f64, viewport_height as f64);
                        }
                        // Update sidebar selection
                        if self.sidebar.visible {
                            self.sidebar.selected_page = Some(self.viewer.current_page());
                        }
                    } else {
                        self.viewer.scroll.pan(0.0, -50.0);
                        self.viewer.scroll.clamp(img_w * zoom, scaled_h, size.width as f64, viewport_height as f64);
                    }
                } else {
                    self.viewer.scroll.pan(0.0, -50.0);
                }
                self.needs_redraw = true;
            }
            Key::Down if modifiers.is_empty() => {
                let size = self.renderer.size();
                let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);

                if let Some((img_w, img_h)) = self.viewer.effective_size() {
                    let zoom = self.viewer.zoom.level;
                    let scaled_h = img_h * zoom;

                    // If at bottom and multipage, go to next page
                    if self.viewer.scroll.at_bottom(scaled_h, viewport_height as f64)
                        && self.viewer.is_multipage()
                        && self.viewer.next_page()
                    {
                        self.viewer.scroll.reset();
                        // Clamp to top of new page
                        if let Some((new_w, new_h)) = self.viewer.effective_size() {
                            let new_zoom = self.viewer.zoom.level;
                            let new_scaled_h = new_h * new_zoom;
                            self.viewer.scroll.clamp(new_w * new_zoom, new_scaled_h, size.width as f64, viewport_height as f64);
                        }
                        // Update sidebar selection
                        if self.sidebar.visible {
                            self.sidebar.selected_page = Some(self.viewer.current_page());
                        }
                    } else {
                        self.viewer.scroll.pan(0.0, 50.0);
                        self.viewer.scroll.clamp(img_w * zoom, scaled_h, size.width as f64, viewport_height as f64);
                    }
                } else {
                    self.viewer.scroll.pan(0.0, 50.0);
                }
                self.needs_redraw = true;
            }

            // Slideshow controls
            Key::Char('s') => {
                self.slideshow.active = !self.slideshow.active;
                if self.slideshow.active {
                    self.slideshow.last_advance = Instant::now();
                    tracing::info!("Slideshow started ({:.1}s interval)", self.slideshow.interval.as_secs_f64());
                } else {
                    tracing::info!("Slideshow stopped");
                }
                self.needs_redraw = true;
            }
            Key::Char('[') => {
                // Decrease slideshow interval (min 1s)
                let new_interval = self.slideshow.interval.as_secs_f64() - 0.5;
                self.slideshow.interval = Duration::from_secs_f64(new_interval.max(1.0));
                self.needs_redraw = true;
            }
            Key::Char(']') => {
                // Increase slideshow interval (max 30s)
                let new_interval = self.slideshow.interval.as_secs_f64() + 0.5;
                self.slideshow.interval = Duration::from_secs_f64(new_interval.min(30.0));
                self.needs_redraw = true;
            }

            // Page navigation (for multi-page documents like PDF)
            Key::PageDown => {
                if self.viewer.next_page() {
                    self.needs_redraw = true;
                }
            }
            Key::PageUp => {
                if self.viewer.prev_page() {
                    self.needs_redraw = true;
                }
            }
            Key::Home => {
                if self.viewer.first_page() {
                    self.needs_redraw = true;
                }
            }
            Key::End => {
                if self.viewer.last_page() {
                    self.needs_redraw = true;
                }
            }

            // View mode switching (for multi-page documents)
            Key::Char('1') => {
                self.viewer.set_view_mode(crate::viewer::DocumentViewMode::SinglePage);
                self.needs_redraw = true;
            }
            Key::Char('2') => {
                self.viewer.set_view_mode(crate::viewer::DocumentViewMode::Continuous);
                self.needs_redraw = true;
            }
            Key::Char('3') => {
                self.viewer.set_view_mode(crate::viewer::DocumentViewMode::DualPage);
                self.needs_redraw = true;
            }

            _ => {}
        }
    }

    fn handle_gallery_key(&mut self, key: Key) {
        let Some(ref mut gallery) = self.gallery else {
            return;
        };

        match key {
            // Navigation
            Key::Right | Key::Char('l') => {
                gallery.select_next();
                self.needs_redraw = true;
            }
            Key::Left | Key::Char('h') => {
                gallery.select_prev();
                self.needs_redraw = true;
            }
            Key::Down | Key::Char('j') => {
                gallery.select_down();
                self.needs_redraw = true;
            }
            Key::Up | Key::Char('k') => {
                gallery.select_up();
                self.needs_redraw = true;
            }

            // Open selected image
            Key::Return | Key::Space => {
                if let Some(path) = gallery.selected_path() {
                    let path = path.to_path_buf();
                    if let Err(e) = self.viewer.load(&path) {
                        tracing::error!("Failed to load image: {}", e);
                    } else {
                        self.mode = ViewMode::Image;
                    }
                }
                self.needs_redraw = true;
            }

            // Sorting (s + modifier for sort type)
            Key::Char('s') => {
                // Cycle through sort modes: Name -> Date -> Size -> Name...
                let new_order = match gallery.sort_order() {
                    SortOrder::Name | SortOrder::NameDesc => SortOrder::Date,
                    SortOrder::Date | SortOrder::DateDesc => SortOrder::Size,
                    SortOrder::Size | SortOrder::SizeDesc => SortOrder::Name,
                };
                gallery.set_sort(new_order);
                self.needs_redraw = true;
            }
            Key::Char('S') => {
                // Reverse current sort
                let new_order = match gallery.sort_order() {
                    SortOrder::Name => SortOrder::NameDesc,
                    SortOrder::NameDesc => SortOrder::Name,
                    SortOrder::Date => SortOrder::DateDesc,
                    SortOrder::DateDesc => SortOrder::Date,
                    SortOrder::Size => SortOrder::SizeDesc,
                    SortOrder::SizeDesc => SortOrder::Size,
                };
                gallery.set_sort(new_order);
                self.needs_redraw = true;
            }

            _ => {}
        }
    }

    fn toggle_view_mode(&mut self) {
        match self.mode {
            ViewMode::Image => {
                // Switch to gallery - need to ensure gallery exists
                if self.gallery.is_none() {
                    // Try to create gallery from current image's directory
                    if let Some(path) = self.viewer.current_path() {
                        if let Some(parent) = path.parent() {
                            match GalleryView::new() {
                                Ok(mut gal) => {
                                    if gal.open(parent).is_ok() {
                                        self.gallery = Some(gal);
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to create gallery: {}", e);
                                }
                            }
                        }
                    }
                }
                if self.gallery.is_some() {
                    self.mode = ViewMode::Gallery;
                }
            }
            ViewMode::Gallery => {
                self.mode = ViewMode::Image;
            }
        }
        self.needs_redraw = true;
    }

    /// Convert screen coordinates to image coordinates
    /// Returns coordinates where Y increases downward (same as screen)
    fn screen_to_image_coords(&self, screen_x: f64, screen_y: f64) -> Option<(f64, f64)> {
        let size = self.renderer.size();
        let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);

        let (img_w, img_h) = self.viewer.effective_size()?;
        let zoom = self.viewer.zoom.level;
        let scaled_w = img_w * zoom;
        let scaled_h = img_h * zoom;

        // Calculate image position (same as in render_single_page)
        let x = if scaled_w < size.width as f64 {
            (size.width as f64 - scaled_w) / 2.0
        } else {
            -self.viewer.scroll.offset_x
        };

        let y = if scaled_h < viewport_height as f64 {
            (viewport_height as f64 - scaled_h) / 2.0
        } else {
            -self.viewer.scroll.offset_y
        };

        // Convert screen to image coordinates
        let img_x = (screen_x - x) / zoom;
        let img_y = (screen_y - y) / zoom;

        // Check if within image bounds
        if img_x >= 0.0 && img_x <= img_w && img_y >= 0.0 && img_y <= img_h {
            Some((img_x, img_y))
        } else {
            None
        }
    }

    /// Convert screen coordinates to PDF page coordinates (Y inverted)
    /// Used for link detection where PDF uses Y=0 at bottom
    fn screen_to_pdf_coords(&self, screen_x: f64, screen_y: f64) -> Option<(f64, f64)> {
        let (img_x, img_y) = self.screen_to_image_coords(screen_x, screen_y)?;

        // Convert to PDF coordinates (Y is inverted)
        let page_size = self.viewer.page_size_for(self.viewer.current_page())?;
        let pdf_y = page_size.height - img_y;
        Some((img_x, pdf_y))
    }

    /// Copy text to clipboard
    fn copy_to_clipboard(&self, text: &str) -> Result<()> {
        // Use xclip for reliable X11 clipboard handling
        // arboard has timeout issues with some clipboard managers
        use std::process::{Command, Stdio};
        use std::io::Write;

        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
            .context("Failed to spawn xclip - is it installed?")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes()).context("Failed to write to xclip")?;
        }

        child.wait().context("xclip failed")?;
        Ok(())
    }

    /// Handle a link click
    fn handle_link_click(&mut self, dest: LinkDestination) {
        match dest {
            LinkDestination::Page(page) => {
                // Navigate to page
                if self.viewer.goto_page(page) {
                    self.needs_redraw = true;
                    // Update sidebar selection
                    if self.sidebar.visible {
                        self.sidebar.selected_page = Some(page);
                    }
                }
            }
            LinkDestination::Uri(uri) => {
                // Open external URI
                tracing::info!("Opening URI: {}", uri);
                if let Err(e) = open::that(&uri) {
                    tracing::error!("Failed to open URI {}: {}", uri, e);
                }
            }
            LinkDestination::Named(name) => {
                // Named destination - would need to resolve via document
                tracing::debug!("Named destination not yet supported: {}", name);
            }
        }
    }

    /// Generate thumbnails for the sidebar
    fn generate_sidebar_thumbnails(&mut self) {
        let page_count = self.viewer.page_count();
        const THUMB_SIZE: u32 = 180;

        // Generate thumbnails for all pages (could be optimized to only visible)
        for page in 0..page_count {
            if self.sidebar.thumbnails.contains_key(&page) {
                continue;
            }

            // Get page size to calculate scale
            if let Some(page_size) = self.viewer.page_size_for(page) {
                let scale = (THUMB_SIZE as f64 / page_size.width)
                    .min(THUMB_SIZE as f64 / page_size.height);

                let thumb_w = (page_size.width * scale) as u32;
                let thumb_h = (page_size.height * scale) as u32;

                // Render page directly at thumbnail scale using backend
                if let Some(backend) = &mut self.viewer.backend_mut() {
                    if let Ok(rendered) = backend.render_page(page, scale) {
                        // Use actual rendered dimensions - they must match the data
                        self.sidebar.add_thumbnail(page, ThumbnailData {
                            width: rendered.width,
                            height: rendered.height,
                            data: rendered.data,
                        });
                    }
                }
            }
        }
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

        match self.mode {
            ViewMode::Gallery => self.render_gallery(viewport_height)?,
            ViewMode::Image => self.render_image(viewport_height)?,
        }

        // Render status bar
        let (file_info, zoom_level, zoom_mode, position) = match self.mode {
            ViewMode::Image => {
                // For multi-page documents, show page position; otherwise show directory position
                let pos = self.viewer.page_position()
                    .or_else(|| self.viewer.directory_position());
                (
                    self.viewer.file_info(),
                    self.viewer.zoom.level,
                    self.viewer.zoom.mode,
                    pos,
                )
            }
            ViewMode::Gallery => {
                let pos = self.gallery.as_ref().map(|g| (g.selection_index() + 1, g.file_count()));
                (None, 1.0, crate::viewer::ZoomMode::Fit, pos)
            }
        };

        self.statusbar.render(
            &self.renderer,
            size.width,
            viewport_height,
            file_info.as_ref(),
            zoom_level,
            zoom_mode,
            position,
        )?;

        // Render sidebar if visible
        if self.sidebar.visible {
            self.sidebar.render(&self.renderer, viewport_height)?;
        }

        // Render search bar if active
        if self.search_active {
            self.render_search_bar(size.width)?;
        } else if !self.viewer.search.results.is_empty() {
            // Show search result position indicator
            self.render_search_status(size.width)?;
        }

        // Render go to page dialog if active
        if self.goto_page_active {
            self.render_goto_page_dialog(size.width)?;
        }

        // Copy to window
        self.renderer.flush();
        copy_surface_to_window(self.renderer.surface_mut(), &self.window, self.gc, 0, 0)?;

        Ok(())
    }

    fn render_image(&mut self, viewport_height: u32) -> Result<()> {
        // Dispatch to appropriate render method based on view mode
        match self.viewer.view_mode() {
            crate::viewer::DocumentViewMode::SinglePage => {
                self.render_single_page(viewport_height)
            }
            crate::viewer::DocumentViewMode::Continuous => {
                self.render_continuous(viewport_height)
            }
            crate::viewer::DocumentViewMode::DualPage => {
                // TODO: Implement dual page view
                self.render_single_page(viewport_height)
            }
        }
    }

    fn render_single_page(&mut self, viewport_height: u32) -> Result<()> {
        let size = self.renderer.size();

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

                // Clip to viewport - critical for performance when zoomed in
                ctx.rectangle(0.0, 0.0, size.width as f64, viewport_height as f64);
                ctx.clip();

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

                // Paint the image surface
                ctx.set_source_surface(image_surface.cairo_surface(), 0.0, 0.0)?;
                // Use NEAREST at high zoom (looking at pixels anyway), Bilinear otherwise
                let filter = if display_scale > 2.0 {
                    gartk_render::cairo::Filter::Nearest
                } else {
                    gartk_render::cairo::Filter::Bilinear
                };
                ctx.source().set_filter(filter);
                ctx.paint()?;

                ctx.restore()?;

                // Draw search highlights if any
                let current_page = self.viewer.current_page();
                let search_rects = self.viewer.search_results_for_page(current_page);
                if !search_rects.is_empty() {
                    self.render_search_highlights(
                        &search_rects,
                        current_page,
                        x,
                        y,
                        zoom,
                        scaled_w,
                        scaled_h,
                        rotation,
                        flip_h,
                        flip_v,
                        size.width as f64,
                        viewport_height as f64,
                    )?;
                }

                // Draw text selection highlight
                // First try text-aware selection region (proper text highlighting)
                if let Some((sel_page, rects)) = self.viewer.selection_region(zoom) {
                    if sel_page == current_page && !rects.is_empty() {
                        self.render_selection_region(
                            &rects,
                            x,
                            y,
                            rotation,
                            flip_h,
                            flip_v,
                            scaled_w,
                            scaled_h,
                            size.width as f64,
                            viewport_height as f64,
                        )?;
                    }
                } else if let Some((sel_page, sel_x1, sel_y1, sel_x2, sel_y2)) = self.viewer.selection_rect() {
                    // Fall back to simple rectangle for non-PDF or if region fails
                    if sel_page == current_page {
                        self.render_selection_highlight(
                            sel_x1,
                            sel_y1,
                            sel_x2,
                            sel_y2,
                            x,
                            y,
                            zoom,
                            scaled_w,
                            scaled_h,
                            rotation,
                            flip_h,
                            flip_v,
                            size.width as f64,
                            viewport_height as f64,
                        )?;
                    }
                }
            }
            return Ok(());
        }

        // Show loading indicator or placeholder
        self.render_loading_state(viewport_height)
    }

    fn render_continuous(&mut self, viewport_height: u32) -> Result<()> {
        let size = self.renderer.size();
        let zoom = self.viewer.zoom.level;
        let scroll_y = self.viewer.continuous_scroll_y();

        // Get visible pages
        let (start_page, end_page) = self.viewer.visible_pages(viewport_height as f64, zoom);

        let ctx = self.renderer.context()?;
        ctx.save()?;

        // Clip to viewport
        ctx.rectangle(0.0, 0.0, size.width as f64, viewport_height as f64);
        ctx.clip();

        // Calculate page gap constant (same as in image_viewer)
        const PAGE_GAP: f64 = 20.0;

        // Render each visible page
        let mut y_offset = 0.0;
        for page_idx in 0..=end_page {
            // Get page size
            let page_size = match self.viewer.page_size_for(page_idx) {
                Some(s) => s,
                None => continue,
            };

            let page_width = page_size.width * zoom;
            let page_height = page_size.height * zoom;

            // Only render if page is visible
            if page_idx >= start_page {
                // Calculate page position
                let page_y = y_offset - scroll_y;
                let page_x = (size.width as f64 - page_width).max(0.0) / 2.0;

                // Skip if page is off-screen
                if page_y + page_height >= 0.0 && page_y < viewport_height as f64 {
                    // Render page surface at current zoom for sharp PDF text
                    if let Ok(surface) = self.viewer.render_page_surface(page_idx, zoom) {
                        let surface_w = surface.width() as f64;
                        let display_scale = page_width / surface_w;

                        ctx.save()?;
                        ctx.translate(page_x, page_y);
                        ctx.scale(display_scale, display_scale);
                        ctx.set_source_surface(surface.cairo_surface(), 0.0, 0.0)?;
                        ctx.source().set_filter(gartk_render::cairo::Filter::Bilinear);
                        ctx.paint()?;
                        ctx.restore()?;

                        // Draw search highlights for this page
                        let search_rects = self.viewer.search_results_for_page(page_idx);
                        if !search_rects.is_empty() {
                            if let Some(ps) = self.viewer.page_size_for(page_idx) {
                                ctx.save()?;
                                ctx.translate(page_x, page_y);

                                for (i, (x1, y1, x2, y2)) in search_rects.iter().enumerate() {
                                    // Convert PDF coordinates to screen
                                    let screen_y1 = ps.height - y2;
                                    let screen_y2 = ps.height - y1;

                                    let rect_x = x1 * zoom;
                                    let rect_y = screen_y1 * zoom;
                                    let rect_w = (x2 - x1) * zoom;
                                    let rect_h = (screen_y2 - screen_y1) * zoom;

                                    let is_current = self.viewer.is_current_search_result(page_idx, i);

                                    if is_current {
                                        ctx.set_source_rgba(1.0, 0.6, 0.0, 0.5);
                                    } else {
                                        ctx.set_source_rgba(1.0, 1.0, 0.0, 0.3);
                                    }

                                    ctx.rectangle(rect_x, rect_y, rect_w, rect_h);
                                    ctx.fill()?;
                                }

                                ctx.restore()?;
                            }
                        }
                    }
                }
            }

            y_offset += page_height + PAGE_GAP;
        }

        ctx.restore()?;
        Ok(())
    }

    fn render_loading_state(&mut self, viewport_height: u32) -> Result<()> {
        let size = self.renderer.size();
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

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn render_search_highlights(
        &self,
        search_rects: &[(f64, f64, f64, f64)],
        page: usize,
        x: f64,
        y: f64,
        zoom: f64,
        scaled_w: f64,
        scaled_h: f64,
        rotation: i32,
        flip_h: bool,
        flip_v: bool,
        viewport_width: f64,
        viewport_height: f64,
    ) -> Result<()> {
        let ctx = self.renderer.context()?;

        ctx.save()?;

        // Clip to viewport
        ctx.rectangle(0.0, 0.0, viewport_width, viewport_height);
        ctx.clip();

        ctx.translate(x, y);

        // Apply same transformations as the image
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

        // Get page size for coordinate conversion
        if let Some(page_size) = self.viewer.page_size_for(page) {
            let page_h = page_size.height;

            for (i, (x1, y1, x2, y2)) in search_rects.iter().enumerate() {
                // Convert from PDF coordinates (origin bottom-left) to screen
                // PDF y increases upward, screen y increases downward
                let screen_y1 = page_h - y2;
                let screen_y2 = page_h - y1;

                let rect_x = x1 * zoom;
                let rect_y = screen_y1 * zoom;
                let rect_w = (x2 - x1) * zoom;
                let rect_h = (screen_y2 - screen_y1) * zoom;

                // Check if this is the current result
                let is_current = self.viewer.is_current_search_result(page, i);

                if is_current {
                    // Current result: orange highlight
                    ctx.set_source_rgba(1.0, 0.6, 0.0, 0.5);
                } else {
                    // Other results: yellow highlight
                    ctx.set_source_rgba(1.0, 1.0, 0.0, 0.3);
                }

                ctx.rectangle(rect_x, rect_y, rect_w, rect_h);
                ctx.fill()?;
            }
        }

        ctx.restore()?;
        Ok(())
    }

    /// Render text-aware selection as multiple rectangles (proper text highlighting)
    #[allow(clippy::too_many_arguments)]
    fn render_selection_region(
        &self,
        rects: &[(i32, i32, i32, i32)],
        x: f64,
        y: f64,
        rotation: i32,
        flip_h: bool,
        flip_v: bool,
        scaled_w: f64,
        scaled_h: f64,
        viewport_width: f64,
        viewport_height: f64,
    ) -> Result<()> {
        let ctx = self.renderer.context()?;

        ctx.save()?;

        // Clip to viewport
        ctx.rectangle(0.0, 0.0, viewport_width, viewport_height);
        ctx.clip();

        ctx.translate(x, y);

        // Apply same transformations as the image
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

        // Render each rectangle from the selection region
        // These are already in screen coordinates at the current zoom
        ctx.set_source_rgba(0.2, 0.5, 0.9, 0.3);
        for &(rx, ry, rw, rh) in rects {
            ctx.rectangle(rx as f64, ry as f64, rw as f64, rh as f64);
        }
        ctx.fill()?;

        ctx.restore()?;
        Ok(())
    }

    /// Render fallback rubber-band selection (single rectangle)
    #[allow(clippy::too_many_arguments)]
    fn render_selection_highlight(
        &self,
        sel_x1: f64,
        sel_y1: f64,
        sel_x2: f64,
        sel_y2: f64,
        x: f64,
        y: f64,
        zoom: f64,
        scaled_w: f64,
        scaled_h: f64,
        rotation: i32,
        flip_h: bool,
        flip_v: bool,
        viewport_width: f64,
        viewport_height: f64,
    ) -> Result<()> {
        let ctx = self.renderer.context()?;

        ctx.save()?;

        // Clip to viewport
        ctx.rectangle(0.0, 0.0, viewport_width, viewport_height);
        ctx.clip();

        ctx.translate(x, y);

        // Apply same transformations as the image
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

        // Selection coords are already in image space (Y down), just scale by zoom
        let rect_x = sel_x1 * zoom;
        let rect_y = sel_y1 * zoom;
        let rect_w = (sel_x2 - sel_x1) * zoom;
        let rect_h = (sel_y2 - sel_y1) * zoom;

        // Blue selection highlight
        ctx.set_source_rgba(0.2, 0.5, 0.9, 0.3);
        ctx.rectangle(rect_x, rect_y, rect_w, rect_h);
        ctx.fill()?;

        // Selection border
        ctx.set_source_rgba(0.2, 0.5, 0.9, 0.8);
        ctx.set_line_width(1.0);
        ctx.rectangle(rect_x, rect_y, rect_w, rect_h);
        ctx.stroke()?;

        ctx.restore()?;
        Ok(())
    }

    fn render_search_bar(&mut self, width: u32) -> Result<()> {
        let ctx = self.renderer.context()?;

        // Search bar dimensions
        let bar_width = 300.0_f64.min(width as f64 - 40.0);
        let bar_height = 32.0;
        let bar_x = (width as f64 - bar_width) / 2.0;
        let bar_y = 10.0;
        let padding = 8.0;
        let radius = 4.0;

        ctx.save()?;

        // Draw background with rounded corners
        ctx.new_path();
        ctx.arc(bar_x + radius, bar_y + radius, radius, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        ctx.arc(bar_x + bar_width - radius, bar_y + radius, radius, 1.5 * std::f64::consts::PI, 2.0 * std::f64::consts::PI);
        ctx.arc(bar_x + bar_width - radius, bar_y + bar_height - radius, radius, 0.0, 0.5 * std::f64::consts::PI);
        ctx.arc(bar_x + radius, bar_y + bar_height - radius, radius, 0.5 * std::f64::consts::PI, std::f64::consts::PI);
        ctx.close_path();

        // Fill background
        ctx.set_source_rgba(0.15, 0.15, 0.15, 0.95);
        ctx.fill_preserve()?;

        // Draw border
        ctx.set_source_rgb(0.4, 0.4, 0.4);
        ctx.set_line_width(1.0);
        ctx.stroke()?;

        // Draw search icon
        ctx.set_source_rgb(0.6, 0.6, 0.6);
        ctx.set_line_width(2.0);
        let icon_x = bar_x + padding + 6.0;
        let icon_y = bar_y + bar_height / 2.0;
        ctx.arc(icon_x, icon_y - 2.0, 5.0, 0.0, 2.0 * std::f64::consts::PI);
        ctx.stroke()?;
        ctx.move_to(icon_x + 3.5, icon_y + 1.5);
        ctx.line_to(icon_x + 7.0, icon_y + 5.0);
        ctx.stroke()?;

        // Draw text
        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Normal,
        );
        ctx.set_font_size(14.0);
        ctx.set_source_rgb(0.9, 0.9, 0.9);

        let text_x = bar_x + padding + 20.0;
        let text_y = bar_y + bar_height / 2.0 + 5.0;

        if self.search_input.is_empty() {
            ctx.set_source_rgb(0.5, 0.5, 0.5);
            ctx.move_to(text_x, text_y);
            ctx.show_text("Search...")?;
        } else {
            ctx.move_to(text_x, text_y);
            ctx.show_text(&self.search_input)?;
        }

        // Draw cursor
        let input_extents = ctx.text_extents(&self.search_input)?;
        ctx.set_source_rgb(0.9, 0.9, 0.9);
        ctx.set_line_width(1.0);
        ctx.move_to(text_x + input_extents.width() + 2.0, bar_y + 8.0);
        ctx.line_to(text_x + input_extents.width() + 2.0, bar_y + bar_height - 8.0);
        ctx.stroke()?;

        ctx.restore()?;
        Ok(())
    }

    fn render_search_status(&mut self, width: u32) -> Result<()> {
        let ctx = self.renderer.context()?;

        if let Some((current, total)) = self.viewer.search_position() {
            let text = format!("{}/{}", current, total);

            // Status pill dimensions
            let padding_h = 12.0;
            let padding_v = 6.0;

            ctx.select_font_face(
                "sans-serif",
                gartk_render::cairo::FontSlant::Normal,
                gartk_render::cairo::FontWeight::Normal,
            );
            ctx.set_font_size(12.0);
            let extents = ctx.text_extents(&text)?;

            let pill_width = extents.width() + padding_h * 2.0;
            let pill_height = extents.height() + padding_v * 2.0;
            let pill_x = width as f64 - pill_width - 10.0;
            let pill_y = 10.0;
            let radius = pill_height / 2.0;

            ctx.save()?;

            // Draw pill background
            ctx.new_path();
            ctx.arc(pill_x + radius, pill_y + radius, radius, 0.5 * std::f64::consts::PI, 1.5 * std::f64::consts::PI);
            ctx.arc(pill_x + pill_width - radius, pill_y + radius, radius, 1.5 * std::f64::consts::PI, 0.5 * std::f64::consts::PI);
            ctx.close_path();

            ctx.set_source_rgba(0.2, 0.4, 0.6, 0.9);
            ctx.fill()?;

            // Draw text
            ctx.set_source_rgb(1.0, 1.0, 1.0);
            ctx.move_to(pill_x + padding_h, pill_y + padding_v + extents.height());
            ctx.show_text(&text)?;

            ctx.restore()?;
        }

        Ok(())
    }

    fn render_gallery(&mut self, viewport_height: u32) -> Result<()> {
        if let Some(ref mut gallery) = self.gallery {
            gallery.render(&self.renderer, viewport_height)?;
        } else {
            // No gallery - show message
            let ctx = self.renderer.context()?;
            let size = self.renderer.size();
            ctx.set_source_rgb(0.5, 0.5, 0.5);
            ctx.select_font_face(
                "sans-serif",
                gartk_render::cairo::FontSlant::Normal,
                gartk_render::cairo::FontWeight::Normal,
            );
            ctx.set_font_size(20.0);
            let text = "No directory loaded. Press 'g' to open gallery.";
            let extents = ctx.text_extents(text)?;
            ctx.move_to(
                (size.width as f64 - extents.width()) / 2.0,
                (viewport_height as f64 + extents.height()) / 2.0,
            );
            ctx.show_text(text)?;
        }
        Ok(())
    }

    fn render_goto_page_dialog(&mut self, width: u32) -> Result<()> {
        let ctx = self.renderer.context()?;

        // Dialog dimensions
        let dialog_width = 250.0_f64.min(width as f64 - 40.0);
        let dialog_height = 32.0;
        let dialog_x = (width as f64 - dialog_width) / 2.0;
        let dialog_y = 10.0;
        let padding = 8.0;
        let radius = 4.0;

        ctx.save()?;

        // Draw background with rounded corners
        ctx.new_path();
        ctx.arc(dialog_x + radius, dialog_y + radius, radius, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        ctx.arc(dialog_x + dialog_width - radius, dialog_y + radius, radius, 1.5 * std::f64::consts::PI, 2.0 * std::f64::consts::PI);
        ctx.arc(dialog_x + dialog_width - radius, dialog_y + dialog_height - radius, radius, 0.0, 0.5 * std::f64::consts::PI);
        ctx.arc(dialog_x + radius, dialog_y + dialog_height - radius, radius, 0.5 * std::f64::consts::PI, std::f64::consts::PI);
        ctx.close_path();

        // Fill background
        ctx.set_source_rgba(0.15, 0.15, 0.15, 0.95);
        ctx.fill_preserve()?;

        // Draw border
        ctx.set_source_rgb(0.4, 0.4, 0.4);
        ctx.set_line_width(1.0);
        ctx.stroke()?;

        // Draw label
        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Normal,
        );
        ctx.set_font_size(14.0);

        let label = format!("Go to page (1-{}):", self.viewer.page_count());
        let text_y = dialog_y + dialog_height / 2.0 + 5.0;

        ctx.set_source_rgb(0.7, 0.7, 0.7);
        ctx.move_to(dialog_x + padding, text_y);
        ctx.show_text(&label)?;

        // Get label width for positioning input
        let label_extents = ctx.text_extents(&label)?;
        let input_x = dialog_x + padding + label_extents.width() + 8.0;

        // Draw input text
        ctx.set_source_rgb(0.9, 0.9, 0.9);
        if self.goto_page_input.is_empty() {
            ctx.set_source_rgb(0.5, 0.5, 0.5);
            ctx.move_to(input_x, text_y);
            ctx.show_text("#")?;
        } else {
            ctx.move_to(input_x, text_y);
            ctx.show_text(&self.goto_page_input)?;
        }

        // Draw cursor
        let input_extents = ctx.text_extents(&self.goto_page_input)?;
        ctx.set_source_rgb(0.9, 0.9, 0.9);
        ctx.set_line_width(1.0);
        ctx.move_to(input_x + input_extents.width() + 2.0, dialog_y + 8.0);
        ctx.line_to(input_x + input_extents.width() + 2.0, dialog_y + dialog_height - 8.0);
        ctx.stroke()?;

        ctx.restore()?;
        Ok(())
    }
}
