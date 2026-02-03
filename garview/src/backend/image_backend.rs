use super::{Backend, PageSize, RenderedPage};
use anyhow::{anyhow, Result};
use image::{codecs::gif::GifDecoder, AnimationDecoder, DynamicImage};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct ImageBackend {
    image: Option<DynamicImage>,
    path: Option<PathBuf>,
    frames: Option<Vec<AnimatedFrame>>,
    exif_rotation: u32,
    /// Cached RGBA data at native resolution
    rgba_cache: Option<CachedRgba>,
}

struct CachedRgba {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

struct AnimatedFrame {
    image: DynamicImage,
    delay: Duration,
}

impl ImageBackend {
    pub fn new() -> Self {
        Self {
            image: None,
            path: None,
            frames: None,
            exif_rotation: 1,
            rgba_cache: None,
        }
    }

    fn load_animated_gif(&mut self, path: &Path) -> Result<bool> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let decoder = match GifDecoder::new(reader) {
            Ok(d) => d,
            Err(_) => return Ok(false),
        };

        let frames_result: Result<Vec<_>, _> = decoder.into_frames().collect_frames();
        let raw_frames = match frames_result {
            Ok(f) if f.len() > 1 => f,
            _ => return Ok(false),
        };

        let frames: Vec<AnimatedFrame> = raw_frames
            .into_iter()
            .map(|f| {
                let (num, denom) = f.delay().numer_denom_ms();
                let delay_ms = if denom > 0 { num / denom } else { 100 };
                let delay = Duration::from_millis(delay_ms.max(10) as u64);
                let buffer = f.into_buffer();
                AnimatedFrame {
                    image: DynamicImage::ImageRgba8(buffer),
                    delay,
                }
            })
            .collect();

        self.frames = Some(frames);
        Ok(true)
    }

    fn read_exif_rotation(&mut self, path: &Path) -> u32 {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return 1,
        };

        let mut reader = BufReader::new(file);
        let exif = match exif::Reader::new().read_from_container(&mut reader) {
            Ok(e) => e,
            Err(_) => return 1,
        };

        exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .unwrap_or(1)
    }

    fn apply_exif_rotation(image: DynamicImage, orientation: u32) -> DynamicImage {
        match orientation {
            2 => image.fliph(),
            3 => image.rotate180(),
            4 => image.flipv(),
            5 => image.rotate90().fliph(),
            6 => image.rotate90(),
            7 => image.rotate270().fliph(),
            8 => image.rotate270(),
            _ => image,
        }
    }
}

impl Backend for ImageBackend {
    fn format_name(&self) -> &'static str {
        "Image"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[
            "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "ico", "avif", "qoi", "ppm",
            "pgm", "pbm", "tga", "dds", "exr", "ff", "apng",
        ]
    }

    fn open(&mut self, path: &Path) -> Result<()> {
        self.close();

        // Try animated GIF first
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if ext == "gif" && self.load_animated_gif(path)? {
            self.path = Some(path.to_path_buf());
            return Ok(());
        }

        // Read EXIF rotation for JPEG
        if ext == "jpg" || ext == "jpeg" {
            self.exif_rotation = self.read_exif_rotation(path);
        }

        // Load static image
        let img = image::open(path)?;
        let img = Self::apply_exif_rotation(img, self.exif_rotation);

        self.image = Some(img);
        self.path = Some(path.to_path_buf());

        Ok(())
    }

    fn close(&mut self) {
        self.image = None;
        self.path = None;
        self.frames = None;
        self.exif_rotation = 1;
        self.rgba_cache = None;
    }

    fn is_open(&self) -> bool {
        self.image.is_some() || self.frames.is_some()
    }

    fn page_count(&self) -> usize {
        if let Some(ref frames) = self.frames {
            frames.len()
        } else {
            1
        }
    }

    fn page_size(&self, page: usize) -> Result<PageSize> {
        if let Some(ref frames) = self.frames {
            let frame = frames.get(page).ok_or_else(|| anyhow!("Invalid frame"))?;
            Ok(PageSize {
                width: frame.image.width() as f64,
                height: frame.image.height() as f64,
            })
        } else if let Some(ref img) = self.image {
            Ok(PageSize {
                width: img.width() as f64,
                height: img.height() as f64,
            })
        } else {
            Err(anyhow!("No image loaded"))
        }
    }

    fn render_page(&mut self, page: usize, scale: f64) -> Result<RenderedPage> {
        // For animated images, don't use the cache (frames are separate)
        if let Some(ref frames) = self.frames {
            let frame = frames.get(page).ok_or_else(|| anyhow!("Invalid frame"))?;
            let img = &frame.image;

            if scale > 1.001 {
                let new_width = (img.width() as f64 * scale).round() as u32;
                let new_height = (img.height() as f64 * scale).round() as u32;
                let resized =
                    img.resize_exact(new_width, new_height, image::imageops::FilterType::Triangle);
                let rgba = resized.to_rgba8();
                return Ok(RenderedPage {
                    data: rgba.into_raw(),
                    width: new_width,
                    height: new_height,
                    index: page,
                });
            } else {
                let rgba = img.to_rgba8();
                return Ok(RenderedPage {
                    data: rgba.into_raw(),
                    width: img.width(),
                    height: img.height(),
                    index: page,
                });
            }
        }

        let img = self.image.as_ref().ok_or_else(|| anyhow!("No image loaded"))?;
        let orig_width = img.width();
        let orig_height = img.height();

        // For scale > 1.0, resize from cached RGBA (or original if no cache)
        if scale > 1.001 {
            let new_width = (orig_width as f64 * scale).round() as u32;
            let new_height = (orig_height as f64 * scale).round() as u32;

            let resized =
                img.resize_exact(new_width, new_height, image::imageops::FilterType::Triangle);

            let rgba = resized.to_rgba8();
            Ok(RenderedPage {
                data: rgba.into_raw(),
                width: new_width,
                height: new_height,
                index: 0,
            })
        } else {
            // For scale <= 1.0, use cached RGBA data
            // This avoids expensive to_rgba8() conversion on every call
            if self.rgba_cache.is_none() {
                let rgba = img.to_rgba8();
                self.rgba_cache = Some(CachedRgba {
                    data: rgba.into_raw(),
                    width: orig_width,
                    height: orig_height,
                });
            }

            let cache = self.rgba_cache.as_ref().unwrap();
            Ok(RenderedPage {
                data: cache.data.clone(),
                width: cache.width,
                height: cache.height,
                index: 0,
            })
        }
    }

    fn is_animated(&self) -> bool {
        self.frames.is_some()
    }

    fn frame_delay(&self, frame: usize) -> Option<Duration> {
        self.frames
            .as_ref()
            .and_then(|f| f.get(frame).map(|fr| fr.delay))
    }
}
