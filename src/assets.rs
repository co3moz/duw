//! The built web UI, embedded into the binary.
//!
//! In debug builds `rust-embed` reads from disk, so `npm run dev`-style
//! iteration works without recompiling. Release builds bake the files in, which
//! is what makes `cargo install duw` a single self-contained binary.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Dist;

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Dist::get(path) {
        Some(file) => reply(path, file),
        // Unknown paths fall back to the SPA entry point.
        None => match Dist::get("index.html") {
            Some(file) => reply("index.html", file),
            None => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "web UI assets are missing from this build",
            )
                .into_response(),
        },
    }
}

fn reply(path: &str, file: rust_embed::EmbeddedFile) -> Response {
    let mime = file.metadata.mimetype().to_string();
    // Hashed asset filenames are safe to cache; index.html must not be.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        file.data.into_owned(),
    )
        .into_response()
}
