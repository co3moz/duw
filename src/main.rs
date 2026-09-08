mod assets;
mod cli;
mod fsext;
mod scan;
mod server;
mod tree;

use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use globset::{Glob, GlobSetBuilder};

use crate::cli::Args;
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

    let root = dunce::canonicalize(&args.path)
        .map_err(|e| format!("cannot open {}: {e}", args.path.display()))?;

    if args.one_file_system && !fsext::ONE_FILE_SYSTEM_SUPPORTED {
        eprintln!("duw: warning: --one-file-system is not supported on this platform, ignoring");
    }

    if args.local_only && !fsext::CLOUD_DETECTION_SUPPORTED {
        eprintln!("duw: warning: --local-only is not supported on this platform, ignoring");
    }

    let exclude = build_excludes(&args)?;

    let opts = ScanOpts {
        root: root.clone(),
        one_file_system: args.one_file_system && fsext::ONE_FILE_SYSTEM_SUPPORTED,
        dereference: args.dereference,
        count_links: args.count_links,
        max_depth: args.max_depth,
        exclude,
        threads: args.threads,
        local_only: args.local_only && fsext::CLOUD_DETECTION_SUPPORTED,
    };

    let scanner = Scanner::new(opts).map_err(|e| format!("cannot scan {}: {e}", root.display()))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let state = AppState {
        scanner: Arc::clone(&scanner),
        root: root.display().to_string(),
        shutdown: shutdown_rx,
    };

    // The walker is CPU/IO bound and fully synchronous; keep it off the async
    // runtime so progress requests stay responsive.
    let worker = Arc::clone(&scanner);
    std::thread::Builder::new()
        .name("duw-scan".into())
        .spawn(move || worker.run())
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
