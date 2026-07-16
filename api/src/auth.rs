use std::net::{IpAddr, SocketAddr};

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Request, State};
use axum::http::header::COOKIE;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use crate::entities::sessions;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "sb_session";

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash error: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub fn new_session_token() -> String {
    format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

pub async fn create_session(state: &AppState, user_id: i32) -> ApiResult<String> {
    let token = new_session_token();
    let now = Utc::now();
    let model = sessions::ActiveModel {
        id: Set(token.clone()),
        user_id: Set(user_id),
        expires_at: Set((now + Duration::days(state.cfg.session_ttl_days)).into()),
        created_at: Set(now.into()),
    };
    model.insert(&state.db()).await?;

    // 顺手清理过期会话
    let _ = sessions::Entity::delete_many()
        .filter(sessions::Column::ExpiresAt.lt(now))
        .exec(&state.db())
        .await;

    Ok(token)
}

pub fn session_cookie(state: &AppState, token: &str, max_age_secs: i64) -> String {
    let secure = if state.cfg.cookie_secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"
    )
}

pub fn read_session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(&format!("{SESSION_COOKIE}=")) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 提取客户端 IP：优先 X-Forwarded-For（反代场景），否则用连接地址
pub fn client_ip(headers: &HeaderMap, addr: Option<&SocketAddr>) -> IpAddr {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = xff.split(',').next() {
            if let Ok(ip) = first.trim().parse::<IpAddr>() {
                return ip;
            }
        }
    }
    addr.map(|a| a.ip())
        .unwrap_or_else(|| IpAddr::from([127, 0, 0, 1]))
}

pub async fn session_user_id(state: &AppState, headers: &HeaderMap) -> Option<i32> {
    let token = read_session_cookie(headers)?;
    let session = sessions::Entity::find_by_id(token)
        .one(&state.db())
        .await
        .ok()??;
    if session.expires_at < Utc::now() {
        return None;
    }
    Some(session.user_id)
}

/// 管理端守卫中间件
pub async fn require_admin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    match session_user_id(&state, request.headers()).await {
        Some(_) => Ok(next.run(request).await),
        None => Err(ApiError::unauthorized()),
    }
}
