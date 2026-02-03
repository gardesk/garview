//! Recent files tracking and session persistence
//!
//! Stores recently opened files with their viewing state (page, zoom, position)
//! in ~/.local/share/garview/recent.json

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Maximum number of recent files to track
const DEFAULT_MAX_ENTRIES: usize = 50;

/// A recently opened file with session state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentFile {
    /// Absolute path to the file
    pub path: PathBuf,
    /// Last time this file was opened
    #[serde(with = "system_time_serde")]
    pub last_opened: SystemTime,
    /// Last viewed page (0-indexed)
    pub page: Option<usize>,
    /// Last zoom level (percentage, e.g., 100.0 = 100%)
    pub zoom: Option<f64>,
    /// Last scroll position (x, y) normalized to document size
    pub scroll_position: Option<(f64, f64)>,
}

impl RecentFile {
    /// Create a new recent file entry
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            last_opened: SystemTime::now(),
            page: None,
            zoom: None,
            scroll_position: None,
        }
    }

    /// Update the session state
    pub fn update_session(&mut self, page: usize, zoom: f64, scroll: (f64, f64)) {
        self.last_opened = SystemTime::now();
        self.page = Some(page);
        self.zoom = Some(zoom);
        self.scroll_position = Some(scroll);
    }

    /// Get filename for display
    pub fn filename(&self) -> &str {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Unknown")
    }

    /// Check if the file still exists
    pub fn exists(&self) -> bool {
        self.path.exists()
    }
}

/// Recent files manager
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RecentFiles {
    /// List of recent files (most recent first)
    files: VecDeque<RecentFile>,
    /// Maximum number of entries to keep
    #[serde(default = "default_max_entries")]
    max_entries: usize,
}

fn default_max_entries() -> usize {
    DEFAULT_MAX_ENTRIES
}

impl RecentFiles {
    /// Create a new empty recent files list
    pub fn new() -> Self {
        Self {
            files: VecDeque::new(),
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }

    /// Load recent files from disk
    pub fn load() -> Result<Self> {
        let path = Self::storage_path();
        if path.exists() {
            let contents = fs::read_to_string(&path)?;
            let mut recent: RecentFiles = serde_json::from_str(&contents)?;
            // Remove entries for files that no longer exist
            recent.files.retain(|f| f.exists());
            Ok(recent)
        } else {
            Ok(Self::new())
        }
    }

    /// Save recent files to disk
    pub fn save(&self) -> Result<()> {
        let path = Self::storage_path();
        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(&path, contents)?;
        Ok(())
    }

    /// Get the storage path for recent files
    fn storage_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join("garview/recent.json")
    }

    /// Add or update a file in the recent list
    pub fn add(&mut self, path: &Path) {
        let abs_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        // Remove existing entry for this path
        self.files.retain(|f| f.path != abs_path);

        // Add new entry at the front
        self.files.push_front(RecentFile::new(abs_path));

        // Trim to max entries
        while self.files.len() > self.max_entries {
            self.files.pop_back();
        }
    }

    /// Update session state for a file
    pub fn update_session(&mut self, path: &Path, page: usize, zoom: f64, scroll: (f64, f64)) {
        let abs_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        if let Some(entry) = self.files.iter_mut().find(|f| f.path == abs_path) {
            entry.update_session(page, zoom, scroll);
        }
    }

    /// Get session state for a file
    pub fn get_session(&self, path: &Path) -> Option<(usize, f64, (f64, f64))> {
        let abs_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        self.files.iter().find(|f| f.path == abs_path).and_then(|f| {
            match (f.page, f.zoom, f.scroll_position) {
                (Some(page), Some(zoom), Some(scroll)) => Some((page, zoom, scroll)),
                _ => None,
            }
        })
    }

    /// Remove a file from the recent list
    pub fn remove(&mut self, path: &Path) {
        let abs_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.files.retain(|f| f.path != abs_path);
    }

    /// Remove file by index
    pub fn remove_index(&mut self, index: usize) {
        if index < self.files.len() {
            self.files.remove(index);
        }
    }

    /// Get all recent files
    pub fn files(&self) -> &VecDeque<RecentFile> {
        &self.files
    }

    /// Get number of recent files
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Clear all recent files
    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// Set maximum entries
    pub fn set_max_entries(&mut self, max: usize) {
        self.max_entries = max;
        while self.files.len() > self.max_entries {
            self.files.pop_back();
        }
    }
}

/// Serde helper for SystemTime
mod system_time_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let duration = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
        duration.as_secs().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_add_and_retrieve() {
        let mut recent = RecentFiles::new();
        let temp = NamedTempFile::new().unwrap();
        let path = temp.path();

        recent.add(path);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent.files()[0].path, path.canonicalize().unwrap());
    }

    #[test]
    fn test_max_entries() {
        let mut recent = RecentFiles::new();
        recent.set_max_entries(3);

        for i in 0..5 {
            let mut temp = NamedTempFile::new().unwrap();
            writeln!(temp, "{}", i).unwrap();
            recent.add(temp.path());
        }

        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn test_session_persistence() {
        let mut recent = RecentFiles::new();
        let temp = NamedTempFile::new().unwrap();
        let path = temp.path();

        recent.add(path);
        recent.update_session(path, 5, 150.0, (0.5, 0.3));

        let session = recent.get_session(path);
        assert!(session.is_some());
        let (page, zoom, scroll) = session.unwrap();
        assert_eq!(page, 5);
        assert_eq!(zoom, 150.0);
        assert_eq!(scroll, (0.5, 0.3));
    }
}
