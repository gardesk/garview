//! garview IPC protocol types.
//!
//! Shared types for communication between garview and garviewctl.

use serde::{Deserialize, Serialize};

/// Commands sent to garview daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Open a file
    Open { path: String },
    /// Close current file
    Close,
    /// Go to next image/page
    Next,
    /// Go to previous image/page
    Prev,
    /// Go to first image/page
    First,
    /// Go to last image/page
    Last,
    /// Go to specific page
    Goto { page: usize },
    /// Zoom in
    ZoomIn,
    /// Zoom out
    ZoomOut,
    /// Reset zoom to fit
    ZoomFit,
    /// Set zoom to 100%
    ZoomActual,
    /// Set specific zoom level (percentage)
    ZoomSet { level: f64 },
    /// Rotate clockwise
    RotateCw,
    /// Rotate counter-clockwise
    RotateCcw,
    /// Flip horizontally
    FlipH,
    /// Flip vertically
    FlipV,
    /// Toggle fullscreen
    Fullscreen,
    /// Toggle sidebar
    Sidebar,
    /// Start slideshow
    SlideshowStart,
    /// Stop slideshow
    SlideshowStop,
    /// Set slideshow interval in seconds
    SlideshowInterval { seconds: f64 },
    /// Get viewer info (current file, page, zoom, etc.)
    GetInfo,
    /// Ping to check if instance is alive
    Ping,
    /// Quit the application
    Quit,
}

/// Information about the current viewer state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewerInfo {
    /// PID of the garview process
    pub pid: u32,
    /// Currently open file path (if any)
    pub file: Option<String>,
    /// Current page (1-indexed)
    pub page: usize,
    /// Total page count
    pub page_count: usize,
    /// Current zoom level (percentage)
    pub zoom: f64,
    /// Image dimensions (width, height)
    pub dimensions: Option<(u32, u32)>,
    /// Whether fullscreen is active
    pub fullscreen: bool,
    /// Whether slideshow is active
    pub slideshow: bool,
}

/// Response from garview daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Response {
    pub fn ok() -> Self {
        Self {
            success: true,
            message: None,
            data: None,
        }
    }

    pub fn ok_with_message(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: Some(message.into()),
            data: None,
        }
    }

    pub fn ok_with_data(data: serde_json::Value) -> Self {
        Self {
            success: true,
            message: None,
            data: Some(data),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: Some(message.into()),
            data: None,
        }
    }
}

/// Get the socket path for garview IPC
pub fn socket_path() -> std::path::PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    std::path::PathBuf::from(runtime_dir).join("garview.sock")
}
