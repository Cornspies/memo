//! Router, HTTP middleware and the `serve` loop.

use std::{net::SocketAddr, sync::PoisonError, time::Instant};

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Request, State},
    http::{HeaderValue, header},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, patch, post, put},
};
use memo_core::api::{API_VERSION, Health};

use crate::{
    account, attachments,
    auth::{self, ClientIp, LogDevice},
    error::ApiError,
    jobs, memos,
    ratelimit::client_ip,
    state::App,
    sync, web,
};

const JSON_LIMIT: usize = 1 << 20;
const SYNC_LIMIT: usize = 16 << 20;
const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' blob:; \
                   connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

pub fn router(app: App) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/auth/prelogin", post(auth::prelogin))
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/password", post(auth::change_password))
        .route("/auth/recover", post(auth::recover))
        .route("/auth/recover/complete", post(auth::recover_complete))
        .route("/me", get(account::me).delete(account::delete_me))
        .route("/devices", get(account::devices))
        .route("/devices/{id}", delete(account::delete_device))
        .route("/users/lookup", get(account::lookup))
        .route("/users/{id}", get(account::user))
        .route("/sync", post(sync::sync).layer(DefaultBodyLimit::max(SYNC_LIMIT)))
        .route("/memos/{id}/members", post(memos::add_member))
        .route("/memos/{id}/members/me", delete(memos::leave))
        .route("/memos/{id}/members/{user_id}", patch(memos::update_member))
        // Carries the whole memo, like a sync push.
        .route("/memos/{id}/revoke", post(memos::revoke).layer(DefaultBodyLimit::max(SYNC_LIMIT)))
        .route("/memos/{id}/revisions", get(memos::revisions))
        .route("/memos/{id}/revisions/{version}", get(memos::revision))
        .route(
            "/memos/{id}/attachments/{attachment_id}/{kind}",
            put(attachments::upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/attachments/{id}/{kind}", get(attachments::download))
        .fallback(|| async { ApiError::not_found() })
        .layer(DefaultBodyLimit::max(JSON_LIMIT));
    Router::new()
        .nest("/api/v1", api)
        .fallback(web::serve)
        .layer(middleware::from_fn_with_state(app.clone(), http_layer))
        .with_state(app)
}

async fn health() -> Json<Health> {
    Json(Health { api: API_VERSION, version: env!("CARGO_PKG_VERSION").into() })
}

/// Client IP, request log, and the headers every response gets.
async fn http_layer(State(app): State<App>, mut request: Request, next: Next) -> Response {
    let started = Instant::now();
    let peer = request.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    let ip = client_ip(request.headers(), peer, &app.config.trusted_proxies);
    let device = LogDevice::default();
    request.extensions_mut().insert(ClientIp(ip));
    request.extensions_mut().insert(device.clone());
    let (method, path) = (request.method().clone(), request.uri().path().to_owned());
    let is_api = path.starts_with("/api/");

    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("memo-api", HeaderValue::from(API_VERSION));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    if is_api {
        headers.entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("no-store"));
    } else {
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
        headers.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    }
    let device = *device.0.lock().unwrap_or_else(PoisonError::into_inner);
    tracing::info!(
        %method,
        path,
        status = response.status().as_u16(),
        ms = started.elapsed().as_millis() as u64,
        device = device.map(|d| d.to_string()).unwrap_or_default(),
        %ip,
    );
    response
}

pub async fn serve(app: App) -> Result<(), String> {
    let listener =
        tokio::net::TcpListener::bind(app.config.listen).await.map_err(|e| format!("{}: {e}", app.config.listen))?;
    tracing::info!("listening on {} (public url {})", app.config.listen, app.config.public_url);

    let gc_app = app.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            gc_app.limiter.prune();
            let app = gc_app.clone();
            match tokio::task::spawn_blocking(move || jobs::gc(&app.db, &app.config)).await {
                Ok(Ok(report)) => tracing::info!(?report, "gc"),
                Ok(Err(e)) => tracing::error!("gc failed: {}", e.message),
                Err(e) => tracing::error!("gc panicked: {e}"),
            }
        }
    });

    axum::serve(listener, router(app).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
