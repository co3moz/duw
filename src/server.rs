//! HTTP API. Everything the UI needs is derived from the live tree, so the same
//! endpoints work during and after a scan.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::{Stream, StreamExt};

use crate::actions;
use crate::assets;
use crate::dupes::{DupeGroup, DupeProgress, Dupes, Phase};
use crate::fsext;
use crate::scan::Scanner;
use crate::snapshots;
use crate::tree::{
    Crumb, Entry, ExtStat, LargeFile, Rollup, SearchFilter, SearchHit, SortKey, Stats, SubtreeNode,
    ROOT,
};

const DEFAULT_LIMIT: usize = 400;
const MAX_LIMIT: usize = 5000;
/// Upper bound on the rectangles a single treemap request may produce.
const TREEMAP_BUDGET: usize = 4000;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone)]
pub struct AppState {
    pub scanner: Arc<Scanner>,
    pub dupes: Arc<Dupes>,
    pub root: String,
    pub local_only: bool,
    /// Threshold the UI starts from; it can ask for a different one.
    pub dupes_min: u64,
    /// Flipped to `true` on Ctrl+C so long-lived responses can end themselves.
    pub shutdown: watch::Receiver<bool>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/state", get(state_handler))
        .route("/api/events", get(events))
        .route("/api/node/{id}", get(node))
        .route("/api/tree/{id}", get(subtree))
        .route("/api/types/{id}", get(types))
        .route("/api/largest/{id}", get(largest))
        .route("/api/search/{id}", get(search))
        .route("/api/errors", get(errors))
        .route("/api/cancel", post(cancel))
        .route("/api/abs/{id}", get(abs_path))
        .route("/api/reveal/{id}", post(reveal))
        .route("/api/trash/{id}", post(trash_node))
        .route("/api/snapshots", get(list_snapshots).post(save_snapshot))
        .route("/api/snapshots/{name}", delete(delete_snapshot))
        .route("/api/snapshots/{name}/diff", get(snapshot_diff))
        .route(
            "/api/duplicates/{id}",
            get(duplicates).post(start_duplicates),
        )
        .route("/api/duplicates/cancel", post(cancel_duplicates))
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
    dupes: DupeProgress,
}

