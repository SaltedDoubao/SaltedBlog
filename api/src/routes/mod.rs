pub mod admin_backup;
pub mod admin_logs;
pub mod admin_misc;
pub mod admin_news;
pub mod admin_posts;
pub mod admin_taxonomy;
pub mod auth;
pub mod dto;
pub mod public;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, Response, StatusCode};
use axum::middleware;
use axum::Router;
use tokio_util::io::ReaderStream;
use tower_http::timeout::RequestBodyTimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    let admin = Router::new()
        .merge(admin_posts::router().layer(RequestBodyTimeoutLayer::new(
            std::time::Duration::from_secs(30),
        )))
        .merge(admin_taxonomy::router().layer(RequestBodyTimeoutLayer::new(
            std::time::Duration::from_secs(30),
        )))
        .merge(
            admin_misc::router(state.cfg.upload_max_mb).layer(RequestBodyTimeoutLayer::new(
                std::time::Duration::from_secs(60),
            )),
        )
        .merge(admin_news::router().layer(RequestBodyTimeoutLayer::new(
            std::time::Duration::from_secs(60),
        )))
        .merge(admin_logs::router().layer(RequestBodyTimeoutLayer::new(
            std::time::Duration::from_secs(30),
        )))
        .merge(admin_backup::router(state.cfg.backup_upload_max_mb).layer(
            RequestBodyTimeoutLayer::new(std::time::Duration::from_secs(15 * 60)),
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::logging::admin_request_log,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_admin,
        ));

    let api = Router::new()
        .merge(public::router())
        .nest(
            "/auth",
            auth::public_router()
                .layer(RequestBodyTimeoutLayer::new(
                    std::time::Duration::from_secs(15),
                ))
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    crate::auth::require_auth_origin,
                ))
                .merge(
                    auth::protected_router()
                        .layer(RequestBodyTimeoutLayer::new(
                            std::time::Duration::from_secs(15),
                        ))
                        .layer(middleware::from_fn_with_state(
                            state.clone(),
                            crate::auth::require_admin,
                        )),
                ),
        )
        .nest("/admin", admin);

    Router::new()
        .nest("/api", api)
        .route("/uploads/{*path}", axum::routing::get(serve_upload))
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::logging::public_request_log,
        ))
        .layer(middleware::from_fn(crate::logging::request_id_middleware))
        .with_state(state)
}

async fn serve_upload(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<Response<Body>, StatusCode> {
    let rel = std::path::Path::new(&path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let ext = rel
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let content_type = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        _ => return Err(StatusCode::NOT_FOUND),
    };
    let file = tokio::fs::File::open(state.cfg.upload_dir.join(rel))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let body = Body::from_stream(ReaderStream::new(file));
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .header("content-security-policy", "default-src 'none'; sandbox")
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
