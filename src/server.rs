//! HTTP API. Everything the UI needs is derived from the live tree, so the same
//! endpoints work during and after a scan.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::{Stream, StreamExt};

use crate::assets;
use crate::fsext;
use crate::scan::Scanner;
use crate::tree::{Crumb, Entry, ExtStat, LargeFile, Rollup, Stats, SubtreeNode};

const DEFAULT_LIMIT: usize = 400;
const MAX_LIMIT: usize = 5000;
/// Upper bound on the rectangles a single treemap request may produce.
const TREEMAP_BUDGET: usize = 4000;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone)]
pub struct AppState {
    pub scanner: Arc<Scanner>,
    pub root: String,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/state", get(state_handler))
        .route("/api/events", get(events))
        .route("/api/node/{id}", get(node))
        .route("/api/tree/{id}", get(subtree))
        .route("/api/types/{id}", get(types))
        .route("/api/largest/{id}", get(largest))
        .route("/api/errors", get(errors))
        .route("/api/cancel", post(cancel))
        .fallback(assets::serve)
        .with_state(state)
}

#[derive(Serialize)]
struct Progress {
    version: u64,
    scanning: bool,
    cancelled: bool,
    elapsed_ms: u64,
    current: String,
    stats: Stats,
    root_size: u64,
    root_alloc: u64,
}

#[derive(Serialize)]
struct FullState {
    root: String,
    root_id: u32,
    platform: Platform,
    #[serde(flatten)]
    progress: Progress,
}

#[derive(Serialize)]
struct Platform {
    os: &'static str,
    one_file_system: bool,
    hardlink_dedup: bool,
    /// True when on-disk sizes are estimated from the cluster size rather than
    /// reported by the filesystem.
    approximate_alloc: bool,
}

fn progress_of(state: &AppState) -> Progress {
    let t = state.scanner.tree.read().unwrap();
    let root = &t.nodes[0];
    Progress {
        version: t.version,
        scanning: !state.scanner.is_done(),
        cancelled: state.scanner.is_cancelled(),
        elapsed_ms: state.scanner.elapsed_ms(),
        current: t.current.clone(),
        stats: t.stats.clone(),
        root_size: root.total_size,
        root_alloc: root.total_alloc,
    }
}

async fn state_handler(State(state): State<AppState>) -> Json<FullState> {
    Json(FullState {
        root: state.root.clone(),
        root_id: 0,
        platform: Platform {
            os: std::env::consts::OS,
            one_file_system: fsext::ONE_FILE_SYSTEM_SUPPORTED,
            hardlink_dedup: fsext::HARDLINK_DEDUP_SUPPORTED,
            approximate_alloc: cfg!(windows),
        },
        progress: progress_of(&state),
    })
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = IntervalStream::new(tokio::time::interval(PROGRESS_INTERVAL)).map(move |_| {
        let p = progress_of(&state);
        let name = if p.scanning { "progress" } else { "done" };
        Ok(Event::default()
            .event(name)
            .json_data(p)
            .unwrap_or_else(|_| Event::default().comment("serialization failed")))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct ViewQuery {
    #[serde(default)]
    metric: Metric,
    limit: Option<usize>,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Metric {
    #[default]
    Size,
    Alloc,
}

impl Metric {
    fn by_alloc(self) -> bool {
        self == Metric::Alloc
    }
}

fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

#[derive(Serialize)]
struct NodeResponse {
    id: u32,
    name: String,
    path: String,
    breadcrumb: Vec<Crumb>,
    size: u64,
    alloc: u64,
    files: u32,
    dirs: u32,
    mtime: i64,
    read: bool,
    children: Vec<Entry>,
    other: Rollup,
    version: u64,
    scanning: bool,
}

async fn node(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<ViewQuery>,
) -> impl IntoResponse {
    let t = state.scanner.tree.read().unwrap();
    let Some(n) = t.get(id) else {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    };
    let view = t.children_view(id, q.metric.by_alloc(), clamp_limit(q.limit));
    let Some(view) = view else {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    };
    Json(NodeResponse {
        id,
        name: n.name.to_string(),
        path: t.rel_path(id),
        breadcrumb: t.breadcrumb(id),
        size: n.total_size,
        alloc: n.total_alloc,
        files: n.files,
        dirs: n.dirs,
        mtime: n.mtime,
        read: n.read,
        children: view.children,
        other: view.other,
        version: t.version,
        scanning: !state.scanner.is_done(),
    })
    .into_response()
}

#[derive(Deserialize)]
struct TreeQuery {
    #[serde(default)]
    metric: Metric,
    depth: Option<u16>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct TreeResponse {
    root: SubtreeNode,
    version: u64,
    truncated: bool,
}

async fn subtree(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<TreeQuery>,
) -> impl IntoResponse {
    let t = state.scanner.tree.read().unwrap();
    let depth = q.depth.unwrap_or(3).clamp(1, 8);
    let limit = q.limit.unwrap_or(60).clamp(1, 500);
    let mut budget = TREEMAP_BUDGET;
    let Some(root) = t.subtree(id, q.metric.by_alloc(), depth, limit, &mut budget) else {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    };
    Json(TreeResponse {
        root,
        version: t.version,
        truncated: budget == 0,
    })
    .into_response()
}

#[derive(Serialize)]
struct TypesResponse {
    types: Vec<ExtStat>,
    version: u64,
}

async fn types(State(state): State<AppState>, Path(id): Path<u32>) -> impl IntoResponse {
    let t = state.scanner.tree.read().unwrap();
    if t.get(id).is_none() {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    }
    Json(TypesResponse {
        types: t.by_extension(id),
        version: t.version,
    })
    .into_response()
}

#[derive(Serialize)]
struct LargestResponse {
    files: Vec<LargeFile>,
    version: u64,
}

async fn largest(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<ViewQuery>,
) -> impl IntoResponse {
    let t = state.scanner.tree.read().unwrap();
    if t.get(id).is_none() {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    Json(LargestResponse {
        files: t.largest_files(id, q.metric.by_alloc(), limit),
        version: t.version,
    })
    .into_response()
}

async fn errors(State(state): State<AppState>) -> impl IntoResponse {
    let t = state.scanner.tree.read().unwrap();
    Json(t.errors.clone())
}

async fn cancel(State(state): State<AppState>) -> impl IntoResponse {
    state.scanner.cancel();
    StatusCode::ACCEPTED
}
