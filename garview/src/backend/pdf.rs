use super::{Backend, LinkDestination, PageSize, RenderedPage};
use anyhow::{anyhow, Result};
use cairo::glib::translate::ToGlibPtr;
use poppler::{ffi, Document, IndexIter, Rectangle, SelectionStyle};
use std::ffi::CStr;
use std::path::{Path, PathBuf};

pub struct PdfBackend {
    document: Option<Document>,
    path: Option<PathBuf>,
}

impl PdfBackend {
    pub fn new() -> Self {
        Self {
            document: None,
            path: None,
        }
    }

    fn get_page(&self, index: usize) -> Result<poppler::Page> {
        let doc = self
            .document
            .as_ref()
            .ok_or_else(|| anyhow!("No document loaded"))?;
        doc.page(index as i32)
            .ok_or_else(|| anyhow!("Invalid page index: {}", index))
    }

    /// Extract TOC entries recursively from an IndexIter
    fn extract_toc_entries(
        iter: &mut IndexIter,
        level: usize,
        entries: &mut Vec<(String, usize, usize)>,
    ) {
        loop {
            // Get the action for this entry using FFI
            unsafe {
                let iter_ptr = iter.to_glib_none().0;
                let action_ptr = ffi::poppler_index_iter_get_action(iter_ptr);

                if !action_ptr.is_null() {
                    // Cast to PopplerActionAny to get type and title
                    let action_any = &*(action_ptr as *const ffi::PopplerActionAny);

                    // Get title
                    let title = if !action_any.title.is_null() {
                        CStr::from_ptr(action_any.title)
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        String::new()
                    };

                    // Get page number based on action type
                    let page = if action_any.type_ == ffi::POPPLER_ACTION_GOTO_DEST {
                        let goto_dest = &*(action_ptr as *const ffi::PopplerActionGotoDest);
                        if !goto_dest.dest.is_null() {
                            let dest = &*goto_dest.dest;
                            // poppler uses 1-based page numbers, we use 0-based
                            (dest.page_num.max(1) - 1) as usize
                        } else {
                            0
                        }
                    } else {
                        0
                    };

                    if !title.is_empty() {
                        entries.push((title, page, level));
                    }

                    // Free the action
                    ffi::poppler_action_free(action_ptr);
                }
            }

            // Process children
            if let Some(mut child) = iter.child() {
                Self::extract_toc_entries(&mut child, level + 1, entries);
            }

            // Move to next sibling
            if !iter.next() {
                break;
            }
        }
    }
}

impl Backend for PdfBackend {
    fn format_name(&self) -> &'static str {
        "PDF"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn open(&mut self, path: &Path) -> Result<()> {
        self.close();

        // poppler requires a file:// URI
        let abs_path = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf());
        let uri = format!("file://{}", abs_path.display());

        let doc = Document::from_file(&uri, None)
            .map_err(|e| anyhow!("Failed to open PDF: {}", e))?;

