use anyhow::{Context, Result};
use gartk_core::{InputEvent, Key, MouseButton, Theme};
use gartk_render::{copy_surface_to_window, Renderer};
use gartk_x11::{Connection, EventLoop, EventLoopConfig, Window, WindowConfig};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use x11rb::protocol::xproto::{self, ConnectionExt, EventMask};

use crate::annotate::{AnnotationManager, ToolType};
use crate::config::Config;
use crate::forms::FormState;
use crate::ipc::{create_viewer_info, IpcCommand, IpcServer};
use crate::recent::RecentFiles;
use crate::ui::{Sidebar, StatusBar, ThumbnailData, STATUS_BAR_HEIGHT, SIDEBAR_WIDTH};
use crate::backend::LinkDestination;
use crate::viewer::{GalleryView, ImageViewer, LoadState, SortOrder, ZoomMode};

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
    /// Recent files manager
    recent_files: RecentFiles,
    /// Recent files panel active
    recent_panel_active: bool,
    /// Selected index in recent panel
    recent_panel_selected: usize,
    /// Properties panel active
    properties_panel_active: bool,
    /// Annotation manager (when in annotation mode)
    annotation: Option<AnnotationManager>,
    /// Annotation mode active
    annotation_mode: bool,
    /// IPC server for garviewctl
    ipc_server: Option<IpcServer>,
    /// PDF form state
    form_state: Option<FormState>,
    /// Last form field click (time, field_id) for double-click detection
    last_form_click: Option<(std::time::Instant, i32)>,
}

