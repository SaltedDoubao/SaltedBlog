use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::{header::SET_COOKIE, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{routing::get, routing::post, Json, Router};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{
    client_ip, create_session, read_session_cookie, session_cookie, session_user_id,
    verify_password,
};
use crate::entities::{sessions, users};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", get(me))
}

#[derive(Deserialize)]
struct LoginInput {
    username: String,
    password: String,
}

async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<LoginInput>,
) -> ApiResult<impl IntoResponse> {
    let ip = client_ip(&headers, Some(&addr));
    if state.limiter.is_blocked(ip) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed attempts, try again later",
        ));
    }

    let user = users::Entity::find()
        .filter(users::Column::Username.eq(input.username.trim()))
        .one(&state.db())
        .await?;

    let Some(user) = user else {
        state.limiter.record_failure(ip);
        return Err(ApiError::unauthorized());
    };
    if !verify_password(&input.password, &user.password_hash) {
        state.limiter.record_failure(ip);
        return Err(ApiError::unauthorized());
    }

    state.limiter.clear(ip);
    let token = create_session(&state, user.id).await?;
    let cookie = session_cookie(&state, &token, state.cfg.session_ttl_days * 86400);

    let mut response = Json(json!({ "username": user.username })).into_response();
    response
        .headers_mut()
        .insert(SET_COOKIE, cookie.parse().unwrap());
    Ok(response)
}

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    if let Some(token) = read_session_cookie(&headers) {
        let _ = sessions::Entity::delete_by_id(token).exec(&state.db()).await;
    }
    let cookie = session_cookie(&state, "", 0);
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(SET_COOKIE, cookie.parse().unwrap());
    Ok(response)
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    let Some(user_id) = session_user_id(&state, &headers).await else {
        return Err(ApiError::unauthorized());
    };
    let user = users::Entity::find_by_id(user_id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(Json(json!({ "username": user.username })))
}
