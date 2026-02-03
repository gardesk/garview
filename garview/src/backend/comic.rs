use crate::backend::{Backend, PageSize, RenderedPage};
use anyhow::{Context, Result};
use image::DynamicImage;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

/// Comic archive backend (CBZ, CB7, CBT)
/// Note: CBR (RAR) requires libunrar system dependency, not included
pub struct ComicBackend {
    /// Extracted page data (filename, image bytes)
    pages: Vec<(String, Vec<u8>)>,
    /// Cached decoded images
    decoded_cache: Vec<Option<DynamicImage>>,
    /// Whether a file is open
    is_open: bool,
}

impl ComicBackend {
    pub fn new() -> Self {
        Self {
            pages: Vec::new(),
            decoded_cache: Vec::new(),
            is_open: false,
        }
    }

    /// Check if a filename is an image based on extension
    fn is_image_file(name: &str) -> bool {
        let lower = name.to_lowercase();
        lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".png")
            || lower.ends_with(".gif")
            || lower.ends_with(".webp")
            || lower.ends_with(".bmp")
    }

    /// Natural sort key for filenames (handles "page1", "page2", "page10" correctly)
    fn natural_sort_key(s: &str) -> Vec<NaturalSortPart> {
        let mut parts = Vec::new();
        let mut current_num = String::new();
        let mut current_str = String::new();

        for c in s.chars() {
            if c.is_ascii_digit() {
                if !current_str.is_empty() {
                    parts.push(NaturalSortPart::Str(current_str.to_lowercase()));
                    current_str.clear();
                }
                current_num.push(c);
            } else {
                if !current_num.is_empty() {
                    parts.push(NaturalSortPart::Num(
                        current_num.parse().unwrap_or(0),
                    ));
                    current_num.clear();
                }
                current_str.push(c);
            }
        }

        if !current_num.is_empty() {
            parts.push(NaturalSortPart::Num(current_num.parse().unwrap_or(0)));
        }
        if !current_str.is_empty() {
            parts.push(NaturalSortPart::Str(current_str.to_lowercase()));
        }

        parts
    }

    /// Open a CBZ (ZIP) archive
    fn open_cbz(&mut self, path: &Path) -> Result<()> {
        let file = File::open(path).context("Failed to open CBZ file")?;
        let mut archive = zip::ZipArchive::new(file).context("Failed to read ZIP archive")?;

        let mut pages: Vec<(String, Vec<u8>)> = Vec::new();

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).context("Failed to read ZIP entry")?;
            let name = entry.name().to_string();

            if entry.is_file() && Self::is_image_file(&name) {
                let mut data = Vec::new();
                entry
                    .read_to_end(&mut data)
                    .context("Failed to read image from archive")?;
                pages.push((name, data));
            }
        }

        // Natural sort by filename
        pages.sort_by(|a, b| {
            Self::natural_sort_key(&a.0).cmp(&Self::natural_sort_key(&b.0))
        });

        self.pages = pages;
        self.decoded_cache = vec![None; self.pages.len()];
        self.is_open = true;

        Ok(())
    }

    /// Open a CB7 (7z) archive
    fn open_cb7(&mut self, path: &Path) -> Result<()> {
        use sevenz_rust::SevenZReader;

        let file = File::open(path).context("Failed to open CB7 file")?;
        let len = file.metadata()?.len();
        let mut archive =
            SevenZReader::new(file, len, "".into()).context("Failed to read 7z archive")?;

        let mut pages: Vec<(String, Vec<u8>)> = Vec::new();

        archive
            .for_each_entries(|entry, reader| {
                let name = entry.name().to_string();
                if !entry.is_directory() && Self::is_image_file(&name) {
                    let mut data = Vec::new();
                    if reader.read_to_end(&mut data).is_ok() {
                        pages.push((name, data));
                    }
                }
                Ok(true)
            })
            .context("Failed to iterate 7z entries")?;

        // Natural sort
        pages.sort_by(|a, b| {
            Self::natural_sort_key(&a.0).cmp(&Self::natural_sort_key(&b.0))
        });

        self.pages = pages;
        self.decoded_cache = vec![None; self.pages.len()];
        self.is_open = true;

        Ok(())
    }

    /// Open a CBT (tar) archive
    fn open_cbt(&mut self, path: &Path) -> Result<()> {
        let file = File::open(path).context("Failed to open CBT file")?;
        let mut archive = tar::Archive::new(file);

        let mut pages: Vec<(String, Vec<u8>)> = Vec::new();

        for entry in archive.entries().context("Failed to read tar entries")? {
            let mut entry = entry.context("Failed to read tar entry")?;
            let path = entry.path().context("Failed to get entry path")?;
            let name = path.to_string_lossy().to_string();

            if entry.header().entry_type().is_file() && Self::is_image_file(&name) {
                let mut data = Vec::new();
                entry
                    .read_to_end(&mut data)
                    .context("Failed to read image from tar")?;
                pages.push((name, data));
            }
        }

        // Natural sort
        pages.sort_by(|a, b| {
            Self::natural_sort_key(&a.0).cmp(&Self::natural_sort_key(&b.0))
        });

        self.pages = pages;
        self.decoded_cache = vec![None; self.pages.len()];
        self.is_open = true;

        Ok(())
    }

    /// Decode an image from bytes
    fn decode_image(&self, data: &[u8]) -> Result<DynamicImage> {
        image::load_from_memory(data).context("Failed to decode image")
    }

    /// Get or decode a page image
    fn get_page_image(&mut self, page: usize) -> Result<&DynamicImage> {
        if page >= self.pages.len() {
            anyhow::bail!("Page {} out of range", page);
        }

        if self.decoded_cache[page].is_none() {
            let img = self.decode_image(&self.pages[page].1)?;
            self.decoded_cache[page] = Some(img);
        }

        Ok(self.decoded_cache[page].as_ref().unwrap())
    }
}

