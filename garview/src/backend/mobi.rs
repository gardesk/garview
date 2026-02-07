use crate::backend::{Backend, LinkDestination, PageSize, RenderedPage};
use anyhow::{Context, Result};
use std::path::Path;

/// MOBI/AZW ebook backend
/// Renders MOBI content as pages using text extraction
pub struct MobiBackend {
    /// Content chunks (for paging)
    pages: Vec<String>,
    /// Book metadata
    title: Option<String>,
    author: Option<String>,
    /// Whether a file is open
    is_open: bool,
    /// Rendering settings
    font_size: f64,
    margin: f64,
    /// Page dimensions for rendering
    page_width: f64,
    page_height: f64,
    /// Characters per page (approximate)
    chars_per_page: usize,
}

impl MobiBackend {
    pub fn new() -> Self {
        Self {
            pages: Vec::new(),
            title: None,
            author: None,
            is_open: false,
            font_size: 16.0,
            margin: 40.0,
            page_width: 800.0,
            page_height: 1000.0,
            chars_per_page: 3000,
        }
    }

    /// Strip HTML tags and extract plain text (MOBI content is HTML-like)
    fn strip_html(html: &str) -> String {
        let mut result = String::new();
        let mut in_tag = false;
        let mut in_script = false;
        let mut in_style = false;
        let mut last_was_space = true;

        let html_lower = html.to_lowercase();
        let mut chars = html.chars().peekable();
        let mut i = 0;

        while let Some(c) = chars.next() {
            if c == '<' {
                in_tag = true;
                let remaining = &html_lower[i..];
                if remaining.starts_with("<script") {
                    in_script = true;
                } else if remaining.starts_with("</script") {
                    in_script = false;
                } else if remaining.starts_with("<style") {
                    in_style = true;
                } else if remaining.starts_with("</style") {
                    in_style = false;
                }
                // Add newlines for block elements
                if remaining.starts_with("<p")
                    || remaining.starts_with("<br")
                    || remaining.starts_with("<div")
                    || remaining.starts_with("<h1")
                    || remaining.starts_with("<h2")
                    || remaining.starts_with("<h3")
                    || remaining.starts_with("<h4")
                    || remaining.starts_with("<li")
                    || remaining.starts_with("<mbp:pagebreak")
                {
                    if !result.ends_with('\n') && !result.is_empty() {
                        result.push('\n');
                    }
                }
            } else if c == '>' {
                in_tag = false;
            } else if !in_tag && !in_script && !in_style {
                // Decode common HTML entities
                if c == '&' {
                    let mut entity = String::new();
                    entity.push(c);
                    while let Some(&next) = chars.peek() {
                        if next == ';' || entity.len() > 10 {
                            chars.next();
                            entity.push(';');
                            break;
                        }
                        if !next.is_alphanumeric() && next != '#' {
                            break;
                        }
                        entity.push(chars.next().unwrap());
                        i += 1;
                    }
                    let decoded = match entity.as_str() {
                        "&amp;" => "&",
                        "&lt;" => "<",
                        "&gt;" => ">",
                        "&quot;" => "\"",
                        "&apos;" => "'",
                        "&nbsp;" => " ",
                        "&mdash;" => "\u{2014}",
                        "&ndash;" => "\u{2013}",
                        "&hellip;" => "\u{2026}",
                        "&rsquo;" => "\u{2019}",
                        "&lsquo;" => "\u{2018}",
                        "&rdquo;" => "\u{201D}",
                        "&ldquo;" => "\u{201C}",
                        _ => {
                            // Try numeric entities
                            if entity.starts_with("&#") {
                                let num_str = entity
                                    .trim_start_matches("&#")
                                    .trim_start_matches('x')
                                    .trim_end_matches(';');
                                if let Ok(code) = if entity.contains('x') {
                                    u32::from_str_radix(num_str, 16)
                                } else {
                                    num_str.parse()
                                } {
                                    if let Some(ch) = char::from_u32(code) {
                                        result.push(ch);
                                        i += 1;
                                        continue;
                                    }
                                }
                            }
                            " " // Unknown entity
                        }
                    };
                    result.push_str(decoded);
                    last_was_space = decoded == " ";
                } else if c.is_whitespace() {
                    if !last_was_space {
                        result.push(' ');
                        last_was_space = true;
                    }
                } else {
                    result.push(c);
                    last_was_space = false;
                }
            }
            i += c.len_utf8();
        }

        // Clean up excessive newlines
        let mut cleaned = String::new();
        let mut newline_count = 0;
        for c in result.chars() {
            if c == '\n' {
                newline_count += 1;
                if newline_count <= 2 {
                    cleaned.push(c);
                }
            } else {
                newline_count = 0;
                cleaned.push(c);
            }
        }

        cleaned.trim().to_string()
    }

