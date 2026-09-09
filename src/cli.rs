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

    /// Look for duplicate files once the scan finishes.
    #[arg(long)]
    pub duplicates: bool,

    /// Smallest file the duplicate scanner considers, e.g. 512K, 10M, 1G.
    #[arg(long, value_name = "SIZE", default_value = "512K", value_parser = parse_size)]
    pub duplicates_min: u64,
}

/// Accepts a plain byte count or a K/M/G suffix, the way `du -t` does.
fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (digits, scale) = match text.chars().last() {
        Some('k' | 'K') => (&text[..text.len() - 1], 1024),
        Some('m' | 'M') => (&text[..text.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&text[..text.len() - 1], 1024 * 1024 * 1024),
        _ => (text, 1),
    };
    digits
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("{text:?} is not a size"))?
        .checked_mul(scale)
        .ok_or_else(|| format!("{text:?} is too large"))
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn sizes_accept_suffixes() {
        assert_eq!(parse_size("512K"), Ok(524288));
        assert_eq!(parse_size("10m"), Ok(10 * 1024 * 1024));
        assert_eq!(parse_size("4096"), Ok(4096));
        assert!(parse_size("nope").is_err());
        assert!(parse_size("99999999999999999999G").is_err());
    }
}