impl App {
    pub fn new(path: Option<String>, fullscreen: bool, slideshow: bool) -> Result<Self> {
        // Load configuration
        let config = Config::load().unwrap_or_else(|e| {
            tracing::warn!("Failed to load config: {}, using defaults", e);
            Config::default()
        });

        // Load recent files
        let recent_files = RecentFiles::load().unwrap_or_else(|e| {
            tracing::warn!("Failed to load recent files: {}, starting fresh", e);
            RecentFiles::new()
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
        let mut recent_files = recent_files; // Make mutable for adding
        if let Some(ref path_str) = path {
            let path = Path::new(path_str);
            if path.is_file() {
                if let Err(e) = viewer.load(path) {
                    tracing::error!("Failed to load {}: {}", path.display(), e);
                } else {
                    // Add to recent files on successful load
                    recent_files.add(path);
                    // Save immediately so recent files persist even on abnormal exit
                    if let Err(e) = recent_files.save() {
                        tracing::warn!("Failed to save recent files: {}", e);
                    }
                    // Restore session state (page, zoom, scroll) if available
                    if let Some((page, zoom, scroll)) = recent_files.get_session(path) {
                        if page < viewer.page_count() {
                            viewer.goto_page(page);
                        }
                        viewer.zoom.mode = ZoomMode::Custom(zoom / 100.0);
                        viewer.zoom.level = zoom / 100.0;
                        viewer.scroll.offset_x = scroll.0;
                        viewer.scroll.offset_y = scroll.1;
                    }
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
            recent_files,
            recent_panel_active: path.is_none(), // Auto-show when no file argument
            recent_panel_selected: 0,
            properties_panel_active: false,
            annotation: None,
            annotation_mode: false,
            ipc_server: IpcServer::start().ok(),
            form_state: None,
            last_form_click: None,
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

        // Enable drag and drop
        if let Err(e) = event_loop.enable_xdnd() {
            tracing::warn!("Failed to enable drag and drop: {}", e);
        }

        // Load form fields for initial document (if any)
        self.load_form_fields();

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
                        self.save_current_session();
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

            // Poll IPC commands
            // Collect commands first to avoid borrow conflicts
            let mut ipc_commands = Vec::new();
            if let Some(ref ipc) = self.ipc_server {
                while let Some(cmd) = ipc.try_recv() {
                    ipc_commands.push(cmd);
                }
            }
            // Process commands
            for (cmd, resp_tx) in ipc_commands {
                let response = self.handle_ipc_command(cmd);
                let _ = resp_tx.send(response);
                self.needs_redraw = true;
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

        // Save recent files on exit
        tracing::debug!("Saving {} recent files", self.recent_files.len());
        if let Err(e) = self.recent_files.save() {
            tracing::error!("Failed to save recent files: {}", e);
        }

        Ok(())
    }

    fn handle_event(&mut self, event: InputEvent) -> Result<bool> {
        match event {
            InputEvent::CloseRequested => {
                self.save_current_session();
                return Ok(false);
            }

            InputEvent::Resize { width, height } => {
                self.renderer.resize(width, height)?;
                self.window.set_size(width, height);
                self.needs_redraw = true;
            }

            InputEvent::Expose => {
                self.needs_redraw = true;
            }

            InputEvent::FileDrop(paths) => {
                // Handle dropped files - open the first valid file
                for path in paths {
                    if path.is_file() {
                        tracing::info!("File dropped: {}", path.display());
                        self.save_current_session();
                        if let Err(e) = self.viewer.load(&path) {
                            tracing::error!("Failed to load dropped file: {}", e);
                        } else {
                            self.recent_files.add(&path);
                            let _ = self.recent_files.save();
                            self.mode = ViewMode::Image;
                            self.restore_session(&path);
                            self.load_form_fields();
                        }
                        break; // Only open the first file
                    } else if path.is_dir() {
                        tracing::info!("Directory dropped: {}", path.display());
                        // Open in gallery mode
                        match GalleryView::new() {
                            Ok(mut gal) => {
                                if let Err(e) = gal.open(&path) {
                                    tracing::error!("Failed to open directory: {}", e);
                                } else {
                                    self.mode = ViewMode::Gallery;
                                    self.gallery = Some(gal);
                                }
                            }
                            Err(e) => {
                                tracing::error!("Failed to create gallery: {}", e);
                            }
                        }
                        break;
                    }
                }
                self.needs_redraw = true;
            }

            InputEvent::Key(key_event) if key_event.pressed => {
                // Handle annotation mode input first
                if self.annotation_mode {
                    if let Some(ref mut ann) = self.annotation {
                        match key_event.key {
                            Key::Escape => {
                                // Exit annotation mode (discard changes)
                                self.annotation_mode = false;
                                self.annotation = None;
                                self.needs_redraw = true;
                            }
                            // Tool selection shortcuts
                            Key::Char('b') => ann.select_tool(ToolType::Brush),
                            Key::Char('l') => ann.select_tool(ToolType::Line),
                            Key::Char('a') => ann.select_tool(ToolType::Arrow),
                            Key::Char('r') => ann.select_tool(ToolType::Rectangle),
                            Key::Char('e') => ann.select_tool(ToolType::Ellipse),
                            Key::Char('t') => ann.select_tool(ToolType::Text),
                            Key::Char('x') => ann.select_tool(ToolType::Blur),
                            Key::Char('h') => ann.select_tool(ToolType::Highlight),
                            // Color presets (1-9)
                            Key::Char(c @ '1'..='9') => {
                                let idx = (c as u8 - b'1') as usize;
                                ann.state.set_color_preset(idx);
                            }
                            // Line width adjustments
                            Key::Char('+') | Key::Char('=') => {
                                ann.state.properties.increase_line_width();
                            }
                            Key::Char('-') => {
                                ann.state.properties.decrease_line_width();
                            }
                            // Toggle fill mode
                            Key::Char('f') => {
                                ann.state.toggle_fill();
                            }
                            // Undo/Redo
                            Key::Char('z') if key_event.modifiers.ctrl => {
                                if key_event.modifiers.shift {
                                    let _ = ann.redo();
                                } else {
                                    let _ = ann.undo();
                                }
                            }
                            Key::Char('y') if key_event.modifiers.ctrl => {
                                let _ = ann.redo();
                            }
                            // Save annotated image (Ctrl+S)
                            Key::Char('s') if key_event.modifiers.ctrl => {
                                if let Err(e) = self.save_annotated_image() {
                                    tracing::error!("Failed to save annotated image: {}", e);
                                }
                            }
                            // Clear all annotations
                            Key::Delete | Key::Char('c') if key_event.modifiers.ctrl && key_event.modifiers.shift => {
                                let _ = ann.clear_all();
                            }
                            _ => {}
                        }
                        self.needs_redraw = true;
                    }
                    return Ok(true);
                }

                // Handle form field input when focused
                if let Some(ref mut form_state) = self.form_state {
                    if form_state.focused_field.is_some() {
                        match key_event.key {
                            Key::Escape => {
                                // Unfocus without committing (discard changes)
                                form_state.unfocus(false);
                                self.needs_redraw = true;
                            }
                            Key::Return => {
                                // Commit text field changes
                                if let Some((field_id, value)) = form_state.unfocus(true) {
                                    // Write value to backend
                                    if let Some(backend) = self.viewer.backend_mut() {
                                        if let Err(e) = backend.set_form_field_value(field_id, value) {
                                            tracing::error!("Failed to set form field value: {}", e);
                                        }
                                    }
                                }
                                self.needs_redraw = true;
                            }
                            Key::Tab => {
                                // Commit current field and move to next
                                if let Some((field_id, value)) = form_state.commit_current() {
                                    if let Some(backend) = self.viewer.backend_mut() {
                                        let _ = backend.set_form_field_value(field_id, value);
                                    }
                                }
                                if key_event.modifiers.shift {
                                    form_state.focus_prev();
                                } else {
                                    form_state.focus_next();
                                }
                                self.needs_redraw = true;
                            }
                            Key::Backspace => {
                                form_state.backspace();
                                self.needs_redraw = true;
                            }
                            Key::Delete => {
                                form_state.delete();
                                self.needs_redraw = true;
                            }
                            Key::Left => {
                                form_state.cursor_left();
                                self.needs_redraw = true;
                            }
                            Key::Right => {
                                form_state.cursor_right();
                                self.needs_redraw = true;
                            }
                            Key::Home => {
                                form_state.cursor_home();
                                self.needs_redraw = true;
                            }
                            Key::End => {
                                form_state.cursor_end();
                                self.needs_redraw = true;
                            }
                            Key::Space => {
                                // Space toggles checkbox/radio, or inserts space in text
                                if let Some(field) = form_state.focused() {
                                    match &field.field_type {
                                        crate::forms::FormFieldType::Checkbox { .. }
                                        | crate::forms::FormFieldType::RadioButton { .. } => {
                                            if let Some((field_id, value)) = form_state.toggle_checkbox() {
                                                if let Some(backend) = self.viewer.backend_mut() {
                                                    let _ = backend.set_form_field_value(field_id, value);
                                                }
                                            }
                                        }
                                        crate::forms::FormFieldType::Text { .. } => {
                                            form_state.insert_char(' ');
                                        }
                                        _ => {}
                                    }
                                }
                                self.needs_redraw = true;
                            }
                            Key::Char(c) => {
                                form_state.insert_char(c);
                                self.needs_redraw = true;
                            }
                            _ => {}
                        }
                        return Ok(true);
                    }
                }

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

                // Recent files panel input
                if self.recent_panel_active {
                    let total_items = self.recent_files.len() + 1; // +1 for "Open File..."
                    match key_event.key {
                        Key::Escape => {
                            self.recent_panel_active = false;
                            self.needs_redraw = true;
                        }
                        Key::Up | Key::Char('k') => {
                            if self.recent_panel_selected > 0 {
                                self.recent_panel_selected -= 1;
                            }
                            self.needs_redraw = true;
                        }
                        Key::Down | Key::Char('j') => {
                            if self.recent_panel_selected + 1 < total_items {
                                self.recent_panel_selected += 1;
                            }
                            self.needs_redraw = true;
                        }
                        Key::Return => {
                            tracing::info!("Enter pressed, selected index: {}", self.recent_panel_selected);
                            if self.recent_panel_selected == 0 {
                                // "Open File..." selected - launch file dialog
                                tracing::info!("Opening file dialog...");
                                self.recent_panel_active = false;
                                self.needs_redraw = true;

                                match self.open_file_dialog() {
                                    Ok(Some(path)) => {
                                        self.save_current_session();
                                        if let Err(e) = self.viewer.load(&path) {
                                            tracing::error!("Failed to load file: {}", e);
                                            // Reopen panel on error
                                            self.recent_panel_active = true;
                                        } else {
                                            self.recent_files.add(&path);
                                            let _ = self.recent_files.save();
                                            self.mode = ViewMode::Image;
                                            self.restore_session(&path);
                                            self.load_form_fields();
                                        }
                                    }
                                    Ok(None) => {
                                        // User cancelled - reopen panel
                                        self.recent_panel_active = true;
                                    }
                                    Err(e) => {
                                        tracing::error!("File dialog error: {}", e);
                                        self.recent_panel_active = true;
                                    }
                                }
                            } else {
                                // Open selected recent file (index adjusted by -1)
                                let file_index = self.recent_panel_selected - 1;
                                if let Some(entry) = self.recent_files.files().get(file_index) {
                                    let path = entry.path.clone();
                                    self.save_current_session();
                                    if let Err(e) = self.viewer.load(&path) {
                                        tracing::error!("Failed to load recent file: {}", e);
                                    } else {
                                        self.recent_files.add(&path);
                                        let _ = self.recent_files.save();
                                        self.mode = ViewMode::Image;
                                        self.restore_session(&path);
                                        self.load_form_fields();
                                    }
                                }
                                self.recent_panel_active = false;
                            }
                            self.needs_redraw = true;
                        }
                        Key::Char('d') | Key::Backspace => {
                            // Remove selected from recent (only for actual files, not "Open File...")
                            if self.recent_panel_selected > 0 {
                                let file_index = self.recent_panel_selected - 1;
                                self.recent_files.remove_index(file_index);
                                // Adjust selection if needed
                                if self.recent_panel_selected > self.recent_files.len() {
                                    self.recent_panel_selected = self.recent_files.len().max(1) - 1 + 1;
                                    // Keep at least at "Open File..." (0) if no files left
                                    if self.recent_files.is_empty() {
                                        self.recent_panel_selected = 0;
                                    }
                                }
                            }
                            self.needs_redraw = true;
                        }
                        _ => {}
                    }
                    return Ok(true);
                }

                // Properties panel input
                if self.properties_panel_active {
                    match key_event.key {
                        Key::Escape | Key::Return | Key::Char('i') if key_event.modifiers.ctrl => {
                            self.properties_panel_active = false;
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
                            self.save_current_session();
                            return Ok(false); // Quit
                        }
                    }
                    Key::Char('q') => {
                        self.save_current_session();
                        return Ok(false);
                    }
                    // Copy selected text (Ctrl+C)
                    Key::Char('c') if key_event.modifiers.ctrl && !key_event.modifiers.shift => {
                        if let Some(text) = self.viewer.selected_text() {
                            if let Err(e) = self.copy_to_clipboard(text) {
                                tracing::error!("Failed to copy to clipboard: {}", e);
                            } else {
                                tracing::info!("Copied {} chars to clipboard", text.len());
                            }
                        }
                        return Ok(true);
                    }
                    // Copy image to clipboard (Ctrl+Shift+C)
                    Key::Char('C') if key_event.modifiers.ctrl && key_event.modifiers.shift => {
                        if let Err(e) = self.copy_image_to_clipboard() {
                            tracing::error!("Failed to copy image to clipboard: {}", e);
                        } else {
                            tracing::info!("Copied image to clipboard");
                        }
                        return Ok(true);
                    }
                    // Save document with form changes (Ctrl+S)
                    Key::Char('s') if key_event.modifiers.ctrl => {
                        let should_save = self.form_state.as_ref().is_some_and(|fs| fs.modified);
                        if should_save {
                            // Clone path to avoid borrow conflict
                            let path = self.viewer.current_path().map(|p| p.to_path_buf());
                            if let Some(path) = path {
                                let save_result = self.viewer.backend_mut()
                                    .map(|b| b.save_document(&path));
                                match save_result {
                                    Some(Ok(())) => {
                                        tracing::info!("Saved document: {}", path.display());
                                        // Clear modified flag
                                        if let Some(ref mut fs) = self.form_state {
                                            fs.modified = false;
                                        }
                                        // Invalidate page cache so poppler re-renders with new values
                                        self.viewer.invalidate_page_cache();
                                        self.needs_redraw = true;
                                    }
                                    Some(Err(e)) => {
                                        tracing::error!("Failed to save document: {}", e);
                                    }
                                    None => {}
                                }
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
                    // Recent files (Ctrl+R)
                    Key::Char('r') if key_event.modifiers.ctrl => {
                        self.recent_panel_active = !self.recent_panel_active;
                        self.recent_panel_selected = 0;
                        self.needs_redraw = true;
                        return Ok(true);
                    }
                    // Properties dialog (Ctrl+I)
                    Key::Char('i') if key_event.modifiers.ctrl => {
                        self.properties_panel_active = !self.properties_panel_active;
                        self.needs_redraw = true;
                        return Ok(true);
                    }
                    // Toggle annotation mode (Ctrl+A)
                    Key::Char('a') if key_event.modifiers.ctrl => {
                        self.toggle_annotation_mode()?;
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
                // Annotation mode mouse handling
                if self.annotation_mode {
                    if let Some(ref mut ann) = self.annotation {
                        if let Ok(redraw) = ann.handle_event(&InputEvent::MousePress(mouse_event.clone())) {
                            if redraw {
                                self.needs_redraw = true;
                            }
                        }
                    }
                    return Ok(true);
                }

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

                    // Check for form field click
                    if self.form_state.is_some() {
                        let pdf_coords = self.screen_to_pdf_coords(
                            mouse_event.position.x as f64,
                            mouse_event.position.y as f64,
                        );
                        let current_page = self.viewer.current_page();

                        if let Some((pdf_x, pdf_y)) = pdf_coords {
                            // First, check if we clicked on a form field
                            let clicked_field = if let Some(ref form_state) = self.form_state {
                                form_state.field_at_position(current_page, pdf_x, pdf_y)
                                    .map(|f| (f.id, f.field_type.clone()))
                            } else {
                                None
                            };

                            if let Some((field_id, field_type)) = clicked_field {
                                let now = std::time::Instant::now();

                                // Commit current field before switching (if different field)
                                if let Some(ref mut form_state) = self.form_state {
                                    if form_state.focused_field != Some(field_id) {
                                        if let Some((fid, value)) = form_state.commit_current() {
                                            if let Some(backend) = self.viewer.backend_mut() {
                                                let _ = backend.set_form_field_value(fid, value);
                                            }
                                        }
                                    }
                                }

                                // Check for double-click on checkbox/radio
                                let is_double_click = self.last_form_click
                                    .map(|(time, last_id)| {
                                        last_id == field_id && now.duration_since(time).as_millis() < 400
                                    })
                                    .unwrap_or(false);

                                if is_double_click {
                                    // Double-click: toggle checkbox/radio immediately
                                    match field_type {
                                        crate::forms::FormFieldType::Checkbox { .. }
                                        | crate::forms::FormFieldType::RadioButton { .. } => {
                                            if let Some(ref mut form_state) = self.form_state {
                                                form_state.focus_field(field_id);
                                                if let Some((fid, value)) = form_state.toggle_checkbox() {
                                                    if let Some(backend) = self.viewer.backend_mut() {
                                                        let _ = backend.set_form_field_value(fid, value);
                                                    }
                                                }
                                            }
                                            self.last_form_click = None;
                                            self.needs_redraw = true;
                                            return Ok(true);
                                        }
                                        _ => {}
                                    }
                                }

                                // Single click: focus the field
                                if let Some(ref mut form_state) = self.form_state {
                                    if form_state.focus_field(field_id) {
                                        self.last_form_click = Some((now, field_id));
                                        self.needs_redraw = true;
                                        return Ok(true);
                                    }
                                }
                            } else {
                                // Clicked outside any form field - commit and unfocus current field
                                if let Some(ref mut form_state) = self.form_state {
                                    if form_state.focused_field.is_some() {
                                        if let Some((fid, value)) = form_state.unfocus(true) {
                                            if let Some(backend) = self.viewer.backend_mut() {
                                                let _ = backend.set_form_field_value(fid, value);
                                            }
                                        }
                                        self.last_form_click = None;
                                        self.needs_redraw = true;
                                    }
                                }
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
                // Annotation mode mouse handling
                if self.annotation_mode {
                    if let Some(ref mut ann) = self.annotation {
                        if let Ok(redraw) = ann.handle_event(&InputEvent::MouseRelease(mouse_event.clone())) {
                            if redraw {
                                self.needs_redraw = true;
                            }
                        }
                    }
                    return Ok(true);
                }

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
                // Annotation mode mouse handling
                if self.annotation_mode {
                    if let Some(ref mut ann) = self.annotation {
                        if let Ok(redraw) = ann.handle_event(&InputEvent::MouseMove(mouse_event.clone())) {
                            if redraw {
                                self.needs_redraw = true;
                            }
                        }
                    }
                    return Ok(true);
                }

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
                self.save_current_session();
                if let Err(e) = self.viewer.next_image() {
                    tracing::error!("Failed to load next image: {}", e);
                }
                self.needs_redraw = true;
            }
            Key::Left | Key::Char('p') => {
                self.save_current_session();
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

            // Document view mode (V cycles: Single -> Continuous -> Dual)
            Key::Char('V') => {
                if self.viewer.is_multipage() {
                    self.viewer.cycle_view_mode();
                    let mode_name = match self.viewer.view_mode() {
                        crate::viewer::DocumentViewMode::SinglePage => "Single Page",
                        crate::viewer::DocumentViewMode::Continuous => "Continuous",
                        crate::viewer::DocumentViewMode::DualPage => "Dual Page",
                    };
                    tracing::info!("View mode: {}", mode_name);
                    self.needs_redraw = true;
                }
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
                    // Save current session before loading new file
                    self.save_current_session();
                    if let Err(e) = self.viewer.load(&path) {
                        tracing::error!("Failed to load image: {}", e);
                    } else {
                        self.recent_files.add(&path);
                        let _ = self.recent_files.save();
                        self.mode = ViewMode::Image;
                        // Restore session for the newly loaded file
                        self.restore_session(&path);
                        self.load_form_fields();
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

            // Format filter
            Key::Char('t') => {
                // Cycle through format filters: All -> Images -> Documents -> Comics -> Ebooks
                gallery.next_filter();
                self.needs_redraw = true;
            }
            Key::Char('T') => {
                // Reverse cycle through format filters
                gallery.prev_filter();
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

    /// Copy current image/page to clipboard as PNG
    fn copy_image_to_clipboard(&mut self) -> Result<()> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        // Get PNG data from viewer
        let png_data = self.viewer.current_page_png()?;

        // Use xclip to copy image data
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "image/png"])
            .stdin(Stdio::piped())
            .spawn()
            .context("Failed to spawn xclip - is it installed?")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&png_data).context("Failed to write PNG to xclip")?;
        }

        child.wait().context("xclip failed")?;
        Ok(())
    }

    /// Open a file dialog using garfield in picker mode
    fn open_file_dialog(&self) -> Result<Option<PathBuf>> {
        use std::process::{Command, Stdio};

        // Supported file formats filter (semicolon-separated glob patterns)
        const FILE_FILTER: &str = "*.png;*.jpg;*.jpeg;*.gif;*.webp;*.bmp;*.tiff;*.tif;\
            *.ico;*.avif;*.qoi;*.ppm;*.pgm;*.pbm;*.tga;*.dds;*.exr;*.ff;*.apng;\
            *.svg;*.svgz;*.pdf";

        // Get parent window ID for proper focus/raise behavior
        let parent_window = format!("{}", self.window.id());

        // Spawn garfield in picker mode with filters and parent window
        // Note: clap converts underscores to hyphens in long flags
        let output = Command::new("garfield")
            .args([
                "--picker",
                "--filter", FILE_FILTER,
                "--parent-window", &parent_window,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to spawn garfield --picker")?;

        // Log any stderr for debugging
        if !output.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("garfield stderr: {}", stderr);
        }

        // Exit code 0 = file selected, 1 = cancelled
        if !output.status.success() {
            return Ok(None); // User cancelled
        }

        // Read first line of stdout (the selected file path)
        let path_str = String::from_utf8_lossy(&output.stdout);
        let path_str = path_str.trim();

        if path_str.is_empty() {
            return Ok(None);
        }

        Ok(Some(PathBuf::from(path_str)))
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

    /// Handle an IPC command
    fn handle_ipc_command(&mut self, cmd: IpcCommand) -> garview_ipc::Response {
        use garview_ipc::Response;

        match cmd {
            IpcCommand::Open(path) => {
                self.save_current_session();
                match self.viewer.load(&path) {
                    Ok(()) => {
                        self.recent_files.add(&path);
                        let _ = self.recent_files.save();
                        self.mode = ViewMode::Image;
                        self.restore_session(&path);
                        self.load_form_fields();
                        Response::ok_with_message(format!("Opened: {}", path.display()))
                    }
                    Err(e) => Response::error(format!("Failed to open: {}", e)),
                }
            }
            IpcCommand::Close => {
                self.save_current_session();
                // No explicit close method - just acknowledge
                Response::ok_with_message("File closed")
            }
            IpcCommand::Next => {
                self.save_current_session();
                match self.viewer.next_image() {
                    Ok(_) => Response::ok(),
                    Err(e) => Response::error(e.to_string()),
                }
            }
            IpcCommand::Prev => {
                self.save_current_session();
                match self.viewer.prev_image() {
                    Ok(_) => Response::ok(),
                    Err(e) => Response::error(e.to_string()),
                }
            }
            IpcCommand::First => {
                self.save_current_session();
                self.viewer.first_page();
                Response::ok()
            }
            IpcCommand::Last => {
                self.save_current_session();
                self.viewer.last_page();
                Response::ok()
            }
            IpcCommand::Goto(page) => {
                if self.viewer.goto_page(page.saturating_sub(1)) {
                    Response::ok()
                } else {
                    Response::error("Invalid page number")
                }
            }
            IpcCommand::ZoomIn => {
                self.viewer.zoom.zoom_in();
                Response::ok()
            }
            IpcCommand::ZoomOut => {
                self.viewer.zoom.zoom_out();
                Response::ok()
            }
            IpcCommand::ZoomFit => {
                self.viewer.zoom.mode = ZoomMode::Fit;
                Response::ok()
            }
            IpcCommand::ZoomActual => {
                self.viewer.zoom.mode = ZoomMode::OneToOne;
                self.viewer.zoom.level = 1.0;
                Response::ok()
            }
            IpcCommand::ZoomSet(level) => {
                self.viewer.zoom.mode = ZoomMode::Custom(level / 100.0);
                self.viewer.zoom.level = level / 100.0;
                Response::ok()
            }
            IpcCommand::RotateCw => {
                self.viewer.rotate_cw();
                Response::ok()
            }
            IpcCommand::RotateCcw => {
                self.viewer.rotate_ccw();
                Response::ok()
            }
            IpcCommand::FlipH => {
                self.viewer.flip_horizontal();
                Response::ok()
            }
            IpcCommand::FlipV => {
                self.viewer.flip_vertical();
                Response::ok()
            }
            IpcCommand::Fullscreen => {
                let _ = self.toggle_fullscreen();
                Response::ok()
            }
            IpcCommand::Sidebar => {
                self.sidebar.toggle();
                Response::ok()
            }
            IpcCommand::SlideshowStart => {
                self.slideshow.active = true;
                self.slideshow.last_advance = std::time::Instant::now();
                Response::ok_with_message("Slideshow started")
            }
            IpcCommand::SlideshowStop => {
                self.slideshow.active = false;
                Response::ok_with_message("Slideshow stopped")
            }
            IpcCommand::SlideshowInterval(seconds) => {
                self.slideshow.interval = std::time::Duration::from_secs_f64(seconds);
                Response::ok_with_message(format!("Slideshow interval: {}s", seconds))
            }
            IpcCommand::GetInfo => {
                let info = create_viewer_info(
                    self.viewer.current_path(),
                    self.viewer.current_page() + 1, // 1-indexed for display
                    self.viewer.page_count(),
                    self.viewer.zoom.level,
                    self.viewer.effective_size().map(|(w, h)| (w as u32, h as u32)),
                    self.fullscreen,
                    self.slideshow.active,
                );
                Response::ok_with_data(serde_json::to_value(info).unwrap_or_default())
            }
            IpcCommand::Quit => {
                // This will be handled by returning false from handle_event
                // For now, just acknowledge
                Response::ok_with_message("Quitting...")
            }
        }
    }

    /// Toggle annotation mode
    fn toggle_annotation_mode(&mut self) -> Result<()> {
        if self.annotation_mode {
            // Exit annotation mode
            self.annotation_mode = false;
            self.annotation = None;
            tracing::info!("Exited annotation mode");
        } else {
            // Enter annotation mode - create annotation manager from current image
            if let Some(rendered) = self.viewer.current_rendered_page() {
                let ann = AnnotationManager::new(&rendered.data, rendered.width, rendered.height)?;
                self.annotation = Some(ann);
                self.annotation_mode = true;
                tracing::info!("Entered annotation mode - use tools: b=brush, l=line, a=arrow, r=rect, e=ellipse, h=highlight");
                tracing::info!("Colors: 1-9, Size: +/-, Fill: f, Undo: Ctrl+Z, Save: Ctrl+S, Exit: Esc");
            } else {
                tracing::warn!("Cannot enter annotation mode: no image loaded");
            }
        }
        self.needs_redraw = true;
        Ok(())
    }

    /// Save the annotated image
    fn save_annotated_image(&mut self) -> Result<()> {
        let ann = self.annotation.as_mut().context("No annotation in progress")?;

        // Export annotated image
        let data = ann.export()?;
        let width = ann.canvas.width();
        let height = ann.canvas.height();

        // Determine output path
        let input_path = self.viewer.current_path().context("No file loaded")?;
        let stem = input_path.file_stem().unwrap_or_default().to_string_lossy();
        let parent = input_path.parent().unwrap_or(Path::new("."));
        let output_path = parent.join(format!("{}_annotated.png", stem));

        // Save as PNG
        let img = image::RgbaImage::from_raw(width, height, data)
            .context("Failed to create image from annotation data")?;
        img.save(&output_path).context("Failed to save annotated image")?;

        tracing::info!("Saved annotated image to: {}", output_path.display());

        // Exit annotation mode after saving
        self.annotation_mode = false;
        self.annotation = None;

        Ok(())
    }

    fn render(&mut self) -> Result<()> {
        let size = self.renderer.size();
        let viewport_height = size.height.saturating_sub(STATUS_BAR_HEIGHT);

        // Clear background
        self.renderer.clear()?;

        // Annotation mode rendering
        if self.annotation_mode {
            if let Some(ref ann) = self.annotation {
                // Render the annotation canvas
                if let Err(e) = ann.render() {
                    tracing::error!("Failed to render annotation: {}", e);
                }

                // Draw composite to window
                let composite = ann.canvas.composite_surface();
                let canvas_w = ann.canvas.width() as f64;
                let canvas_h = ann.canvas.height() as f64;

                // Center in viewport
                let x = if canvas_w < size.width as f64 {
                    (size.width as f64 - canvas_w) / 2.0
                } else {
                    0.0
                };
                let y = if canvas_h < viewport_height as f64 {
                    (viewport_height as f64 - canvas_h) / 2.0
                } else {
                    0.0
                };

                let ctx = self.renderer.context()?;
                ctx.set_source_surface(composite.cairo_surface(), x, y)?;
                ctx.paint()?;

                // Draw annotation toolbar at top
                self.render_annotation_toolbar(&ctx, size.width, &ann)?;
            }
        } else {
            match self.mode {
                ViewMode::Gallery => self.render_gallery(viewport_height)?,
                ViewMode::Image => self.render_image(viewport_height)?,
            }
        }

        // Render status bar
        match self.mode {
            ViewMode::Image => {
                // For multi-page documents, show page position; otherwise show directory position
                let pos = self.viewer.page_position()
                    .or_else(|| self.viewer.directory_position());
                // Check if form has unsaved changes
                let modified = self.form_state.as_ref().is_some_and(|fs| fs.modified);
                self.statusbar.render_full(
                    &self.renderer,
                    size.width,
                    viewport_height,
                    self.viewer.file_info().as_ref(),
                    self.viewer.zoom.level,
                    self.viewer.zoom.mode,
                    pos,
                    None,
                    modified,
                )?;
            }
            ViewMode::Gallery => {
                let (pos, filter_name) = self.gallery.as_ref()
                    .map(|g| ((g.selection_index() + 1, g.file_count()), g.format_filter().name()))
                    .unwrap_or(((0, 0), "All"));
                self.statusbar.render_with_filter(
                    &self.renderer,
                    size.width,
                    viewport_height,
                    None,
                    1.0,
                    crate::viewer::ZoomMode::Fit,
                    Some(pos),
                    Some(filter_name),
                )?;
            }
        }

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

        // Render recent files panel if active
        if self.recent_panel_active {
            self.render_recent_panel(size.width, size.height)?;
        }

        if self.properties_panel_active {
            self.render_properties_panel(size.width, size.height)?;
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
                self.render_dual_page(viewport_height)
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

                // Render form fields if present
                if self.form_state.is_some() {
                    if let Some(page_size) = self.viewer.page_size_for(current_page) {
                        self.render_form_fields(
                            current_page,
                            x,
                            y,
                            zoom,
                            page_size.height,
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

    fn render_dual_page(&mut self, viewport_height: u32) -> Result<()> {
        let size = self.renderer.size();
        let page_count = self.viewer.page_count();
        let current_page = self.viewer.current_page();

        // In dual page mode, show even page on left, odd on right
        // If current page is odd (1, 3, 5...), show previous page + current
        // If current page is even (0, 2, 4...), show current + next
        let (left_page, right_page) = if current_page % 2 == 0 {
            (current_page, current_page + 1)
        } else {
            (current_page.saturating_sub(1), current_page)
        };

        // Get page sizes
        let left_size = self.viewer.page_size_for(left_page);
        let right_size = if right_page < page_count {
            self.viewer.page_size_for(right_page)
        } else {
            None
        };

        // Calculate combined dimensions
        let (left_w, left_h) = left_size.map(|s| (s.width, s.height)).unwrap_or((1.0, 1.0));
        let (right_w, right_h) = right_size.map(|s| (s.width, s.height)).unwrap_or((0.0, 0.0));

        let page_gap = 20.0; // Gap between pages
        let total_width = left_w + (if right_size.is_some() { right_w + page_gap } else { 0.0 });
        let max_height = left_h.max(right_h);

        // Update zoom for combined dimensions
        self.viewer.zoom.update(total_width, max_height, size.width as f64, viewport_height as f64);

        let zoom = self.viewer.zoom.level;
        let scaled_total_w = total_width * zoom;
        let scaled_max_h = max_height * zoom;
        let scaled_left_w = left_w * zoom;
        let scaled_left_h = left_h * zoom;
        let scaled_right_w = right_w * zoom;
        let scaled_right_h = right_h * zoom;
        let scaled_gap = page_gap * zoom;

        // Clamp scroll
        self.viewer.scroll.clamp(
            scaled_total_w,
            scaled_max_h,
            size.width as f64,
            viewport_height as f64,
        );

        let offset_x = self.viewer.scroll.offset_x;
        let offset_y = self.viewer.scroll.offset_y;

        // Calculate starting x position (centered if smaller than viewport)
        let start_x = if scaled_total_w < size.width as f64 {
            (size.width as f64 - scaled_total_w) / 2.0
        } else {
            -offset_x
        };

        let ctx = self.renderer.context()?;
        ctx.save()?;

        // Clip to viewport
        ctx.rectangle(0.0, 0.0, size.width as f64, viewport_height as f64);
        ctx.clip();

        // Render left page
        if let Ok(left_surface) = self.viewer.render_page_surface(left_page, zoom) {
            let surface_w = left_surface.width() as f64;
            let display_scale = scaled_left_w / surface_w;

            // Center vertically if page is shorter than max height
            let y = if scaled_left_h < viewport_height as f64 {
                (viewport_height as f64 - scaled_left_h) / 2.0
            } else {
                -offset_y + (scaled_max_h - scaled_left_h) / 2.0
            };

            ctx.save()?;
            ctx.translate(start_x, y);
            ctx.scale(display_scale, display_scale);
            ctx.set_source_surface(left_surface.cairo_surface(), 0.0, 0.0)?;
            let filter = if display_scale > 2.0 {
                gartk_render::cairo::Filter::Nearest
            } else {
                gartk_render::cairo::Filter::Bilinear
            };
            ctx.source().set_filter(filter);
            ctx.paint()?;
            ctx.restore()?;
        }

        // Render right page (if exists)
        if right_page < page_count {
            if let Ok(right_surface) = self.viewer.render_page_surface(right_page, zoom) {
                let surface_w = right_surface.width() as f64;
                let display_scale = scaled_right_w / surface_w;

                // Position right page after left page + gap
                let x = start_x + scaled_left_w + scaled_gap;
                let y = if scaled_right_h < viewport_height as f64 {
                    (viewport_height as f64 - scaled_right_h) / 2.0
                } else {
                    -offset_y + (scaled_max_h - scaled_right_h) / 2.0
                };

                ctx.save()?;
                ctx.translate(x, y);
                ctx.scale(display_scale, display_scale);
                ctx.set_source_surface(right_surface.cairo_surface(), 0.0, 0.0)?;
                let filter = if display_scale > 2.0 {
                    gartk_render::cairo::Filter::Nearest
                } else {
                    gartk_render::cairo::Filter::Bilinear
                };
                ctx.source().set_filter(filter);
                ctx.paint()?;
                ctx.restore()?;
            }
        }

        ctx.restore()?;

        // Show page indicator overlay for dual-page mode
        let page_text = if right_page < page_count {
            format!("{}-{}/{}", left_page + 1, right_page + 1, page_count)
        } else {
            format!("{}/{}", left_page + 1, page_count)
        };

        // Draw page indicator in corner
        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Normal,
        );
        ctx.set_font_size(12.0);
        let extents = ctx.text_extents(&page_text)?;
        let pad = 4.0;
        let pill_w = extents.width() + pad * 2.0;
        let pill_h = extents.height() + pad * 2.0;
        let pill_x = size.width as f64 - pill_w - 10.0;
        let pill_y = 10.0;

        // Background pill
        ctx.new_path();
        let radius = pill_h / 2.0;
        ctx.arc(pill_x + radius, pill_y + radius, radius, 0.5 * std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        ctx.arc(pill_x + pill_w - radius, pill_y + radius, radius, 1.5 * std::f64::consts::PI, 0.5 * std::f64::consts::PI);
        ctx.close_path();
        ctx.set_source_rgba(0.0, 0.0, 0.0, 0.6);
        ctx.fill()?;

        // Text
        ctx.set_source_rgb(1.0, 1.0, 1.0);
        ctx.move_to(pill_x + pad, pill_y + pad + extents.height());
        ctx.show_text(&page_text)?;

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

    /// Render form field boundaries and content overlays
    #[allow(clippy::too_many_arguments)]
    fn render_form_fields(
        &self,
        page: usize,
        x: f64,
        y: f64,
        zoom: f64,
        page_height: f64,
        viewport_width: f64,
        viewport_height: f64,
    ) -> Result<()> {
        let form_state = match &self.form_state {
            Some(fs) => fs,
            None => return Ok(()),
        };

        // Only render overlay for the focused field - let poppler render unfocused fields natively
        let focused_id = match form_state.focused_field {
            Some(id) => id,
            None => return Ok(()), // No focused field = no overlay needed
        };

        let field = match form_state.field_by_id(focused_id) {
            Some(f) => f,
            None => return Ok(()),
        };

        // Only render if field is on current page
        if field.page != page {
            return Ok(());
        }

        let ctx = self.renderer.context()?;
        ctx.save()?;

        // Clip to viewport
        ctx.rectangle(0.0, 0.0, viewport_width, viewport_height);
        ctx.clip();

        // Translate to page position
        ctx.translate(x, y);

        // Calculate field rectangle
        let (pdf_x1, pdf_y1, pdf_x2, pdf_y2) = field.rect;
        let screen_y1 = page_height - pdf_y2;
        let screen_y2 = page_height - pdf_y1;
        let rect_x = pdf_x1 * zoom;
        let rect_y = screen_y1 * zoom;
        let rect_w = (pdf_x2 - pdf_x1) * zoom;
        let rect_h = (screen_y2 - screen_y1) * zoom;

        // Draw focus highlight
        ctx.set_source_rgba(0.2, 0.4, 0.8, 0.3);
        ctx.rectangle(rect_x, rect_y, rect_w, rect_h);
        ctx.fill()?;

        ctx.set_source_rgba(0.2, 0.4, 0.8, 0.9);
        ctx.set_line_width(2.0);
        ctx.rectangle(rect_x, rect_y, rect_w, rect_h);
        ctx.stroke()?;

        // Draw field content based on type
        match &field.field_type {
            crate::forms::FormFieldType::Text { max_len, .. } => {
                let text = &form_state.text_buffer;
                let uses_char_spacing = max_len.map(|m| m > 0 && m <= 10).unwrap_or(false);

                // Always cover PDF's native text with white fill for focused field
                ctx.set_source_rgb(1.0, 1.0, 1.0);
                let inset = 1.0 * zoom;
                ctx.rectangle(rect_x + inset, rect_y + inset, rect_w - 2.0 * inset, rect_h - 2.0 * inset);
                ctx.fill()?;

                // For character-spaced fields, redraw the grid lines we just covered
                if uses_char_spacing {
                    if let Some(max) = max_len {
                        let char_width = rect_w / (*max as f64);
                        ctx.set_source_rgba(0.3, 0.3, 0.3, 0.8);
                        ctx.set_line_width(1.0);
                        for i in 1..*max {
                            let line_x = rect_x + (i as f64 * char_width);
                            ctx.move_to(line_x, rect_y + inset);
                            ctx.line_to(line_x, rect_y + rect_h - inset);
                        }
                        ctx.stroke()?;
                    }
                }

                // Calculate font size
                let font_size = if field.font_size > 0.0 {
                    field.font_size * zoom
                } else {
                    (rect_h * 0.7).min(14.0 * zoom)
                };

                ctx.select_font_face(
                    "Sans",
                    gartk_render::cairo::FontSlant::Normal,
                    gartk_render::cairo::FontWeight::Normal,
                );
                ctx.set_font_size(font_size);

                // Center text vertically
                let extents = ctx.font_extents()?;
                let text_y = rect_y + (rect_h + extents.height()) / 2.0 - extents.descent();

                // Render text
                if !text.is_empty() {
                    ctx.set_source_rgb(0.0, 0.0, 0.0);

                    if let Some(max) = max_len {
                        if *max > 0 && *max <= 10 {
                            // Fixed character spacing
                            let char_width = rect_w / (*max as f64);
                            for (i, ch) in text.chars().enumerate() {
                                if i >= *max as usize {
                                    break;
                                }
                                let ch_str = ch.to_string();
                                let ch_extents = ctx.text_extents(&ch_str)?;
                                let char_x = rect_x + (i as f64 * char_width) + (char_width - ch_extents.width()) / 2.0;
                                ctx.move_to(char_x, text_y);
                                ctx.show_text(&ch_str)?;
                            }
                        } else {
                            ctx.move_to(rect_x + 2.0 * zoom, text_y);
                            ctx.show_text(text)?;
                        }
                    } else {
                        ctx.move_to(rect_x + 2.0 * zoom, text_y);
                        ctx.show_text(text)?;
                    }
                }

                // Draw cursor
                let cursor_pos = form_state.cursor_pos.min(form_state.text_buffer.len());
                let cursor_x = if let Some(max) = max_len {
                    if *max > 0 && *max <= 10 {
                        let char_width = rect_w / (*max as f64);
                        rect_x + (cursor_pos as f64 * char_width)
                    } else {
                        let cursor_text = &form_state.text_buffer[..cursor_pos];
                        rect_x + 2.0 * zoom + ctx.text_extents(cursor_text)?.x_advance()
                    }
                } else {
                    let cursor_text = &form_state.text_buffer[..cursor_pos];
                    rect_x + 2.0 * zoom + ctx.text_extents(cursor_text)?.x_advance()
                };
                ctx.set_source_rgba(0.0, 0.0, 0.0, 0.8);
                ctx.set_line_width(1.0);
                ctx.move_to(cursor_x, rect_y + 2.0);
                ctx.line_to(cursor_x, rect_y + rect_h - 2.0);
                ctx.stroke()?;
            }
            crate::forms::FormFieldType::Checkbox { checked } => {
                // Only draw focus indicator; poppler handles the checkmark
                // But show our state indicator for immediate feedback during toggle
                if *checked {
                    ctx.set_source_rgba(0.2, 0.6, 0.2, 0.9);
                    let cx = rect_x + rect_w / 2.0;
                    let cy = rect_y + rect_h / 2.0;
                    let size = (rect_w.min(rect_h) * 0.4).min(10.0 * zoom);
                    ctx.set_line_width(2.0);
                    ctx.move_to(cx - size * 0.5, cy);
                    ctx.line_to(cx - size * 0.1, cy + size * 0.4);
                    ctx.line_to(cx + size * 0.5, cy - size * 0.4);
                    ctx.stroke()?;
                }
            }
            crate::forms::FormFieldType::RadioButton { selected, .. } => {
                // Only draw focus indicator; poppler handles the fill
                if *selected {
                    ctx.set_source_rgba(0.2, 0.6, 0.2, 0.9);
                    let cx = rect_x + rect_w / 2.0;
                    let cy = rect_y + rect_h / 2.0;
                    let radius = (rect_w.min(rect_h) * 0.3).min(6.0 * zoom);
                    ctx.arc(cx, cy, radius, 0.0, 2.0 * std::f64::consts::PI);
                    ctx.fill()?;
                }
            }
            crate::forms::FormFieldType::Dropdown { .. } => {
                // Just show focus highlight for dropdowns
            }
            _ => {}
        }

        ctx.restore()?;
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

    fn render_recent_panel(&mut self, width: u32, height: u32) -> Result<()> {
        let ctx = self.renderer.context()?;

        // Panel dimensions - centered, takes up most of the screen
        let panel_width = 600.0_f64.min(width as f64 - 80.0);
        let panel_height = 400.0_f64.min(height as f64 - 80.0);
        let panel_x = (width as f64 - panel_width) / 2.0;
        let panel_y = (height as f64 - panel_height) / 2.0;
        let padding = 16.0;
        let radius = 8.0;
        let item_height = 28.0;
        let header_height = 40.0;

        ctx.save()?;

        // Draw semi-transparent overlay
        ctx.set_source_rgba(0.0, 0.0, 0.0, 0.5);
        ctx.rectangle(0.0, 0.0, width as f64, height as f64);
        ctx.fill()?;

        // Draw panel background with rounded corners
        ctx.new_path();
        ctx.arc(panel_x + radius, panel_y + radius, radius, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        ctx.arc(panel_x + panel_width - radius, panel_y + radius, radius, 1.5 * std::f64::consts::PI, 2.0 * std::f64::consts::PI);
        ctx.arc(panel_x + panel_width - radius, panel_y + panel_height - radius, radius, 0.0, 0.5 * std::f64::consts::PI);
        ctx.arc(panel_x + radius, panel_y + panel_height - radius, radius, 0.5 * std::f64::consts::PI, std::f64::consts::PI);
        ctx.close_path();

        // Fill background
        ctx.set_source_rgba(0.12, 0.12, 0.14, 0.98);
        ctx.fill_preserve()?;

        // Draw border
        ctx.set_source_rgb(0.3, 0.3, 0.35);
        ctx.set_line_width(1.0);
        ctx.stroke()?;

        // Draw header
        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Bold,
        );
        ctx.set_font_size(16.0);
        ctx.set_source_rgb(0.9, 0.9, 0.9);
        ctx.move_to(panel_x + padding, panel_y + padding + 20.0);
        ctx.show_text("Recent Files")?;

        // Draw hint
        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Normal,
        );
        ctx.set_font_size(11.0);
        ctx.set_source_rgb(0.5, 0.5, 0.5);
        let hint = "j/k: navigate  Enter: open  d: remove  Esc: close";
        let hint_extents = ctx.text_extents(hint)?;
        ctx.move_to(panel_x + panel_width - padding - hint_extents.width(), panel_y + padding + 20.0);
        ctx.show_text(hint)?;

        // Draw separator
        ctx.set_source_rgb(0.25, 0.25, 0.28);
        ctx.set_line_width(1.0);
        ctx.move_to(panel_x + padding, panel_y + header_height);
        ctx.line_to(panel_x + panel_width - padding, panel_y + header_height);
        ctx.stroke()?;

        // Draw file list
        let list_y = panel_y + header_height + 8.0;
        let max_visible = ((panel_height - header_height - 16.0) / item_height) as usize;
        let total_items = self.recent_files.len() + 1; // +1 for "Open File..."

        ctx.set_font_size(13.0);

        // Draw "Open File..." as first item (index 0)
        {
            let y = list_y + item_height;

            // Highlight if selected
            if self.recent_panel_selected == 0 {
                ctx.set_source_rgba(0.2, 0.4, 0.7, 0.5);
                ctx.rectangle(
                    panel_x + padding - 4.0,
                    y - item_height + 8.0,
                    panel_width - padding * 2.0 + 8.0,
                    item_height,
                );
                ctx.fill()?;
            }

            // "Open File..." text with accent color
            ctx.set_source_rgb(0.5, 0.75, 0.95);
            ctx.move_to(panel_x + padding, y);
            ctx.show_text("Open File...")?;

            // Hint text
            ctx.set_source_rgb(0.45, 0.45, 0.5);
            ctx.set_font_size(11.0);
            ctx.move_to(panel_x + padding + 100.0, y);
            ctx.show_text("Browse with garfield")?;
            ctx.set_font_size(13.0);
        }

        // Draw recent files (starting at visual index 1, selection index 1+)
        if self.recent_files.is_empty() {
            ctx.set_source_rgb(0.5, 0.5, 0.5);
            ctx.move_to(panel_x + padding, list_y + 2.0 * item_height);
            ctx.show_text("No recent files")?;
        } else {
            for (i, entry) in self.recent_files.files().iter().take(max_visible.saturating_sub(1)).enumerate() {
                let visual_index = i + 1; // Offset by 1 for "Open File..."
                let y = list_y + (visual_index as f64 + 1.0) * item_height;

                // Highlight selected
                if visual_index == self.recent_panel_selected {
                    ctx.set_source_rgba(0.2, 0.4, 0.7, 0.5);
                    ctx.rectangle(
                        panel_x + padding - 4.0,
                        y - item_height + 8.0,
                        panel_width - padding * 2.0 + 8.0,
                        item_height,
                    );
                    ctx.fill()?;
                }

                // File name
                ctx.set_source_rgb(0.85, 0.85, 0.85);
                ctx.move_to(panel_x + padding, y);
                let filename = entry.filename();
                // Truncate long filenames
                let display_name = if filename.len() > 50 {
                    format!("{}...", &filename[..47])
                } else {
                    filename.to_string()
                };
                ctx.show_text(&display_name)?;

                // Path (dimmed)
                ctx.set_source_rgb(0.45, 0.45, 0.5);
                ctx.set_font_size(11.0);
                let parent = entry.path.parent()
                    .and_then(|p| p.to_str())
                    .unwrap_or("");
                let display_path = if parent.len() > 60 {
                    format!("...{}", &parent[parent.len()-57..])
                } else {
                    parent.to_string()
                };
                let name_extents = ctx.text_extents(&display_name)?;
                ctx.move_to(panel_x + padding + name_extents.width() + 16.0, y);
                ctx.show_text(&display_path)?;

                // Time ago (right-aligned)
                let time_str = format_time_ago(entry.last_opened);
                let time_extents = ctx.text_extents(&time_str)?;
                ctx.move_to(panel_x + panel_width - padding - time_extents.width(), y);
                ctx.show_text(&time_str)?;

                ctx.set_font_size(13.0);
            }
        }

        ctx.restore()?;
        Ok(())
    }

    fn render_properties_panel(&mut self, width: u32, height: u32) -> Result<()> {
        let ctx = self.renderer.context()?;

        // Get file info
        let file_info = self.viewer.file_info();

        // Panel dimensions - centered, smaller than recent panel
        let panel_width = 450.0_f64.min(width as f64 - 80.0);
        let panel_height = 280.0_f64.min(height as f64 - 80.0);
        let panel_x = (width as f64 - panel_width) / 2.0;
        let panel_y = (height as f64 - panel_height) / 2.0;
        let padding = 20.0;
        let radius = 8.0;
        let row_height = 32.0;
        let header_height = 48.0;

        ctx.save()?;

        // Draw semi-transparent overlay
        ctx.set_source_rgba(0.0, 0.0, 0.0, 0.5);
        ctx.rectangle(0.0, 0.0, width as f64, height as f64);
        ctx.fill()?;

        // Draw panel background with rounded corners
        ctx.new_path();
        ctx.arc(panel_x + radius, panel_y + radius, radius, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        ctx.arc(panel_x + panel_width - radius, panel_y + radius, radius, 1.5 * std::f64::consts::PI, 2.0 * std::f64::consts::PI);
        ctx.arc(panel_x + panel_width - radius, panel_y + panel_height - radius, radius, 0.0, 0.5 * std::f64::consts::PI);
        ctx.arc(panel_x + radius, panel_y + panel_height - radius, radius, 0.5 * std::f64::consts::PI, std::f64::consts::PI);
        ctx.close_path();

        // Fill background
        ctx.set_source_rgba(0.12, 0.12, 0.14, 0.98);
        ctx.fill_preserve()?;

        // Draw border
        ctx.set_source_rgb(0.3, 0.3, 0.35);
        ctx.set_line_width(1.0);
        ctx.stroke()?;

        // Draw header
        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Bold,
        );
        ctx.set_font_size(16.0);
        ctx.set_source_rgb(0.9, 0.9, 0.9);
        ctx.move_to(panel_x + padding, panel_y + padding + 20.0);
        ctx.show_text("Properties")?;

        // Draw hint
        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Normal,
        );
        ctx.set_font_size(11.0);
        ctx.set_source_rgb(0.5, 0.5, 0.5);
        let hint = "Esc/Enter: close";
        let hint_extents = ctx.text_extents(hint)?;
        ctx.move_to(panel_x + panel_width - padding - hint_extents.width(), panel_y + padding + 20.0);
        ctx.show_text(hint)?;

        // Draw separator
        ctx.set_source_rgb(0.25, 0.25, 0.28);
        ctx.set_line_width(1.0);
        ctx.move_to(panel_x + padding, panel_y + header_height);
        ctx.line_to(panel_x + panel_width - padding, panel_y + header_height);
        ctx.stroke()?;

        // Draw properties
        let content_y = panel_y + header_height + padding;
        let label_x = panel_x + padding;
        let value_x = panel_x + padding + 100.0;

        ctx.set_font_size(13.0);

        let properties: Vec<(&str, String)> = if let Some(info) = file_info {
            let mut props = vec![
                ("Name:", info.filename.clone()),
                ("Dimensions:", format!("{} x {}", info.width, info.height)),
                ("File size:", info.format_size()),
                ("Format:", info.format.clone()),
            ];

            // Add page count for multi-page documents
            let page_count = self.viewer.page_count();
            if page_count > 1 {
                props.push(("Pages:", format!("{}", page_count)));
            }

            // Add path
            if let Some(path) = self.viewer.current_path() {
                props.push(("Path:", path.display().to_string()));
            }

            props
        } else {
            vec![("No file loaded", String::new())]
        };

        for (i, (label, value)) in properties.iter().enumerate() {
            let y = content_y + (i as f64 + 1.0) * row_height;

            // Label
            ctx.set_source_rgb(0.6, 0.6, 0.65);
            ctx.move_to(label_x, y);
            ctx.show_text(label)?;

            // Value
            ctx.set_source_rgb(0.85, 0.85, 0.85);

            // Truncate long values (especially paths)
            let max_value_width = panel_width - 100.0 - padding * 2.0 - 20.0;
            let value_extents = ctx.text_extents(value)?;
            let display_value = if value_extents.width() > max_value_width {
                // Truncate from the beginning for paths
                let chars: Vec<char> = value.chars().collect();
                let mut truncated = String::from("...");
                for c in chars.iter().rev().take(40).collect::<Vec<_>>().into_iter().rev() {
                    truncated.push(*c);
                }
                truncated
            } else {
                value.clone()
            };

            ctx.move_to(value_x, y);
            ctx.show_text(&display_value)?;
        }

        ctx.restore()?;
        Ok(())
    }

    /// Save current session state (zoom, scroll, page) for the current file
    fn save_current_session(&mut self) {
        if let Some(path) = self.viewer.current_path() {
            let page = self.viewer.current_page();
            let zoom = self.viewer.zoom.level * 100.0; // Convert to percentage
            let scroll = (self.viewer.scroll.offset_x, self.viewer.scroll.offset_y);
            self.recent_files.update_session(&path, page, zoom, scroll);
            let _ = self.recent_files.save();
        }
    }

    /// Restore session state for a file if available
    fn restore_session(&mut self, path: &Path) {
        if let Some((page, zoom, scroll)) = self.recent_files.get_session(path) {
            // Restore page
            if page < self.viewer.page_count() {
                self.viewer.goto_page(page);
            }
            // Restore zoom (convert from percentage)
            self.viewer.zoom.mode = ZoomMode::Custom(zoom / 100.0);
            self.viewer.zoom.level = zoom / 100.0;
            // Restore scroll position
            self.viewer.scroll.offset_x = scroll.0;
            self.viewer.scroll.offset_y = scroll.1;
        }
    }

    /// Load form fields from the current document (if supported)
    fn load_form_fields(&mut self) {
        // Clear any existing form state
        self.form_state = None;

        // Check if backend supports forms
        if let Some(backend) = self.viewer.backend_mut() {
            if !backend.supports_forms() {
                return;
            }

            // Load form fields from all pages
            let mut state = FormState::new();
            let page_count = backend.page_count();

            for page in 0..page_count {
                let fields = backend.get_form_fields(page);
                state.fields.extend(fields);
            }

            if state.has_fields() {
                tracing::info!("Loaded {} form fields", state.fields.len());
                self.form_state = Some(state);
            }
        }
    }

    /// Render the annotation toolbar at the top of the screen
    fn render_annotation_toolbar(
        &self,
        ctx: &cairo::Context,
        width: u32,
        ann: &AnnotationManager,
    ) -> Result<()> {
        let toolbar_height = 40.0;
        let padding = 10.0;

        // Background
        ctx.set_source_rgba(0.1, 0.1, 0.12, 0.95);
        ctx.rectangle(0.0, 0.0, width as f64, toolbar_height);
        ctx.fill()?;

        // Border
        ctx.set_source_rgb(0.3, 0.3, 0.35);
        ctx.move_to(0.0, toolbar_height);
        ctx.line_to(width as f64, toolbar_height);
        ctx.stroke()?;

        // Tool buttons
        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Normal,
        );
        ctx.set_font_size(12.0);

        let tools = ToolType::all();
        let mut x = padding;

        for tool in tools {
            let is_active = ann.state.current_tool == *tool;
            let label = format!("[{}] {}", tool.shortcut().to_ascii_uppercase(), tool.name());

            // Background for active tool
            if is_active {
                ctx.set_source_rgba(0.3, 0.5, 0.8, 0.8);
                ctx.rectangle(x - 4.0, 6.0, 70.0, 28.0);
                ctx.fill()?;
            }

            // Text
            ctx.set_source_rgb(if is_active { 1.0 } else { 0.7 }, if is_active { 1.0 } else { 0.7 }, if is_active { 1.0 } else { 0.7 });
            ctx.move_to(x, 25.0);
            ctx.show_text(&label)?;

            x += 75.0;
        }

        // Separator
        ctx.set_source_rgb(0.3, 0.3, 0.35);
        ctx.move_to(x, 8.0);
        ctx.line_to(x, 32.0);
        ctx.stroke()?;
        x += padding;

        // Color preview
        let color = &ann.state.properties.color;
        ctx.set_source_rgba(color.r as f64, color.g as f64, color.b as f64, color.a as f64);
        ctx.rectangle(x, 10.0, 20.0, 20.0);
        ctx.fill()?;
        ctx.set_source_rgb(0.5, 0.5, 0.5);
        ctx.rectangle(x, 10.0, 20.0, 20.0);
        ctx.stroke()?;
        x += 30.0;

        // Line width
        ctx.set_source_rgb(0.7, 0.7, 0.7);
        ctx.move_to(x, 25.0);
        ctx.show_text(&format!("W:{:.0}", ann.state.properties.line_width))?;
        x += 45.0;

        // Fill mode
        ctx.move_to(x, 25.0);
        ctx.show_text(if ann.state.properties.fill { "[F]ill" } else { "[F]rame" })?;
        x += 55.0;

        // Undo/Redo status
        ctx.set_source_rgb(0.5, 0.5, 0.5);
        ctx.move_to(x, 25.0);
        ctx.show_text(&format!("Undo:{} Redo:{}", ann.history.undo_count(), ann.history.redo_count()))?;

        // Help text on right
        ctx.set_source_rgb(0.5, 0.5, 0.5);
        let help = "Esc:Exit  Ctrl+S:Save  1-9:Colors  +/-:Size";
        let extents = ctx.text_extents(help)?;
        ctx.move_to(width as f64 - extents.width() - padding, 25.0);
        ctx.show_text(help)?;

        Ok(())
    }
}

/// Format a SystemTime as a human-readable "time ago" string
fn format_time_ago(time: SystemTime) -> String {
    let now = SystemTime::now();
    let duration = now.duration_since(time).unwrap_or_default();
    let secs = duration.as_secs();

    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        let mins = secs / 60;
        format!("{}m ago", mins)
    } else if secs < 86400 {
        let hours = secs / 3600;
        format!("{}h ago", hours)
    } else if secs < 604800 {
        let days = secs / 86400;
        format!("{}d ago", days)
    } else {
        let weeks = secs / 604800;
        format!("{}w ago", weeks)
    }
}
