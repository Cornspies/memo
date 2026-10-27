//! The embedded web app (see `build.rs`). Hashed files under `assets/` are cached forever;
//! everything else is revalidated with an ETag.

use std::{collections::HashMap, sync::OnceLock};

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};

use crate::db::sha256;

include!(concat!(env!("OUT_DIR"), "/web_files.rs"));

struct File {
    bytes: &'static [u8],
    etag: HeaderValue,
}

fn files() -> &'static HashMap<&'static str, File> {
    static FILES_BY_PATH: OnceLock<HashMap<&'static str, File>> = OnceLock::new();
    FILES_BY_PATH.get_or_init(|| {
        FILES
            .iter()
            .map(|&(path, bytes)| {
                let hash: String = sha256(bytes)[..16].iter().map(|b| format!("{b:02x}")).collect();
                let etag = HeaderValue::from_str(&format!("\"{hash}\"")).expect("hex is ASCII");
                (path, File { bytes, etag })
            })
            .collect()
    })
}

pub fn is_built() -> bool {
    !FILES.is_empty()
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    if !is_built() {
        return (StatusCode::NOT_FOUND, "This server was built without the web app (see web/README.md).\n")
            .into_response();
    }
    let path = uri.path().trim_start_matches('/');
    // The app routes with the URL fragment, so any other path without an extension is the app.
    let path =
        if path.is_empty() || !path.rsplit('/').next().unwrap_or("").contains('.') { "index.html" } else { path };
    let Some(file) = files().get(path) else {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    };
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    let mut response = if headers.get(header::IF_NONE_MATCH) == Some(&file.etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, content_type(path))], file.bytes).into_response()
    };
    response.headers_mut().insert(header::ETAG, file.etag.clone());
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}
