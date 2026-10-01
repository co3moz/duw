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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn root_serves_index_html_without_caching() {
        let response = serve(Uri::from_static("/")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/html");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(body
            .windows(b"<!doctype".len())
            .any(|w| w.eq_ignore_ascii_case(b"<!doctype")));
    }

    #[tokio::test]
    async fn unknown_routes_fall_back_to_the_spa_entry_point() {
        let root = serve(Uri::from_static("/")).await;
        let fallback = serve(Uri::from_static("/a/client/route")).await;
        let root_body = to_bytes(root.into_body(), usize::MAX).await.unwrap();
        let fallback_body = to_bytes(fallback.into_body(), usize::MAX).await.unwrap();
        assert_eq!(fallback_body, root_body);
    }

    #[tokio::test]
    async fn hashed_assets_use_immutable_cache_headers() {
        let asset = Dist::iter()
            .find(|path| path.starts_with("assets/"))
            .expect("web build should contain a hashed asset");
        let response = serve(Uri::try_from(format!("/{asset}")).unwrap()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert!(response.headers().get(header::CONTENT_TYPE).is_some());
    }
}
