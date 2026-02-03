//! Sidebar component with page thumbnails and table of contents.

use anyhow::Result;
use gartk_render::Renderer;
use std::collections::HashMap;

/// Sidebar tab
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SidebarTab {
    #[default]
    Thumbnails,
    TableOfContents,
}

/// Table of contents entry
#[derive(Debug, Clone)]
pub struct TocEntry {
    pub title: String,
    pub page: usize,
    pub level: usize,
}

/// Sidebar state and rendering
pub struct Sidebar {
    /// Whether sidebar is visible
    pub visible: bool,
    /// Sidebar width in pixels
    pub width: u32,
    /// Current tab
    pub tab: SidebarTab,
    /// Scroll offset for thumbnails
    pub scroll_y: f64,
    /// Page thumbnails (page index -> thumbnail data)
    pub thumbnails: HashMap<usize, ThumbnailData>,
    /// Table of contents
    pub toc: Vec<TocEntry>,
    /// Currently selected/hovered page
    pub selected_page: Option<usize>,
    /// Total page count
    pub page_count: usize,
}

/// Thumbnail data for a page
#[derive(Debug, Clone)]
pub struct ThumbnailData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // RGBA
}

/// Sidebar width constant
pub const SIDEBAR_WIDTH: u32 = 200;
/// Thumbnail height
const THUMBNAIL_HEIGHT: u32 = 150;
/// Thumbnail padding
const THUMBNAIL_PADDING: u32 = 10;
/// Tab bar height
const TAB_BAR_HEIGHT: u32 = 32;

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            visible: false,
            width: SIDEBAR_WIDTH,
            tab: SidebarTab::default(),
            scroll_y: 0.0,
            thumbnails: HashMap::new(),
            toc: Vec::new(),
            selected_page: None,
            page_count: 0,
        }
    }

    /// Toggle sidebar visibility
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// Set page count and reset state
    pub fn set_page_count(&mut self, count: usize) {
        self.page_count = count;
        self.thumbnails.clear();
        self.scroll_y = 0.0;
        self.selected_page = None;
    }

    /// Set table of contents
    #[allow(dead_code)]
    pub fn set_toc(&mut self, toc: Vec<TocEntry>) {
        self.toc = toc;
    }

    /// Add a thumbnail for a page
    pub fn add_thumbnail(&mut self, page: usize, data: ThumbnailData) {
        self.thumbnails.insert(page, data);
    }

    /// Switch tabs
    pub fn switch_tab(&mut self, tab: SidebarTab) {
        self.tab = tab;
        self.scroll_y = 0.0;
    }

    /// Scroll the sidebar content
    pub fn scroll(&mut self, delta_y: f64, viewport_height: u32) {
        let content_height = match self.tab {
            SidebarTab::Thumbnails => {
                (self.page_count as u32 * (THUMBNAIL_HEIGHT + THUMBNAIL_PADDING)) as f64
            }
            SidebarTab::TableOfContents => (self.toc.len() * 24) as f64,
        };

        let available_height = viewport_height.saturating_sub(TAB_BAR_HEIGHT) as f64;
        let max_scroll = (content_height - available_height).max(0.0);

        self.scroll_y = (self.scroll_y + delta_y).clamp(0.0, max_scroll);
    }

    /// Handle click at position, returns page number if a page was clicked
    pub fn handle_click(&mut self, x: f64, y: f64) -> Option<usize> {
        if !self.visible || x > self.width as f64 {
            return None;
        }

        // Check tab bar clicks
        if y < TAB_BAR_HEIGHT as f64 {
            let tab_width = self.width as f64 / 2.0;
            if x < tab_width {
                self.switch_tab(SidebarTab::Thumbnails);
            } else {
                self.switch_tab(SidebarTab::TableOfContents);
            }
            return None;
        }

        // Handle content clicks
        let content_y = y - TAB_BAR_HEIGHT as f64 + self.scroll_y;

        match self.tab {
            SidebarTab::Thumbnails => {
                let item_height = (THUMBNAIL_HEIGHT + THUMBNAIL_PADDING) as f64;
                let page = (content_y / item_height) as usize;
                if page < self.page_count {
                    self.selected_page = Some(page);
                    return Some(page);
                }
            }
            SidebarTab::TableOfContents => {
                let item_height = 24.0;
                let index = (content_y / item_height) as usize;
                if let Some(entry) = self.toc.get(index) {
                    self.selected_page = Some(entry.page);
                    return Some(entry.page);
                }
            }
        }

        None
    }

    /// Render the sidebar
    pub fn render(&self, renderer: &Renderer, viewport_height: u32) -> Result<()> {
        if !self.visible {
            return Ok(());
        }

        let ctx = renderer.context()?;
        ctx.save()?;

        // Sidebar background
        ctx.set_source_rgb(0.12, 0.12, 0.12);
        ctx.rectangle(0.0, 0.0, self.width as f64, viewport_height as f64);
        ctx.fill()?;

        // Tab bar
        self.render_tab_bar(&ctx)?;

        // Content area - clip to prevent overflow
        ctx.rectangle(
            0.0,
            TAB_BAR_HEIGHT as f64,
            self.width as f64,
            (viewport_height - TAB_BAR_HEIGHT) as f64,
        );
        ctx.clip();

        ctx.translate(0.0, TAB_BAR_HEIGHT as f64 - self.scroll_y);

        match self.tab {
            SidebarTab::Thumbnails => self.render_thumbnails(&ctx, viewport_height)?,
            SidebarTab::TableOfContents => self.render_toc(&ctx, viewport_height)?,
        }

        ctx.restore()?;

        // Right border
        ctx.set_source_rgb(0.3, 0.3, 0.3);
        ctx.set_line_width(1.0);
        ctx.move_to(self.width as f64 - 0.5, 0.0);
        ctx.line_to(self.width as f64 - 0.5, viewport_height as f64);
        ctx.stroke()?;

        Ok(())
    }

    fn render_tab_bar(&self, ctx: &gartk_render::cairo::Context) -> Result<()> {
        let tab_width = self.width as f64 / 2.0;

        // Tab backgrounds
        for (i, tab) in [SidebarTab::Thumbnails, SidebarTab::TableOfContents]
            .iter()
            .enumerate()
        {
            let x = i as f64 * tab_width;
            let is_active = *tab == self.tab;

            if is_active {
                ctx.set_source_rgb(0.2, 0.2, 0.2);
            } else {
                ctx.set_source_rgb(0.15, 0.15, 0.15);
            }
            ctx.rectangle(x, 0.0, tab_width, TAB_BAR_HEIGHT as f64);
            ctx.fill()?;

            // Tab text
            ctx.select_font_face(
                "sans-serif",
                gartk_render::cairo::FontSlant::Normal,
                gartk_render::cairo::FontWeight::Normal,
            );
            ctx.set_font_size(12.0);

            let text = match tab {
                SidebarTab::Thumbnails => "Pages",
                SidebarTab::TableOfContents => "Contents",
            };

            if is_active {
                ctx.set_source_rgb(0.9, 0.9, 0.9);
            } else {
                ctx.set_source_rgb(0.6, 0.6, 0.6);
            }

            let extents = ctx.text_extents(text)?;
            ctx.move_to(
                x + (tab_width - extents.width()) / 2.0,
                (TAB_BAR_HEIGHT as f64 + extents.height()) / 2.0,
            );
            ctx.show_text(text)?;
        }

        // Tab separator
        ctx.set_source_rgb(0.3, 0.3, 0.3);
        ctx.set_line_width(1.0);
        ctx.move_to(tab_width, 0.0);
        ctx.line_to(tab_width, TAB_BAR_HEIGHT as f64);
        ctx.stroke()?;

        // Bottom border
        ctx.move_to(0.0, TAB_BAR_HEIGHT as f64 - 0.5);
        ctx.line_to(self.width as f64, TAB_BAR_HEIGHT as f64 - 0.5);
        ctx.stroke()?;

        Ok(())
    }

    fn render_thumbnails(
        &self,
        ctx: &gartk_render::cairo::Context,
        viewport_height: u32,
    ) -> Result<()> {
        let available_height = viewport_height.saturating_sub(TAB_BAR_HEIGHT);
        let item_height = (THUMBNAIL_HEIGHT + THUMBNAIL_PADDING) as f64;

        // Calculate visible range
        let first_visible = (self.scroll_y / item_height) as usize;
        let last_visible = ((self.scroll_y + available_height as f64) / item_height) as usize + 1;

        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Normal,
        );
        ctx.set_font_size(11.0);

        for page in first_visible..last_visible.min(self.page_count) {
            let y = page as f64 * item_height + THUMBNAIL_PADDING as f64 / 2.0;

            // Thumbnail frame
            let thumb_width = self.width - THUMBNAIL_PADDING * 2;
            let thumb_x = THUMBNAIL_PADDING as f64;

            // Selection highlight
            if self.selected_page == Some(page) {
                ctx.set_source_rgba(0.3, 0.5, 0.8, 0.3);
                ctx.rectangle(
                    thumb_x - 4.0,
                    y - 4.0,
                    thumb_width as f64 + 8.0,
                    THUMBNAIL_HEIGHT as f64 + 20.0,
                );
                ctx.fill()?;
            }

            // Thumbnail background (placeholder)
            ctx.set_source_rgb(0.95, 0.95, 0.95);
            ctx.rectangle(thumb_x, y, thumb_width as f64, THUMBNAIL_HEIGHT as f64);
            ctx.fill()?;

            // If we have thumbnail data, render it
            if let Some(thumb) = self.thumbnails.get(&page) {
                // Scale thumbnail to fit
                let scale_x = thumb_width as f64 / thumb.width as f64;
                let scale_y = THUMBNAIL_HEIGHT as f64 / thumb.height as f64;
                let scale = scale_x.min(scale_y);

                let scaled_w = thumb.width as f64 * scale;
                let scaled_h = thumb.height as f64 * scale;
                let offset_x = (thumb_width as f64 - scaled_w) / 2.0;
                let offset_y = (THUMBNAIL_HEIGHT as f64 - scaled_h) / 2.0;

                ctx.save()?;
                ctx.translate(thumb_x + offset_x, y + offset_y);
                ctx.scale(scale, scale);

                // Create surface from thumbnail data
                if let Ok(surface) = gartk_render::Surface::from_rgba(
                    &thumb.data,
                    thumb.width,
                    thumb.height,
                ) {
                    ctx.set_source_surface(surface.cairo_surface(), 0.0, 0.0)?;
                    ctx.paint()?;
                }

                ctx.restore()?;
            }

            // Border
            ctx.set_source_rgb(0.5, 0.5, 0.5);
            ctx.set_line_width(1.0);
            ctx.rectangle(thumb_x, y, thumb_width as f64, THUMBNAIL_HEIGHT as f64);
            ctx.stroke()?;

            // Page number
            let label = format!("{}", page + 1);
            ctx.set_source_rgb(0.7, 0.7, 0.7);
            let extents = ctx.text_extents(&label)?;
            ctx.move_to(
                (self.width as f64 - extents.width()) / 2.0,
                y + THUMBNAIL_HEIGHT as f64 + 14.0,
            );
            ctx.show_text(&label)?;
        }

        Ok(())
    }

    fn render_toc(
        &self,
        ctx: &gartk_render::cairo::Context,
        viewport_height: u32,
    ) -> Result<()> {
        let available_height = viewport_height.saturating_sub(TAB_BAR_HEIGHT);
        let item_height = 24.0;

        // Calculate visible range
        let first_visible = (self.scroll_y / item_height) as usize;
        let last_visible = ((self.scroll_y + available_height as f64) / item_height) as usize + 1;

        ctx.select_font_face(
            "sans-serif",
            gartk_render::cairo::FontSlant::Normal,
            gartk_render::cairo::FontWeight::Normal,
        );
        ctx.set_font_size(12.0);

        if self.toc.is_empty() {
            ctx.set_source_rgb(0.5, 0.5, 0.5);
            ctx.move_to(10.0, 30.0);
            ctx.show_text("No table of contents")?;
            return Ok(());
        }

        for (i, entry) in self.toc.iter().enumerate() {
            if i < first_visible || i > last_visible {
                continue;
            }

            let y = i as f64 * item_height;
            let indent = entry.level as f64 * 12.0;

            // Selection highlight
            if self.selected_page == Some(entry.page) {
                ctx.set_source_rgba(0.3, 0.5, 0.8, 0.3);
                ctx.rectangle(0.0, y, self.width as f64, item_height);
                ctx.fill()?;
            }

            // Entry text
            ctx.set_source_rgb(0.8, 0.8, 0.8);
            ctx.move_to(8.0 + indent, y + 16.0);

            // Truncate long titles
            let max_width = (self.width as f64 - 20.0 - indent) as usize;
            let title = if entry.title.len() > max_width / 7 {
                format!("{}...", &entry.title[..max_width / 7])
            } else {
                entry.title.clone()
            };
            ctx.show_text(&title)?;
        }

        Ok(())
    }
}
