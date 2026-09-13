//! HTTP API. Everything the UI needs is derived from the live tree, so the same
//! endpoints work during and after a scan.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::response::Response;
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
        .route_layer(middleware::from_fn(protect_mutations))
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

#[derive(Serialize)]
struct TreeResponse {
    root: SubtreeNode,
    version: u64,
    truncated: bool,
}

async fn subtree(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<SearchParams>,
) -> impl IntoResponse {
    let t = state.scanner.tree.read().unwrap();
    let depth = q.depth.unwrap_or(3).clamp(1, 8);
    let limit = q.limit.unwrap_or(60).clamp(1, 500);
    let mut budget = TREEMAP_BUDGET;
    let root = if q.has_filter() {
        q.with_filter(|filter| {
            t.filtered_subtree(id, filter, q.metric.by_alloc(), depth, limit, &mut budget)
        })
    } else {
        t.subtree(id, q.metric.by_alloc(), depth, limit, &mut budget)
    };
    let Some(root) = root else {
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
    depth: Option<u16>,
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

impl SearchParams {
    fn has_filter(&self) -> bool {
        self.q.is_some()
            || self.ext.is_some()
            || self.min.is_some()
            || self.max.is_some()
            || self.age.is_some()
    }

    fn with_filter<R>(&self, f: impl FnOnce(&SearchFilter<'_>) -> R) -> R {
        let query = self
            .q
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let exts: Vec<String> = self
            .ext
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        let now = snapshots::now().min(i64::MAX as u64) as i64;
        f(&SearchFilter {
            query: &query,
            exts: &exts,
            min: self.min.unwrap_or(0),
            max: self.max.unwrap_or(u64::MAX),
            max_mtime: self.age.map(|days| {
                now.saturating_sub(days.saturating_mul(86_400).min(i64::MAX as u64) as i64)
            }),
        })
    }
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

    let limit = p.limit.unwrap_or(500).clamp(1, MAX_LIMIT);
    let mut hits =
        p.with_filter(|filter| t.search(id, filter, p.metric.by_alloc(), limit + 1, 20_000));
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

async fn protect_mutations(request: Request, next: Next) -> Response {
    if !request.method().is_safe() && cross_site(request.headers()) {
        return (StatusCode::FORBIDDEN, "cross-site request rejected").into_response();
    }
    next.run(request).await
}

/// Absolute path of a node, built from the scan root and the tree-relative
/// path. Removed entries and descendants of removed directories are rejected.
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
    state.dupes.forget_removed();
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
    let result = {
        let tree = state.scanner.tree.read().unwrap();
        let mut files = tree.sorted_files();
        snapshots::save(&name, &state.root, snapshots::now(), &mut files)
    };
    match result {
        Ok(meta) => Json(meta).into_response(),
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
    let from = match snapshots::meta(&name) {
        Ok(meta) => meta,
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
    let limit = q.limit.unwrap_or(200).clamp(1, MAX_LIMIT);
    let result = {
        let tree = state.scanner.tree.read().unwrap();
        snapshots::diff(&name, &tree, limit)
    };
    match result {
        Ok(diff) => Json(DiffResponse {
            from,
            to_root: state.root.clone(),
            to_created: snapshots::now(),
            diff,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot compare snapshot: {e}"),
        )
            .into_response(),
    }
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
/// threshold from the UI lands here. Every run checks current file contents.
async fn start_duplicates(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<DupeQuery>,
) -> impl IntoResponse {
    if !state.scanner.is_done() {
        return (StatusCode::CONFLICT, "a scan is still running").into_response();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::ScanOpts;
    use crate::tree::{Kind, NewEntry};
    use axum::body::Body;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        state: AppState,
        root: std::path::PathBuf,
    }
    impl Fixture {
        fn new(done: bool) -> Self {
            let root = std::env::temp_dir().join(format!(
                "duw-api-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            let scanner = Scanner::new(ScanOpts {
                root: root.clone(),
                one_file_system: false,
                dereference: false,
                count_links: false,
                max_depth: None,
                exclude: None,
                threads: Some(1),
                local_only: true,
            })
            .unwrap();
            if done {
                scanner.run();
            }
            let dupes = Dupes::new(scanner.tree.clone(), root.clone());
            let (_, shutdown) = watch::channel(false);
            let state = AppState {
                scanner,
                dupes,
                root: root.display().to_string(),
                local_only: true,
                dupes_min: 1,
                shutdown,
            };
            Self { state, root }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.root);
        }
    }

    #[tokio::test]
    async fn all_mutations_reject_cross_site_requests() {
        let f = Fixture::new(true);
        for (method, path) in [
            ("POST", "/api/cancel"),
            ("POST", "/api/duplicates/0"),
            ("POST", "/api/duplicates/cancel"),
            ("POST", "/api/trash/0"),
            ("POST", "/api/reveal/0"),
            ("POST", "/api/snapshots"),
            ("DELETE", "/api/snapshots/test"),
        ] {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:8080")
                .header("origin", "http://untrusted.example")
                .body(Body::empty())
                .unwrap();
            let response = router(f.state.clone()).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }
        assert!(!f.state.scanner.is_cancelled());
    }

    #[tokio::test]
    async fn duplicates_wait_for_main_scan_and_dev_origin_is_accepted() {
        let f = Fixture::new(false);
        let request = Request::builder()
            .method("POST")
            .uri("/api/duplicates/0")
            .header("host", "localhost:5173")
            .header("origin", "http://localhost:5173")
            .body(Body::empty())
            .unwrap();
        let response = router(f.state.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(f.state.dupes.progress().phase, Phase::Idle);
        let request = Request::builder()
            .method("POST")
            .uri("/api/cancel")
            .header("host", "localhost:5173")
            .header("origin", "http://localhost:5173")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router(f.state.clone())
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
    }

    #[tokio::test]
    async fn removed_ids_return_404_and_tree_accepts_search_filters() {
        let f = Fixture::new(true);
        {
            let mut t = f.state.scanner.tree.write().unwrap();
            t.add_children(
                ROOT,
                vec![NewEntry {
                    name: "needle.txt".into(),
                    kind: Kind::File,
                    size: 12,
                    alloc: 16,
                    mtime: 0,
                    err: false,
                    cloud: false,
                }],
            );
        }
        let request = Request::builder()
            .uri("/api/tree/0?q=needle&ext=txt&min=10&age=1&depth=3&limit=60")
            .body(Body::empty())
            .unwrap();
        let response = router(f.state.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 100_000)
            .await
            .unwrap();
        let data: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(data["root"]["size"], 12);
        f.state.scanner.tree.write().unwrap().remove(1);
        for path in [
            "/api/node/1",
            "/api/tree/1",
            "/api/abs/1",
            "/api/duplicates/1",
        ] {
            let request = Request::builder().uri(path).body(Body::empty()).unwrap();
            assert_eq!(
                router(f.state.clone())
                    .oneshot(request)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
    }
}
