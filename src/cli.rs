use std::path::PathBuf;

use clap::Parser;

/// Disk usage, streamed to your browser.
///
/// Scans a directory tree the way `du` does and serves a live, interactive
/// report on localhost while the scan is still running.
#[derive(Parser, Debug)]
#[command(name = "duw", version, about, long_about = None)]
pub struct Args {
    /// Directory to scan.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Port to listen on (0 picks a free one).
    #[arg(short = 'p', long, default_value_t = 0)]
    pub port: u16,

    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Do not open a browser window.
    #[arg(long)]
    pub no_open: bool,

    /// Skip directories on different filesystems (Unix only).
    #[arg(short = 'x', long)]
    pub one_file_system: bool,

    /// Follow symbolic links.
    #[arg(short = 'L', long)]
    pub dereference: bool,

    /// Count hard-linked files once per link instead of once in total.
    #[arg(short = 'l', long)]
    pub count_links: bool,

    /// Ignore files and folders whose contents live in the cloud rather than on
    /// this disk, such as OneDrive or iCloud placeholders (Windows only).
    #[arg(long)]
    pub local_only: bool,

    /// Do not descend more than N levels below the starting directory.
    #[arg(short = 'd', long, value_name = "N")]
    pub max_depth: Option<u16>,

    /// Exclude entries matching a glob pattern (repeatable).
    #[arg(long, value_name = "PATTERN")]
    pub exclude: Vec<String>,

    /// Read exclude patterns from a file, one per line.
    #[arg(short = 'X', long, value_name = "FILE")]
    pub exclude_from: Option<PathBuf>,

    /// Number of scanning threads (defaults to the number of cores).
    #[arg(short = 'j', long, value_name = "N")]
    pub threads: Option<usize>,
}