/// Part of a natural sort key
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum NaturalSortPart {
    Num(u64),
    Str(String),
}

impl Backend for ComicBackend {
    fn format_name(&self) -> &'static str {
        "Comic"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cbz", "cb7", "cbt"]
    }

    fn open(&mut self, path: &Path) -> Result<()> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        match ext.as_str() {
            "cbz" => self.open_cbz(path),
            "cb7" => self.open_cb7(path),
            "cbt" => self.open_cbt(path),
            _ => anyhow::bail!("Unsupported comic format: {}", ext),
        }
    }

    fn close(&mut self) {
        self.pages.clear();
        self.decoded_cache.clear();
        self.is_open = false;
    }

    fn is_open(&self) -> bool {
        self.is_open
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_size(&self, page: usize) -> Result<PageSize> {
        // We need to decode to get dimensions
        // Clone self to work around borrow checker
        let data = self.pages.get(page).context("Page out of range")?.1.clone();
        let img = self.decode_image(&data)?;

        Ok(PageSize {
            width: img.width() as f64,
            height: img.height() as f64,
        })
    }

    fn render_page(&mut self, page: usize, scale: f64) -> Result<RenderedPage> {
        let img = self.get_page_image(page)?;

        let orig_width = img.width();
        let orig_height = img.height();

        let new_width = ((orig_width as f64) * scale).round() as u32;
        let new_height = ((orig_height as f64) * scale).round() as u32;

        // Scale if needed
        let scaled = if scale != 1.0 {
            img.resize(new_width, new_height, image::imageops::FilterType::Triangle)
        } else {
            img.clone()
        };

        // Convert to RGBA
        let rgba = scaled.to_rgba8();

        Ok(RenderedPage {
            data: rgba.into_raw(),
            width: new_width,
            height: new_height,
            index: page,
        })
    }
}
