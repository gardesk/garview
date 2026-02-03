use super::{Backend, PageSize, RenderedPage};
use anyhow::{anyhow, Result};
use poppler::Document;
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
}
