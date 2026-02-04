//! IPC server for garview.
//!
//! Listens on a Unix socket for commands from garviewctl.

use anyhow::{Context, Result};
use garview_ipc::{socket_path, Command, Response, ViewerInfo};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

/// IPC command received from a client
pub enum IpcCommand {
    /// Open a file
    Open(PathBuf),
    /// Navigation commands
    Next,
    Prev,
    First,
    Last,
    Goto(usize),
    /// Zoom commands
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActual,
    ZoomSet(f64),
    /// Transform commands
    RotateCw,
    RotateCcw,
    FlipH,
    FlipV,
    /// UI commands
    Fullscreen,
    Sidebar,
    /// Slideshow commands
    SlideshowStart,
    SlideshowStop,
    SlideshowInterval(f64),
    /// Query commands
    GetInfo,
    /// Control commands
    Close,
    Quit,
}

/// IPC server that runs in a background thread
pub struct IpcServer {
    /// Receiver for commands from the IPC thread
    receiver: Receiver<(IpcCommand, Sender<Response>)>,
    /// Socket path (for cleanup)
    socket_path: PathBuf,
}

impl IpcServer {
    /// Start the IPC server in a background thread
    pub fn start() -> Result<Self> {
        let socket = socket_path();

        // Remove existing socket if present
        if socket.exists() {
            std::fs::remove_file(&socket).ok();
        }

        // Create listener
        let listener = UnixListener::bind(&socket)
            .with_context(|| format!("Failed to bind socket at {}", socket.display()))?;

        // Set non-blocking for the listener
        listener.set_nonblocking(true)?;

        tracing::info!("IPC server listening on {}", socket.display());

        // Channel for sending commands to main thread
        let (tx, rx) = mpsc::channel();

        // Spawn background thread
        let socket_clone = socket.clone();
        thread::spawn(move || {
            Self::server_loop(listener, tx, socket_clone);
        });

        Ok(Self {
            receiver: rx,
            socket_path: socket,
        })
    }

    /// Server loop - runs in background thread
    fn server_loop(
        listener: UnixListener,
        tx: Sender<(IpcCommand, Sender<Response>)>,
        _socket_path: PathBuf,
    ) {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let tx = tx.clone();
                    // Handle each connection in a new thread
                    thread::spawn(move || {
                        if let Err(e) = Self::handle_client(stream, tx) {
                            tracing::error!("IPC client error: {}", e);
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No connection ready, sleep briefly
                    thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => {
                    tracing::error!("IPC accept error: {}", e);
                    break;
                }
            }
        }
    }

    /// Handle a single client connection
    fn handle_client(
        mut stream: UnixStream,
        tx: Sender<(IpcCommand, Sender<Response>)>,
    ) -> Result<()> {
        let reader = BufReader::new(stream.try_clone()?);

        for line in reader.lines() {
            let line = line?;
            if line.is_empty() {
                continue;
            }

            // Parse command
            let cmd: Command = match serde_json::from_str(&line) {
                Ok(c) => c,
                Err(e) => {
                    let resp = Response::error(format!("Invalid command: {}", e));
                    writeln!(stream, "{}", serde_json::to_string(&resp)?)?;
                    continue;
                }
            };

            // Handle ping locally
            if matches!(cmd, Command::Ping) {
                let resp = Response::ok_with_message("pong");
                writeln!(stream, "{}", serde_json::to_string(&resp)?)?;
                continue;
            }

            // Convert to internal command
            let ipc_cmd = match cmd {
                Command::Open { path } => IpcCommand::Open(PathBuf::from(path)),
                Command::Close => IpcCommand::Close,
                Command::Next => IpcCommand::Next,
                Command::Prev => IpcCommand::Prev,
                Command::First => IpcCommand::First,
                Command::Last => IpcCommand::Last,
                Command::Goto { page } => IpcCommand::Goto(page),
                Command::ZoomIn => IpcCommand::ZoomIn,
                Command::ZoomOut => IpcCommand::ZoomOut,
                Command::ZoomFit => IpcCommand::ZoomFit,
                Command::ZoomActual => IpcCommand::ZoomActual,
                Command::ZoomSet { level } => IpcCommand::ZoomSet(level),
                Command::RotateCw => IpcCommand::RotateCw,
                Command::RotateCcw => IpcCommand::RotateCcw,
                Command::FlipH => IpcCommand::FlipH,
                Command::FlipV => IpcCommand::FlipV,
                Command::Fullscreen => IpcCommand::Fullscreen,
                Command::Sidebar => IpcCommand::Sidebar,
                Command::SlideshowStart => IpcCommand::SlideshowStart,
                Command::SlideshowStop => IpcCommand::SlideshowStop,
                Command::SlideshowInterval { seconds } => IpcCommand::SlideshowInterval(seconds),
                Command::GetInfo => IpcCommand::GetInfo,
                Command::Quit => IpcCommand::Quit,
                Command::Ping => unreachable!(), // Handled above
            };

            // Create response channel
            let (resp_tx, resp_rx) = mpsc::channel();

            // Send command to main thread
            if tx.send((ipc_cmd, resp_tx)).is_err() {
                break; // Main thread closed
            }

            // Wait for response
            let response = resp_rx
                .recv()
                .unwrap_or_else(|_| Response::error("No response from viewer"));

            writeln!(stream, "{}", serde_json::to_string(&response)?)?;
        }

        Ok(())
    }

    /// Poll for incoming commands (non-blocking)
    pub fn try_recv(&self) -> Option<(IpcCommand, Sender<Response>)> {
        match self.receiver.try_recv() {
            Ok(cmd) => Some(cmd),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => None,
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        // Clean up socket
        std::fs::remove_file(&self.socket_path).ok();
    }
}

/// Create a ViewerInfo struct from current state
pub fn create_viewer_info(
    file: Option<&std::path::Path>,
    page: usize,
    page_count: usize,
    zoom: f64,
    dimensions: Option<(u32, u32)>,
    fullscreen: bool,
    slideshow: bool,
) -> ViewerInfo {
    ViewerInfo {
        pid: std::process::id(),
        file: file.map(|p| p.to_string_lossy().to_string()),
        page,
        page_count,
        zoom: zoom * 100.0, // Convert to percentage
        dimensions,
        fullscreen,
        slideshow,
    }
}
