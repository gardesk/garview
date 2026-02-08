use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::prelude::*;

mod annotate;
mod app;
mod backend;
mod cache;
mod config;
mod forms;
mod ipc;
mod recent;
mod ui;
mod viewer;

use app::App;

#[derive(Parser)]
#[command(name = "garview", about = "Image & Document Viewer")]
struct Args {
    /// File or directory to open
    path: Option<String>,

    /// Start in fullscreen mode
    #[arg(short, long)]
    fullscreen: bool,

    /// Start slideshow mode
    #[arg(short, long)]
    slideshow: bool,

    /// Log to file (e.g., --log /tmp/garview.log)
    #[arg(long, value_name = "FILE")]
    log: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Set up logging
    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("info".parse().unwrap());

    if let Some(log_path) = &args.log {
        // Log to file
        let file = std::fs::File::create(log_path)?;
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file)
            .with_ansi(false);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .init();

        eprintln!("Logging to: {}", log_path.display());
    } else {
        // Log to stderr
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .init();
    }

    let mut app = App::new(args.path, args.fullscreen, args.slideshow)?;
    app.run()
}
