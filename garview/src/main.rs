use anyhow::Result;
use clap::Parser;

mod app;
mod backend;
mod cache;
mod config;
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
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();

    let mut app = App::new(args.path, args.fullscreen, args.slideshow)?;
    app.run()
}
