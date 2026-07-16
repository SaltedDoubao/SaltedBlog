pub mod admin_misc;
pub mod admin_posts;
pub mod admin_taxonomy;
pub mod auth;
pub mod dto;
pub mod public;

use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::Router;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    let admin = Router::new()
        .merge(admin_posts::router())
        .merge(admin_taxonomy::router())
        .merge(admin_misc::router())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_admin,
        ));

    let api = Router::new()
        .merge(public::router())
        .nest("/auth", auth::router())
        .nest("/admin", admin);

    let max_body = state.cfg.upload_max_mb * 1024 * 1024;

    Router::new()
        .nest("/api", api)
        .nest_service("/uploads", ServeDir::new(&state.cfg.upload_dir))
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .layer(DefaultBodyLimit::max(max_body))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
