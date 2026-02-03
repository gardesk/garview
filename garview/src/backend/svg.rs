use super::{Backend, PageSize, RenderedPage};
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

pub struct SvgBackend {
    tree: Option<resvg::usvg::Tree>,
    path: Option<PathBuf>,
}

impl SvgBackend {
    pub fn new() -> Self {
        Self {
            tree: None,
            path: None,
        }
    }
}

impl Backend for SvgBackend {
    fn format_name(&self) -> &'static str {
        "SVG"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["svg", "svgz"]
    }

    fn open(&mut self, path: &Path) -> Result<()> {
        let data = std::fs::read(path)?;
        let opts = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_data(&data, &opts)?;

        self.tree = Some(tree);
        self.path = Some(path.to_path_buf());

        Ok(())
    }

    fn close(&mut self) {
        self.tree = None;
        self.path = None;
    }

    fn is_open(&self) -> bool {
        self.tree.is_some()
    }

    fn page_count(&self) -> usize {
        1
    }

    fn page_size(&self, _page: usize) -> Result<PageSize> {
        let tree = self.tree.as_ref().ok_or_else(|| anyhow!("No SVG loaded"))?;
        let size = tree.size();
        Ok(PageSize {
            width: size.width() as f64,
            height: size.height() as f64,
        })
    }

    fn render_page(&mut self, _page: usize, scale: f64) -> Result<RenderedPage> {
        let tree = self.tree.as_ref().ok_or_else(|| anyhow!("No SVG loaded"))?;

        let size = tree.size();
        let width = (size.width() as f64 * scale).round() as u32;
        let height = (size.height() as f64 * scale).round() as u32;

        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| anyhow!("Failed to create pixmap"))?;

        let transform =
            resvg::tiny_skia::Transform::from_scale(scale as f32, scale as f32);
        resvg::render(tree, transform, &mut pixmap.as_mut());

        // tiny_skia uses premultiplied alpha, convert to straight alpha
        let data = pixmap.take();

        Ok(RenderedPage {
            data,
            width,
            height,
            index: 0,
        })
    }
}
