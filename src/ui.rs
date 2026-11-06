//! Embedded static web UI (built from web/dist by `npm run build`; see web/).
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use include_dir::{include_dir, Dir};

static DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

pub async fn static_handler(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(file) = DIST.get_file(path) {
        let mime = mime_for(path);
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime)],
            file.contents().to_vec(),
        )
            .into_response();
    }
    // SPA fallback → index.html
    if let Some(file) = DIST.get_file("index.html") {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            file.contents().to_vec(),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        build_placeholder(),
    )
        .into_response()
}

fn build_placeholder() -> Vec<u8> {
    // fallback used only before `npm run build` populates web/dist
    br#"<!doctype html><meta charset="utf-8"><title>WagglePost</title>
<p>WagglePost core is running. Run <code>npm run build</code> in <code>web/</code> and restart to serve the UI.</p>
"#.to_vec()
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}