#[derive(Serialize)]
struct FullState {
    root: String,
    root_id: u32,
    platform: Platform,
    /// Whether cloud placeholders are being hidden, so the UI knows when the
    /// cloud markers on entries are actually being acted on.
    local_only: bool,
    dupes_min: u64,
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
        dupes: state.dupes.progress(),
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
        local_only: state.local_only,
        dupes_min: state.dupes_min,
        progress: progress_of(&state),
    })
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let shutdown = state.shutdown.clone();

    // This response would otherwise never end, and a graceful shutdown waits
    // for every in-flight response: an open browser tab would keep Ctrl+C from
    // ever stopping the process. Ending the stream on the next tick after the
    // signal closes that door.
    let stream = IntervalStream::new(tokio::time::interval(PROGRESS_INTERVAL))
        .take_while(move |_| !*shutdown.borrow())
        .map(move |_| {
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
    #[serde(default)]
    sort: Sort,
    /// Ascending order; the default is biggest/newest first.
    #[serde(default)]
    asc: bool,
    limit: Option<usize>,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Sort {
    #[default]
    Size,
    Name,
    Mtime,
    Count,
}

impl Sort {
    fn key(self) -> SortKey {
        match self {
            Sort::Size => SortKey::Size,
            Sort::Name => SortKey::Name,
            Sort::Mtime => SortKey::Mtime,
            Sort::Count => SortKey::Count,
        }
    }
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
    let view = t.children_view(
        id,
        q.metric.by_alloc(),
        q.sort.key(),
        q.asc,
        clamp_limit(q.limit),
    );
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

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
    /// Comma-separated extensions, with or without a leading dot.
    ext: Option<String>,
    min: Option<u64>,
    max: Option<u64>,
    /// Match entries at least this many days old.
    age: Option<u64>,
    #[serde(default)]
    metric: Metric,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct SearchResponse {
    hits: Vec<SearchHit>,
    version: u64,
    truncated: bool,
}

/// Entries matching a filter anywhere under `id`. The list panel uses this
/// while a filter is active, so a search reaches the whole subtree rather than
/// just the folder in view.
async fn search(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(p): Query<SearchParams>,
) -> impl IntoResponse {
    let t = state.scanner.tree.read().unwrap();
    if t.get(id).is_none() {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    }

    let query = p.q.unwrap_or_default().to_ascii_lowercase();
    let exts: Vec<String> = p
        .ext
        .unwrap_or_default()
        .split(',')
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let filter = SearchFilter {
        query: &query,
        exts: &exts,
        min: p.min.unwrap_or(0),
        max: p.max.unwrap_or(u64::MAX),
        max_mtime: p.age.map(|days| now - days as i64 * 86_400),
    };

    let limit = p.limit.unwrap_or(500).clamp(1, MAX_LIMIT);
    let mut hits = t.search(id, &filter, p.metric.by_alloc(), limit + 1, 20_000);
    let truncated = hits.len() > limit;
    hits.truncate(limit);

    Json(SearchResponse {
        hits,
        version: t.version,
        truncated,
    })
    .into_response()
}

async fn cancel(State(state): State<AppState>) -> impl IntoResponse {
    state.scanner.cancel();
    StatusCode::ACCEPTED
}

/// Rejects cross-site requests so an arbitrary web page cannot drive file
/// actions on a localhost server. Browsers send `Origin` on POST requests;
/// command-line clients such as curl do not, and are allowed through.
fn cross_site(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    match origin.split_once("://") {
        Some((_, rest)) => rest != host,
        // `Origin: null` and other opaque values are not trustworthy.
        None => true,
    }
}

/// Absolute path of a node, built from the scan root and the tree-relative
/// path. It never touches the filesystem, so it also works for entries that
/// have been removed from the tree.
fn node_path(state: &AppState, id: u32) -> Option<std::path::PathBuf> {
    let t = state.scanner.tree.read().unwrap();
    t.get(id)?;
    let mut path = std::path::PathBuf::from(&state.root);
    for part in t.rel_path(id).split('/').filter(|p| !p.is_empty()) {
        path.push(part);
    }
    Some(path)
}

#[derive(Serialize)]
struct PathResponse {
    path: String,
}

async fn abs_path(State(state): State<AppState>, Path(id): Path<u32>) -> impl IntoResponse {
    match node_path(&state, id) {
        Some(path) => Json(PathResponse {
            path: path.display().to_string(),
        })
        .into_response(),
        None => (StatusCode::NOT_FOUND, "no such node").into_response(),
    }
}

async fn reveal(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if cross_site(&headers) {
        return (StatusCode::FORBIDDEN, "cross-site request rejected").into_response();
    }
    let Some(path) = node_path(&state, id) else {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    };
    match actions::reveal(&path) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not open the file manager: {e}"),
        )
            .into_response(),
    }
}

async fn trash_node(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if cross_site(&headers) {
        return (StatusCode::FORBIDDEN, "cross-site request rejected").into_response();
    }
    if id == ROOT {
        return (StatusCode::BAD_REQUEST, "cannot move the scan root").into_response();
    }
    if !state.scanner.is_done() {
        return (StatusCode::CONFLICT, "a scan is still running").into_response();
    }
    let Some(path) = node_path(&state, id) else {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    };
    if let Err(e) = actions::to_trash(&path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not move to trash: {e}"),
        )
            .into_response();
    }
    // The bytes are gone either way; a failed detach would only leave the tree
    // stale until the next scan.
    state.scanner.tree.write().unwrap().remove(id);
    state.dupes.forget(id);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Serialize)]
struct SnapshotList {
    snapshots: Vec<snapshots::SnapshotMeta>,
    /// Where the files live, shown so users can find (or back up) them.
    dir: String,
}

