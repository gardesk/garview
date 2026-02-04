//! garviewctl - Control garview instances via IPC
//!
//! Send commands to running garview instances.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use garview_ipc::{socket_path, Command, Response};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

#[derive(Parser)]
#[command(name = "garviewctl", about = "Control garview image viewer")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Open a file
    Open {
        /// File path to open
        path: String,
    },
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
    Goto {
        /// Page number (1-indexed)
        page: usize,
    },
    /// Zoom in
    ZoomIn,
    /// Zoom out
    ZoomOut,
    /// Fit image to window
    ZoomFit,
    /// Set zoom to 100%
    ZoomActual,
    /// Set specific zoom level
    ZoomSet {
        /// Zoom level (percentage, e.g., 150 for 150%)
        level: f64,
    },
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
    /// Slideshow control
    Slideshow {
        #[command(subcommand)]
        action: SlideshowAction,
    },
    /// Get viewer info
    Info,
    /// Check if garview is running
    Ping,
    /// Quit garview
    Quit,
}

#[derive(Subcommand)]
enum SlideshowAction {
    /// Start slideshow
    Start,
    /// Stop slideshow
    Stop,
    /// Set slideshow interval
    Interval {
        /// Interval in seconds
        seconds: f64,
    },
}

fn send_command(cmd: Command) -> Result<Response> {
    let socket = socket_path();

    let mut stream = UnixStream::connect(&socket)
        .with_context(|| format!("Failed to connect to garview at {}", socket.display()))?;

    // Send command as JSON line
    let json = serde_json::to_string(&cmd)?;
    writeln!(stream, "{}", json)?;
    stream.flush()?;

    // Read response
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let response: Response = serde_json::from_str(&line)
        .context("Failed to parse response from garview")?;

    Ok(response)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let cmd = match cli.command {
        Commands::Open { path } => {
            // Convert to absolute path
            let abs_path = std::fs::canonicalize(&path)
                .unwrap_or_else(|_| std::path::PathBuf::from(&path));
            Command::Open {
                path: abs_path.to_string_lossy().to_string(),
            }
        }
        Commands::Close => Command::Close,
        Commands::Next => Command::Next,
        Commands::Prev => Command::Prev,
        Commands::First => Command::First,
        Commands::Last => Command::Last,
        Commands::Goto { page } => Command::Goto { page },
        Commands::ZoomIn => Command::ZoomIn,
        Commands::ZoomOut => Command::ZoomOut,
        Commands::ZoomFit => Command::ZoomFit,
        Commands::ZoomActual => Command::ZoomActual,
        Commands::ZoomSet { level } => Command::ZoomSet { level },
        Commands::RotateCw => Command::RotateCw,
        Commands::RotateCcw => Command::RotateCcw,
        Commands::FlipH => Command::FlipH,
        Commands::FlipV => Command::FlipV,
        Commands::Fullscreen => Command::Fullscreen,
        Commands::Sidebar => Command::Sidebar,
        Commands::Slideshow { action } => match action {
            SlideshowAction::Start => Command::SlideshowStart,
            SlideshowAction::Stop => Command::SlideshowStop,
            SlideshowAction::Interval { seconds } => Command::SlideshowInterval { seconds },
        },
        Commands::Info => Command::GetInfo,
        Commands::Ping => Command::Ping,
        Commands::Quit => Command::Quit,
    };

    let response = send_command(cmd)?;

    if response.success {
        if let Some(msg) = response.message {
            println!("{}", msg);
        }
        if let Some(data) = response.data {
            println!("{}", serde_json::to_string_pretty(&data)?);
        }
    } else {
        eprintln!("Error: {}", response.message.unwrap_or_else(|| "Unknown error".to_string()));
        std::process::exit(1);
    }

    Ok(())
}