    /// Split content into pages
    fn paginate(content: &str, chars_per_page: usize) -> Vec<String> {
        let mut pages = Vec::new();
        let mut current_page = String::new();

        for paragraph in content.split("\n\n") {
            let paragraph = paragraph.trim();
            if paragraph.is_empty() {
                continue;
            }

            // If adding this paragraph would exceed page limit
            if current_page.len() + paragraph.len() > chars_per_page && !current_page.is_empty() {
                pages.push(current_page.trim().to_string());
                current_page = String::new();
            }

            if !current_page.is_empty() {
                current_page.push_str("\n\n");
            }
            current_page.push_str(paragraph);
        }

        if !current_page.trim().is_empty() {
            pages.push(current_page.trim().to_string());
        }

        if pages.is_empty() {
            pages.push(content.to_string());
        }

        pages
    }

    /// Render text to RGBA pixels using Cairo/Pango
    fn render_text_to_rgba(&self, text: &str, width: u32, height: u32) -> Result<Vec<u8>> {
        use cairo::{Context, Format, ImageSurface};

        let mut surface = ImageSurface::create(Format::ARgb32, width as i32, height as i32)
            .context("Failed to create surface")?;
        let ctx = Context::new(&surface).context("Failed to create context")?;

        // Cream/sepia background for ebook feel
        ctx.set_source_rgb(0.98, 0.96, 0.93);
        ctx.paint().ok();

        // Dark brown text
        ctx.set_source_rgb(0.2, 0.15, 0.1);

        // Use Pango for text layout
        let pango_ctx = pangocairo::functions::create_context(&ctx);
        let layout = pango::Layout::new(&pango_ctx);

        let mut font_desc = pango::FontDescription::new();
        font_desc.set_family("serif");
        font_desc.set_size((self.font_size * pango::SCALE as f64) as i32);
        layout.set_font_description(Some(&font_desc));

        let text_width = width as i32 - (2.0 * self.margin) as i32;
        layout.set_width(text_width * pango::SCALE);
        layout.set_wrap(pango::WrapMode::Word);

        layout.set_text(text);

        ctx.move_to(self.margin, self.margin);
        pangocairo::functions::show_layout(&ctx, &layout);

        drop(ctx);

        // Convert ARGB to RGBA
        let stride = surface.stride() as usize;
        let data = surface.data().context("Failed to get surface data")?;
        let mut rgba = vec![0u8; (width * height * 4) as usize];

        for y in 0..height as usize {
            for x in 0..width as usize {
                let src_idx = y * stride + x * 4;
                let dst_idx = (y * width as usize + x) * 4;

                if src_idx + 3 < data.len() {
                    let b = data[src_idx];
                    let g = data[src_idx + 1];
                    let r = data[src_idx + 2];
                    let a = data[src_idx + 3];

                    rgba[dst_idx] = r;
                    rgba[dst_idx + 1] = g;
                    rgba[dst_idx + 2] = b;
                    rgba[dst_idx + 3] = a;
                }
            }
        }

        Ok(rgba)
    }
}

impl Backend for MobiBackend {
    fn format_name(&self) -> &'static str {
        "MOBI"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["mobi", "azw", "azw3", "prc"]
    }

    fn open(&mut self, path: &Path) -> Result<()> {
        let mobi = mobi::Mobi::from_path(path).context("Failed to open MOBI file")?;

        // Get metadata
        self.title = Some(mobi.title());
        self.author = mobi.author();

        // Get content
        let content = mobi.content_as_string_lossy();
        let text = Self::strip_html(&content);

        if text.is_empty() {
            anyhow::bail!("MOBI contains no readable content");
        }

        // Paginate content
        self.pages = Self::paginate(&text, self.chars_per_page);
        self.is_open = true;

        Ok(())
    }

    fn close(&mut self) {
        self.pages.clear();
        self.title = None;
        self.author = None;
        self.is_open = false;
    }

    fn is_open(&self) -> bool {
        self.is_open
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_size(&self, _page: usize) -> Result<PageSize> {
        Ok(PageSize {
            width: self.page_width,
            height: self.page_height,
        })
    }

    fn render_page(&mut self, page: usize, scale: f64) -> Result<RenderedPage> {
        let content = self.pages.get(page).context("Page out of range")?;

        let width = (self.page_width * scale).round() as u32;
        let height = (self.page_height * scale).round() as u32;

        let data = self.render_text_to_rgba(content, width, height)?;

        Ok(RenderedPage {
            data,
            width,
            height,
            index: page,
        })
    }

    fn supports_search(&self) -> bool {
        true
    }

    fn search_page(&self, page: usize, query: &str) -> Vec<(f64, f64, f64, f64)> {
        if let Some(content) = self.pages.get(page) {
            let query_lower = query.to_lowercase();
            let text_lower = content.to_lowercase();

            if text_lower.contains(&query_lower) {
                vec![(self.margin, self.margin, self.page_width - self.margin, 50.0)]
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }

    fn supports_text_selection(&self) -> bool {
        true
    }

    fn get_text_for_area(&self, page: usize, _area: (f64, f64, f64, f64)) -> Option<String> {
        self.pages.get(page).cloned()
    }

    fn has_toc(&self) -> bool {
        false // MOBI TOC parsing would require deeper format analysis
    }

    fn get_toc(&self) -> Vec<(String, usize, usize)> {
        Vec::new()
    }

    fn supports_links(&self) -> bool {
        false
    }

    fn get_links(&self, _page: usize) -> Vec<((f64, f64, f64, f64), LinkDestination)> {
        Vec::new()
    }
}