        self.document = Some(doc);
        self.path = Some(path.to_path_buf());
        Ok(())
    }

    fn close(&mut self) {
        self.document = None;
        self.path = None;
    }

    fn is_open(&self) -> bool {
        self.document.is_some()
    }

    fn page_count(&self) -> usize {
        self.document
            .as_ref()
            .map(|d| d.n_pages() as usize)
            .unwrap_or(0)
    }

    fn page_size(&self, page: usize) -> Result<PageSize> {
        let p = self.get_page(page)?;
        let (width, height) = p.size();
        Ok(PageSize { width, height })
    }

    fn render_page(&mut self, page: usize, scale: f64) -> Result<RenderedPage> {
        let p = self.get_page(page)?;
        let (width, height) = p.size();

        let width_px = (width * scale).ceil() as i32;
        let height_px = (height * scale).ceil() as i32;

        // Create Cairo surface for rendering
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width_px, height_px)
            .map_err(|e| anyhow!("Failed to create surface: {}", e))?;

        {
            let ctx = cairo::Context::new(&surface)
                .map_err(|e| anyhow!("Failed to create context: {}", e))?;

            // White background
            ctx.set_source_rgb(1.0, 1.0, 1.0);
            ctx.paint().map_err(|e| anyhow!("Paint failed: {}", e))?;

            // Scale and render
            ctx.scale(scale, scale);
            p.render(&ctx);
        }

        // Flush and get data
        surface
            .flush();

        let stride = surface.stride() as usize;
        let data = surface.data().map_err(|e| anyhow!("Failed to get surface data: {}", e))?;

        // Convert ARGB (Cairo) to RGBA
        let mut rgba = Vec::with_capacity((width_px * height_px * 4) as usize);
        for y in 0..height_px as usize {
            for x in 0..width_px as usize {
                let offset = y * stride + x * 4;
                // Cairo uses BGRA on little-endian systems (stored as ARGB32)
                let b = data[offset];
                let g = data[offset + 1];
                let r = data[offset + 2];
                let a = data[offset + 3];
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(a);
            }
        }

        Ok(RenderedPage {
            data: rgba,
            width: width_px as u32,
            height: height_px as u32,
            index: page,
        })
    }

    fn supports_search(&self) -> bool {
        true
    }

    fn search_page(&self, page: usize, query: &str) -> Vec<(f64, f64, f64, f64)> {
        let p = match self.get_page(page) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        p.find_text(query)
            .into_iter()
            .map(|r| (r.x1(), r.y1(), r.x2(), r.y2()))
            .collect()
    }

    fn supports_text_selection(&self) -> bool {
        true
    }

    fn get_text_for_area(&self, page: usize, area: (f64, f64, f64, f64)) -> Option<String> {
        let p = self.get_page(page).ok()?;
        let (x1, y1, x2, y2) = area;

        // Create a Rectangle for the selection area
        let mut rect = Rectangle::default();
        rect.set_x1(x1);
        rect.set_y1(y1);
        rect.set_x2(x2);
        rect.set_y2(y2);

        // Use selected_text with glyph selection style for accurate word selection
        let result = p.selected_text(SelectionStyle::Glyph, &mut rect)
            .map(|s| s.to_string());

        tracing::debug!(
            "PDF get_text_for_area: page={}, rect=({:.1},{:.1})-({:.1},{:.1}), result={:?}",
            page, x1, y1, x2, y2,
            result.as_ref().map(|s| s.chars().take(50).collect::<String>())
        );

        result
    }

    fn get_selection_region(
        &self,
        page: usize,
        area: (f64, f64, f64, f64),
        scale: f64,
    ) -> Vec<(i32, i32, i32, i32)> {
        let p = match self.get_page(page) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        let (x1, y1, x2, y2) = area;
        let mut rect = Rectangle::default();
        rect.set_x1(x1);
        rect.set_y1(y1);
        rect.set_x2(x2);
        rect.set_y2(y2);

        // Get the selection region from poppler - this returns the actual text bounds
        if let Some(region) = p.selected_region(scale, SelectionStyle::Glyph, &mut rect) {
            let num_rects = region.num_rectangles();
            let mut result = Vec::with_capacity(num_rects as usize);
            for i in 0..num_rects {
                let r = region.rectangle(i);
                result.push((r.x(), r.y(), r.width(), r.height()));
            }
            result
        } else {
            Vec::new()
        }
    }

    fn has_toc(&self) -> bool {
        if let Some(ref doc) = self.document {
            // Check if document has an index by creating an IndexIter
            let iter = IndexIter::new(doc);
            // IndexIter::new returns an empty iter if no TOC, but we can check
            // by trying to get an action from the first entry
            unsafe {
                let iter_ptr = iter.to_glib_none().0;
                let action_ptr = ffi::poppler_index_iter_get_action(iter_ptr);
                let has_entries = !action_ptr.is_null();
                if !action_ptr.is_null() {
                    ffi::poppler_action_free(action_ptr);
                }
                has_entries
            }
        } else {
            false
        }
    }

    fn get_toc(&self) -> Vec<(String, usize, usize)> {
        if let Some(ref doc) = self.document {
            let mut iter = IndexIter::new(doc);
            let mut entries = Vec::new();
            Self::extract_toc_entries(&mut iter, 0, &mut entries);
            entries
        } else {
            Vec::new()
        }
    }

    fn supports_links(&self) -> bool {
        // TODO: poppler-rs LinkMapping bindings don't fully expose the fields
        // needed to extract link destinations. This would require FFI work.
        // For now, links are detected but not extracted.
        false
    }

    fn get_links(&self, _page: usize) -> Vec<((f64, f64, f64, f64), LinkDestination)> {
        // TODO: Implement when poppler-rs provides better LinkMapping accessors
        // The link_mapping() call works, but accessing the action and area
        // fields requires unsafe FFI code through poppler-sys
        Vec::new()
    }
}
