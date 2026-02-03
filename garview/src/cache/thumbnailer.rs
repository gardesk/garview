use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

/// Default thumbnail size
pub const THUMBNAIL_SIZE: u32 = 128;

/// Result of thumbnail generation
pub struct ThumbnailResult {
    pub path: PathBuf,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Request for thumbnail generation
struct ThumbnailRequest {
    path: PathBuf,
    size: u32,
}

/// Thumbnail cache with async generation
pub struct ThumbnailCache {
    /// Cache directory
    cache_dir: PathBuf,
    /// In-memory cache of loaded thumbnails
    memory_cache: HashMap<PathBuf, ThumbnailResult>,
    /// Pending requests
    pending: Arc<Mutex<Vec<PathBuf>>>,
    /// Channel to receive completed thumbnails
    receiver: Receiver<Result<ThumbnailResult>>,
    /// Channel to send requests
    sender: Sender<ThumbnailRequest>,
    /// Thumbnail size
    size: u32,
}

impl ThumbnailCache {
    /// Create a new thumbnail cache
    pub fn new(size: u32) -> Result<Self> {
        let cache_dir = Self::cache_directory()?;
        fs::create_dir_all(&cache_dir)?;

        let (request_tx, request_rx) = mpsc::channel::<ThumbnailRequest>();
        let (result_tx, result_rx) = mpsc::channel::<Result<ThumbnailResult>>();

        // Spawn worker thread for thumbnail generation
        let cache_dir_clone = cache_dir.clone();
        thread::spawn(move || {
            Self::worker_thread(request_rx, result_tx, cache_dir_clone);
        });

        Ok(Self {
            cache_dir,
            memory_cache: HashMap::new(),
            pending: Arc::new(Mutex::new(Vec::new())),
            receiver: result_rx,
            sender: request_tx,
            size,
        })
    }

    /// Get the cache directory path
    fn cache_directory() -> Result<PathBuf> {
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| anyhow!("Could not find cache directory"))?
            .join("garview/thumbnails");
        Ok(cache_dir)
    }

    /// Generate a cache key for a file path
    fn cache_key(path: &Path, size: u32) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        size.hash(&mut hasher);

        // Include mtime if available for cache invalidation
        if let Ok(metadata) = fs::metadata(path) {
            if let Ok(mtime) = metadata.modified() {
                mtime.hash(&mut hasher);
            }
        }

        format!("{:016x}.png", hasher.finish())
    }

    /// Get cached thumbnail path
    fn cached_path(&self, path: &Path) -> PathBuf {
        let key = Self::cache_key(path, self.size);
        self.cache_dir.join(key)
    }

    /// Check if a thumbnail is cached on disk
    pub fn is_cached(&self, path: &Path) -> bool {
        self.cached_path(path).exists()
    }

    /// Get a thumbnail from memory cache
    pub fn get(&self, path: &Path) -> Option<&ThumbnailResult> {
        self.memory_cache.get(path)
    }

    /// Check if a thumbnail is currently being generated
    pub fn is_pending(&self, path: &Path) -> bool {
        self.pending.lock().unwrap().contains(&path.to_path_buf())
    }

    /// Request thumbnail generation (async)
    pub fn request(&self, path: &Path) {
        let path_buf = path.to_path_buf();

        // Don't request if already in memory, pending, or doesn't exist
        if self.memory_cache.contains_key(&path_buf) {
            return;
        }
        if self.is_pending(&path_buf) {
            return;
        }
        if !path.exists() {
            return;
        }

        // Add to pending
        self.pending.lock().unwrap().push(path_buf.clone());

        // Send request to worker
        let _ = self.sender.send(ThumbnailRequest {
            path: path_buf,
            size: self.size,
        });
    }

    /// Poll for completed thumbnails, returns paths that completed
    pub fn poll(&mut self) -> Vec<PathBuf> {
        let mut completed = Vec::new();

        while let Ok(result) = self.receiver.try_recv() {
            match result {
                Ok(thumb) => {
                    let path = thumb.path.clone();
                    // Remove from pending
                    self.pending.lock().unwrap().retain(|p| p != &path);
                    // Add to memory cache
                    self.memory_cache.insert(path.clone(), thumb);
                    completed.push(path);
                }
                Err(e) => {
                    tracing::warn!("Thumbnail generation failed: {}", e);
                }
            }
        }

        completed
    }

    /// Load a thumbnail from disk cache into memory
    pub fn load_cached(&mut self, path: &Path) -> Result<()> {
        let cached_path = self.cached_path(path);
        if !cached_path.exists() {
            return Err(anyhow!("Not cached"));
        }

        let img = image::open(&cached_path)?;
        let rgba = img.to_rgba8();

        let result = ThumbnailResult {
            path: path.to_path_buf(),
            data: rgba.to_vec(),
            width: rgba.width(),
            height: rgba.height(),
        };

        self.memory_cache.insert(path.to_path_buf(), result);
        Ok(())
    }

    /// Worker thread that processes thumbnail requests
    fn worker_thread(
        requests: Receiver<ThumbnailRequest>,
        results: Sender<Result<ThumbnailResult>>,
        cache_dir: PathBuf,
    ) {
        while let Ok(request) = requests.recv() {
            let result = Self::generate_thumbnail(&request.path, request.size, &cache_dir);
            if results.send(result).is_err() {
                break; // Channel closed
            }
        }
    }

    /// Generate a thumbnail for a file
    fn generate_thumbnail(path: &Path, size: u32, cache_dir: &Path) -> Result<ThumbnailResult> {
        // Check disk cache first
        let cache_key = Self::cache_key(path, size);
        let cached_path = cache_dir.join(&cache_key);

        if cached_path.exists() {
            let img = image::open(&cached_path)?;
            let rgba = img.to_rgba8();
            return Ok(ThumbnailResult {
                path: path.to_path_buf(),
                data: rgba.to_vec(),
                width: rgba.width(),
                height: rgba.height(),
            });
        }

        // Generate thumbnail
        let img = image::open(path)?;

        // Resize maintaining aspect ratio
        let thumb = img.thumbnail(size, size);
        let rgba = thumb.to_rgba8();

        // Save to disk cache
        rgba.save(&cached_path)?;

        Ok(ThumbnailResult {
            path: path.to_path_buf(),
            data: rgba.to_vec(),
            width: rgba.width(),
            height: rgba.height(),
        })
    }

    /// Clear memory cache (keeps disk cache)
    #[allow(dead_code)]
    pub fn clear_memory(&mut self) {
        self.memory_cache.clear();
    }

    /// Get memory cache size
    #[allow(dead_code)]
    pub fn memory_cache_size(&self) -> usize {
        self.memory_cache.len()
    }
}
