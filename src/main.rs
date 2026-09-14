mod actions;
mod assets;
mod cli;
mod demo;
mod dupes;
mod fsext;
mod scan;
mod server;
mod snapshots;
mod tree;

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use globset::{Glob, GlobSetBuilder};

use crate::cli::Args;
use crate::dupes::Dupes;
use crate::scan::{ScanOpts, Scanner};
use crate::server::AppState;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("duw: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse();
    let demo = args.demo;

    // Demo mode never touches the filesystem; its root only names the tree.
    let root = if demo {
        if args.path == Path::new(".") {
            PathBuf::from("atlas-archive")
        } else {
            args.path.clone()
        }
    } else {
        dunce::canonicalize(&args.path)
            .map_err(|e| format!("cannot open {}: {e}", args.path.display()))?
    };

    if args.one_file_system && !fsext::ONE_FILE_SYSTEM_SUPPORTED {
        eprintln!("duw: warning: --one-file-system is not supported on this platform, ignoring");
    }

    let exclude = build_excludes(&args)?;

    // Cloud placeholders are hidden unless the user explicitly asks for them.
    let local_only = !args.include_cloud && fsext::CLOUD_DETECTION_SUPPORTED;

    let opts = ScanOpts {
        root: root.clone(),
        one_file_system: args.one_file_system && fsext::ONE_FILE_SYSTEM_SUPPORTED,
        dereference: args.dereference,
        count_links: args.count_links,
        max_depth: args.max_depth,
        exclude,
        threads: args.threads,
        local_only,
    };

    let scanner = if demo {
        Scanner::new_demo(opts)
    } else {
        Scanner::new(opts).map_err(|e| format!("cannot scan {}: {e}", root.display()))?
    };

    // Script and CI modes: scan synchronously, print, and never bind a port.
    if args.json || args.top.is_some() {
        if demo {
            scanner.run_demo();
        } else {
            scanner.run();
        }
        return print_report(&scanner, &root, &args);
    }

    if demo {
        println!("duw: demo mode, serving a synthetic tree");
    }

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let dupes = Dupes::new(Arc::clone(&scanner.tree), root.clone(), demo);

    let state = AppState {
        scanner: Arc::clone(&scanner),
        dupes: Arc::clone(&dupes),
        root: root.display().to_string(),
        local_only,
        dupes_min: args.duplicates_min,
        shutdown: shutdown_rx,
    };

    // The walker is CPU/IO bound and fully synchronous; keep it off the async
    // runtime so progress requests stay responsive.
    let worker = Arc::clone(&scanner);
    let auto_dupes = args.duplicates.then_some(args.duplicates_min);
    std::thread::Builder::new()
        .name("duw-scan".into())
        .spawn(move || {
            if demo {
                worker.run_demo();
            } else {
                worker.run();
            }
            // Duplicate detection needs the whole tree, so it waits for the
            // walk rather than racing it.
            if let Some(min) = auto_dupes {
                if !worker.is_cancelled() {
                    dupes.start(tree::ROOT, min);
                }
            }
        })
        .map_err(|e| format!("cannot start scanner: {e}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start runtime: {e}"))?;

    runtime.block_on(async move {
        let ip: IpAddr = args
            .host
            .parse()
            .map_err(|_| format!("invalid host address: {}", args.host))?;
        let addr = SocketAddr::new(ip, args.port);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("cannot bind {addr}: {e}"))?;
        let local = listener
            .local_addr()
            .map_err(|e| format!("cannot read local address: {e}"))?;
        let url = format!("http://{}", display_addr(local));

        println!("duw: scanning {}", root.display());
        println!("duw: {url}");
        println!("duw: press Ctrl+C to stop");

        if !args.no_open {
            if let Err(e) = opener::open_browser(&url) {
                eprintln!("duw: could not open a browser ({e}), open {url} yourself");
            }
        }

        axum::serve(listener, server::router(state))
            .with_graceful_shutdown(shutdown(shutdown_tx))
            .await
            .map_err(|e| format!("server error: {e}"))?;

        scanner.cancel();
        Ok::<(), String>(())
    })
}

/// `0.0.0.0` is not a useful thing to click on; point the user at localhost.
fn display_addr(addr: SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        format!("localhost:{}", addr.port())
    } else {
        addr.to_string()
    }
}

fn build_excludes(args: &Args) -> Result<Option<globset::GlobSet>, String> {
    let mut patterns: Vec<String> = args.exclude.clone();
    if let Some(file) = &args.exclude_from {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        patterns.extend(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string),
        );
    }
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for p in &patterns {
        let glob = Glob::new(p).map_err(|e| format!("bad exclude pattern {p:?}: {e}"))?;
        builder.add(glob);
    }
    builder
        .build()
        .map(Some)
        .map_err(|e| format!("cannot build exclude set: {e}"))
}

/// Grace period before a stuck connection stops being the process's problem.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

async fn shutdown(streams: tokio::sync::watch::Sender<bool>) {
    let _ = tokio::signal::ctrl_c().await;
    println!("\nduw: shutting down");

    // Tell the SSE handlers to end their responses, otherwise the graceful
    // shutdown below waits for browser tabs that will never disconnect.
    let _ = streams.send(true);

    tokio::spawn(async {
        tokio::time::sleep(SHUTDOWN_GRACE).await;
        eprintln!("duw: a connection would not close, exiting anyway");
        std::process::exit(0);
    });
}

#[derive(serde::Serialize)]
struct JsonReport {
    root: String,
    elapsed_ms: u64,
    stats: tree::Stats,
    errors: Vec<tree::ScanError>,
    largest: Vec<tree::LargeFile>,
    types: Vec<tree::ExtStat>,
}

/// Prints the finished scan for scripts. `--json` emits the whole report,
/// otherwise one `size<TAB>path` line per entry.
fn print_report(scanner: &Scanner, root: &std::path::Path, args: &Args) -> Result<(), String> {
    let limit = args.top.unwrap_or(100).clamp(1, 1_000_000);
    let t = scanner.tree.read().unwrap();
    let largest = t.largest_files(tree::ROOT, false, limit);

    if args.json {
        let report = JsonReport {
            root: root.display().to_string(),
            elapsed_ms: scanner.elapsed_ms(),
            stats: t.stats.clone(),
            errors: t.errors.clone(),
            largest,
            types: t.by_extension(tree::ROOT),
        };
        let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        println!("{text}");
    } else {
        for f in largest {
            println!("{}\t{}", f.size, f.path);
        }
    }
    Ok(())
}