async fn list_snapshots() -> impl IntoResponse {
    let dir = snapshots::dir()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    match snapshots::list() {
        Ok(snapshots) => Json(SnapshotList { snapshots, dir }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot list snapshots: {e}"),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct SaveSnapshotQuery {
    name: Option<String>,
}

/// Saves the current tree so a later scan can be compared against it. The
/// tree is compact when the file count is manageable, but can still be sizable.
async fn save_snapshot(
    State(state): State<AppState>,
    Query(q): Query<SaveSnapshotQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if cross_site(&headers) {
        return (StatusCode::FORBIDDEN, "cross-site request rejected").into_response();
    }
    if !state.scanner.is_done() {
        return (StatusCode::CONFLICT, "a scan is still running").into_response();
    }
    let name = q
        .name
        .unwrap_or_else(|| format!("snapshot-{}", snapshots::now()));
    if !snapshots::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid snapshot name").into_response();
    }
    let snapshot = snapshots::Snapshot {
        name,
        root: state.root.clone(),
        created: snapshots::now(),
        entries: state.scanner.tree.read().unwrap().snapshot_entries(),
    };
    match snapshots::save(&snapshot) {
        Ok(file_bytes) => Json(snapshots::SnapshotMeta::of(&snapshot, file_bytes)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot save snapshot: {e}"),
        )
            .into_response(),
    }
}

async fn delete_snapshot(Path(name): Path<String>, headers: HeaderMap) -> impl IntoResponse {
    if cross_site(&headers) {
        return (StatusCode::FORBIDDEN, "cross-site request rejected").into_response();
    }
    match snapshots::delete(&name) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, "no such snapshot").into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot delete snapshot: {e}"),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct DiffQuery {
    limit: Option<usize>,
}

#[derive(Serialize)]
struct DiffResponse {
    from: snapshots::SnapshotMeta,
    to_root: String,
    to_created: u64,
    #[serde(flatten)]
    diff: snapshots::DiffResult,
}

/// Compares a saved snapshot with the tree that is currently loaded.
async fn snapshot_diff(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DiffQuery>,
) -> impl IntoResponse {
    let from = match snapshots::load(&name) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::NOT_FOUND, "no such snapshot").into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot read snapshot: {e}"),
            )
                .into_response()
        }
    };
    let current = state.scanner.tree.read().unwrap().snapshot_entries();
    let limit = q.limit.unwrap_or(200).clamp(1, MAX_LIMIT);
    Json(DiffResponse {
        from: snapshots::SnapshotMeta::of(&from, snapshots::file_size(&name).unwrap_or(0)),
        to_root: state.root.clone(),
        to_created: snapshots::now(),
        diff: snapshots::diff(&from.entries, &current, limit),
    })
    .into_response()
}

#[derive(Deserialize)]
struct DupeQuery {
    /// Smallest file to consider; defaults to whatever the CLI was given.
    min: Option<u64>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct DupesResponse {
    progress: DupeProgress,
    groups: Vec<DupeGroup>,
    total_groups: usize,
    truncated: bool,
}

/// Reads whatever the duplicate scanner has produced so far. The scan itself is
/// started separately, so polling this while it runs is cheap.
async fn duplicates(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<DupeQuery>,
) -> impl IntoResponse {
    if state.scanner.tree.read().unwrap().get(id).is_none() {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    }
    let limit = q.limit.unwrap_or(200).clamp(1, 5000);
    let progress = state.dupes.progress();
    // The results belong to whatever scope produced them. Asking for another
    // folder must not surface the previous folder's groups.
    let (groups, total_groups) = if progress.scope == id && progress.phase != Phase::Idle {
        let mut all = state.dupes.groups();
        let total = all.len();
        all.truncate(limit);
        (all, total)
    } else {
        (Vec::new(), 0)
    };

    Json(DupesResponse {
        progress,
        groups,
        total_groups,
        truncated: total_groups > limit,
    })
    .into_response()
}

/// Starts (or restarts) a duplicate scan for one directory. Changing the
/// threshold from the UI lands here, and cached digests make a rerun cheap.
async fn start_duplicates(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<DupeQuery>,
) -> impl IntoResponse {
    if state.scanner.tree.read().unwrap().get(id).is_none() {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    }
    let min = q.min.unwrap_or(state.dupes_min);
    state.dupes.start(id, min);
    Json(state.dupes.progress()).into_response()
}

async fn cancel_duplicates(State(state): State<AppState>) -> impl IntoResponse {
    state.dupes.cancel();
    StatusCode::ACCEPTED
}
