use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Extension, State};
use axum::http::{header::SET_COOKIE, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::{
    routing::{get, post},
    Json, Router,
};
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{
    auth_cookie, client_ip, create_session, csrf_cookie, decrypt_secret, encrypt_secret,
    generate_recovery_codes, generate_totp_secret, hash_password, random_token, read_cookie,
    recovery_hash, require_step_up, sha256_hex, totp_setup, verify_password, verify_totp,
    AdminContext,
};
use crate::entities::{mfa_recovery_codes, preauth_tokens, sessions, users};
use crate::error::{ApiError, ApiResult};
use crate::logging::{record, EventContext, NewEvent, CATEGORY_AUTH, CATEGORY_SECURITY};
use crate::state::AppState;

pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/mfa/setup", get(mfa_setup))
        .route("/mfa/confirm", post(mfa_confirm))
        .route("/mfa/verify", post(mfa_verify))
}

pub fn protected_router() -> Router<AppState> {
    Router::new()
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route("/step-up", post(step_up))
        .route("/password", post(change_password))
        .route("/mfa/recovery-codes", post(regenerate_recovery_codes))
}

#[derive(Deserialize)]
struct LoginInput {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct CodeInput {
    code: String,
}

#[derive(Deserialize)]
struct StepUpInput {
    password: String,
    code: String,
}

#[derive(Deserialize)]
struct ChangePasswordInput {
    current_password: String,
    new_password: String,
}

fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers.get("user-agent").and_then(|v| v.to_str().ok())
}

async fn issue_full_session(
    state: &AppState,
    user_id: i32,
    ip: String,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let (token, csrf) = create_session(state, user_id, Some(ip), user_agent(headers)).await?;
    let cookie = auth_cookie(
        state,
        &token,
        state.cfg.session_absolute_hours * 3600,
        false,
    );
    let csrf_cookie = csrf_cookie(state, &csrf, state.cfg.session_absolute_hours * 3600);
    let user = users::Entity::find_by_id(user_id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let mut response = Json(json!({
        "username": user.username,
        "csrf_token": csrf,
        "mfa_enabled": user.mfa_enabled_at.is_some(),
    }))
    .into_response();
    response.headers_mut().append(
        SET_COOKIE,
        cookie
            .parse()
            .map_err(|_| ApiError::internal("cookie build"))?,
    );
    response.headers_mut().append(
        SET_COOKIE,
        csrf_cookie
            .parse()
            .map_err(|_| ApiError::internal("cookie build"))?,
    );
    Ok(response)
}

async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<LoginInput>,
) -> ApiResult<Response> {
    if input.username.len() > 64 || input.password.len() > 256 {
        return Err(ApiError::unauthorized());
    }
    let ip = client_ip(&headers, Some(&addr), &state.cfg);
    let account = input.username.trim();
    if let Some(retry_after) = state.limiter.retry_after(ip, account) {
        record(
            &state,
            NewEvent {
                category: CATEGORY_SECURITY,
                level: "warn",
                event_type: "auth.rate_limited".into(),
                outcome: "blocked",
                source_ip: Some(ip.to_string()),
                summary: format!(
                    "登录请求已被限流，约 {} 秒后重试",
                    retry_after.as_secs().max(1)
                ),
                ..Default::default()
            },
        )
        .await;
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed attempts, try again later",
        ));
    }
    let user = users::Entity::find()
        .filter(users::Column::Username.eq(account))
        .one(&state.db())
        .await?;
    let dummy = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$R6UiIibQ4taM0M7Q37B4CiqJ5J8klv7O3hAIE5DEcrE";
    let valid = user.as_ref().map_or_else(
        || {
            let _ = verify_password(&input.password, dummy);
            false
        },
        |u| verify_password(&input.password, &u.password_hash),
    );
    if !valid {
        state.limiter.record_failure(ip, account);
        record(
            &state,
            NewEvent {
                category: CATEGORY_AUTH,
                level: "warn",
                event_type: "auth.login_failed".into(),
                outcome: "failure",
                source_ip: Some(ip.to_string()),
                summary: "管理员登录失败".into(),
                ..Default::default()
            },
        )
        .await;
        return Err(ApiError::unauthorized());
    }
    let user = user.expect("validated user");
    state.limiter.clear(ip, account);
    if !state.cfg.mfa_required && user.mfa_enabled_at.is_none() {
        record(
            &state,
            NewEvent {
                category: CATEGORY_AUTH,
                level: "info",
                event_type: "auth.login_success".into(),
                outcome: "success",
                actor_user_id: Some(user.id),
                actor_name: Some(user.username.clone()),
                source_ip: Some(ip.to_string()),
                summary: "管理员登录成功（开发模式未启用 MFA）".into(),
                ..Default::default()
            },
        )
        .await;
        return issue_full_session(&state, user.id, ip.to_string(), &headers).await;
    }
    let token = random_token(32);
    let now = Utc::now();
    preauth_tokens::ActiveModel {
        id: Set(sha256_hex(&token)),
        user_id: Set(user.id),
        expires_at: Set((now + Duration::minutes(5)).into()),
        mfa_secret_enc: Set(None),
        attempts: Set(0),
        created_at: Set(now.into()),
    }
    .insert(&state.db())
    .await?;
    let cookie = auth_cookie(&state, &token, 300, true);
    let mut response = (
        StatusCode::ACCEPTED,
        Json(json!({ "next": if user.mfa_enabled_at.is_some() { "totp" } else { "enroll" } })),
    )
        .into_response();
    response.headers_mut().append(
        SET_COOKIE,
        cookie
            .parse()
            .map_err(|_| ApiError::internal("cookie build"))?,
    );
    Ok(response)
}

async fn load_preauth(state: &AppState, headers: &HeaderMap) -> ApiResult<preauth_tokens::Model> {
    let token = read_cookie(headers, state, true).ok_or_else(ApiError::unauthorized)?;
    let row = preauth_tokens::Entity::find_by_id(sha256_hex(token))
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    if row.expires_at < Utc::now() || row.attempts >= 8 {
        return Err(ApiError::unauthorized());
    }
    Ok(row)
}

async fn mfa_setup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let row = load_preauth(&state, &headers).await?;
    let user = users::Entity::find_by_id(row.user_id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    if user.mfa_enabled_at.is_some() {
        return Err(ApiError::bad_request("MFA already enabled"));
    }
    let secret = if let Some(enc) = &row.mfa_secret_enc {
        decrypt_secret(&state.cfg.mfa_encryption_key, enc)?
    } else {
        let secret = generate_totp_secret();
        let mut model: preauth_tokens::ActiveModel = row.into();
        model.mfa_secret_enc = Set(Some(encrypt_secret(
            &state.cfg.mfa_encryption_key,
            &secret,
        )?));
        model.update(&state.db()).await?;
        secret
    };
    let (secret, otpauth_uri) = totp_setup(&secret, &user.username);
    Ok(Json(
        json!({ "secret": secret, "otpauth_uri": otpauth_uri }),
    ))
}

async fn mfa_confirm(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<CodeInput>,
) -> ApiResult<impl IntoResponse> {
    let row = load_preauth(&state, &headers).await?;
    let enc = row
        .mfa_secret_enc
        .clone()
        .ok_or_else(|| ApiError::bad_request("start MFA setup first"))?;
    let secret = decrypt_secret(&state.cfg.mfa_encryption_key, &enc)?;
    let Some(step) = verify_totp(&secret, &input.code, None) else {
        let mut model: preauth_tokens::ActiveModel = row.into();
        model.attempts = Set(model.attempts.take().unwrap_or(0) + 1);
        let _ = model.update(&state.db()).await;
        return Err(ApiError::unauthorized());
    };
    let now = Utc::now();
    let mut user_model: users::ActiveModel = users::Entity::find_by_id(row.user_id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::unauthorized)?
        .into();
    user_model.mfa_secret_enc = Set(Some(enc));
    user_model.mfa_enabled_at = Set(Some(now.into()));
    user_model.last_totp_step = Set(Some(step));
    let user = user_model.update(&state.db()).await?;
    mfa_recovery_codes::Entity::delete_many()
        .filter(mfa_recovery_codes::Column::UserId.eq(user.id))
        .exec(&state.db())
        .await?;
    let codes = generate_recovery_codes();
    for code in &codes {
        mfa_recovery_codes::ActiveModel {
            user_id: Set(user.id),
            code_hash: Set(recovery_hash(&state.cfg.mfa_encryption_key, code)),
            used_at: Set(None),
            created_at: Set(now.into()),
            ..Default::default()
        }
        .insert(&state.db())
        .await?;
    }
    preauth_tokens::Entity::delete_by_id(row.id)
        .exec(&state.db())
        .await?;
    let ip = client_ip(&headers, Some(&addr), &state.cfg).to_string();
    let (token, csrf) =
        create_session(&state, user.id, Some(ip.clone()), user_agent(&headers)).await?;
    record(
        &state,
        NewEvent {
            category: CATEGORY_AUTH,
            level: "info",
            event_type: "auth.mfa_enrolled".into(),
            outcome: "success",
            actor_user_id: Some(user.id),
            actor_name: Some(user.username.clone()),
            source_ip: Some(ip),
            summary: "管理员已启用 TOTP".into(),
            ..Default::default()
        },
    )
    .await;
    let cookie = auth_cookie(
        &state,
        &token,
        state.cfg.session_absolute_hours * 3600,
        false,
    );
    let csrf_cookie_header = csrf_cookie(&state, &csrf, state.cfg.session_absolute_hours * 3600);
    let clear_pre = auth_cookie(&state, "", 0, true);
    let mut response = Json(json!({ "username": user.username, "csrf_token": csrf, "recovery_codes": codes, "mfa_enabled": true })).into_response();
    response
        .headers_mut()
        .append(SET_COOKIE, cookie.parse().unwrap());
    response
        .headers_mut()
        .append(SET_COOKIE, csrf_cookie_header.parse().unwrap());
    response
        .headers_mut()
        .append(SET_COOKIE, clear_pre.parse().unwrap());
    Ok(response)
}

async fn mfa_verify(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<CodeInput>,
) -> ApiResult<impl IntoResponse> {
    let row = load_preauth(&state, &headers).await?;
    let user = users::Entity::find_by_id(row.user_id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let mut accepted = false;
    let mut new_step = user.last_totp_step;
    if let Some(enc) = &user.mfa_secret_enc {
        let secret = decrypt_secret(&state.cfg.mfa_encryption_key, enc)?;
        if let Some(step) = verify_totp(&secret, &input.code, user.last_totp_step) {
            accepted = true;
            new_step = Some(step);
        }
    }
    if !accepted {
        let hash = recovery_hash(&state.cfg.mfa_encryption_key, &input.code);
        if let Some(code) = mfa_recovery_codes::Entity::find()
            .filter(mfa_recovery_codes::Column::UserId.eq(user.id))
            .filter(mfa_recovery_codes::Column::CodeHash.eq(hash))
            .filter(mfa_recovery_codes::Column::UsedAt.is_null())
            .one(&state.db())
            .await?
        {
            let mut model: mfa_recovery_codes::ActiveModel = code.into();
            model.used_at = Set(Some(Utc::now().into()));
            model.update(&state.db()).await?;
            accepted = true;
        }
    }
    if !accepted {
        let mut model: preauth_tokens::ActiveModel = row.clone().into();
        model.attempts = Set(row.attempts + 1);
        let _ = model.update(&state.db()).await;
        record(
            &state,
            NewEvent {
                category: CATEGORY_AUTH,
                level: "warn",
                event_type: "auth.mfa_failed".into(),
                outcome: "failure",
                actor_user_id: Some(user.id),
                actor_name: Some(user.username),
                summary: "TOTP 或恢复码验证失败".into(),
                ..Default::default()
            },
        )
        .await;
        return Err(ApiError::unauthorized());
    }
    if new_step != user.last_totp_step {
        let mut model: users::ActiveModel = user.clone().into();
        model.last_totp_step = Set(new_step);
        model.update(&state.db()).await?;
    }
    preauth_tokens::Entity::delete_by_id(row.id)
        .exec(&state.db())
        .await?;
    let ip = client_ip(&headers, Some(&addr), &state.cfg).to_string();
    record(
        &state,
        NewEvent {
            category: CATEGORY_AUTH,
            level: "info",
            event_type: "auth.login_success".into(),
            outcome: "success",
            actor_user_id: Some(user.id),
            actor_name: Some(user.username.clone()),
            source_ip: Some(ip.clone()),
            summary: "管理员双因子登录成功".into(),
            ..Default::default()
        },
    )
    .await;
    issue_full_session(&state, user.id, ip, &headers).await
}

async fn logout(
    State(state): State<AppState>,
    Extension(ctx): Extension<AdminContext>,
    Extension(event_ctx): Extension<EventContext>,
) -> ApiResult<impl IntoResponse> {
    sessions::Entity::delete_by_id(ctx.session_hash)
        .exec(&state.db())
        .await?;
    record(
        &state,
        NewEvent {
            category: CATEGORY_AUTH,
            level: "info",
            event_type: "auth.logout".into(),
            outcome: "success",
            actor_user_id: Some(ctx.user_id),
            actor_name: Some(ctx.username),
            summary: "管理员退出登录".into(),
            ..Default::default()
        }
        .with_context(&event_ctx),
    )
    .await;
    let cookie = auth_cookie(&state, "", 0, false);
    let csrf = csrf_cookie(&state, "", 0);
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .append(SET_COOKIE, cookie.parse().unwrap());
    response
        .headers_mut()
        .append(SET_COOKIE, csrf.parse().unwrap());
    Ok(response)
}

async fn me(
    State(state): State<AppState>,
    Extension(ctx): Extension<AdminContext>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(json!({
        "username": ctx.username,
        "mfa_enabled": true,
        "csrf_required": true,
        "csrf_cookie": if state.cfg.cookie_secure { "__Host-sb_csrf" } else { "sb_csrf" },
        "expires_at": ctx.expires_at,
        "elevated_until": ctx.elevated_until
    })))
}

async fn step_up(
    State(state): State<AppState>,
    Extension(ctx): Extension<AdminContext>,
    Extension(event_ctx): Extension<EventContext>,
    Json(input): Json<StepUpInput>,
) -> ApiResult<impl IntoResponse> {
    let user = users::Entity::find_by_id(ctx.user_id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let secret = decrypt_secret(
        &state.cfg.mfa_encryption_key,
        user.mfa_secret_enc
            .as_deref()
            .ok_or_else(ApiError::unauthorized)?,
    )?;
    let Some(step) = verify_totp(&secret, &input.code, user.last_totp_step)
        .filter(|_| verify_password(&input.password, &user.password_hash))
    else {
        record(
            &state,
            NewEvent {
                category: CATEGORY_AUTH,
                level: "warn",
                event_type: "auth.step_up_failed".into(),
                outcome: "failure",
                summary: "高危操作二次验证失败".into(),
                detail: Some(json!({ "error_code": "invalid_credentials" })),
                ..Default::default()
            }
            .with_context(&event_ctx),
        )
        .await;
        return Err(ApiError::unauthorized());
    };
    let mut user_model: users::ActiveModel = user.clone().into();
    user_model.last_totp_step = Set(Some(step));
    user_model.update(&state.db()).await?;
    let until = Utc::now() + Duration::minutes(state.cfg.step_up_minutes);
    let row = sessions::Entity::find_by_id(&ctx.session_hash)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let mut model: sessions::ActiveModel = row.into();
    model.elevated_until = Set(Some(until.into()));
    model.update(&state.db()).await?;
    record(
        &state,
        NewEvent {
            category: CATEGORY_AUTH,
            level: "info",
            event_type: "auth.step_up".into(),
            outcome: "success",
            actor_user_id: Some(user.id),
            actor_name: Some(user.username),
            summary: "高危操作二次验证成功".into(),
            ..Default::default()
        }
        .with_context(&event_ctx),
    )
    .await;
    Ok(Json(json!({ "elevated_until": until })))
}

async fn change_password(
    State(state): State<AppState>,
    Extension(ctx): Extension<AdminContext>,
    Extension(event_ctx): Extension<EventContext>,
    Json(input): Json<ChangePasswordInput>,
) -> ApiResult<impl IntoResponse> {
    if let Err(error) = require_step_up(&ctx) {
        record(
            &state,
            NewEvent {
                category: CATEGORY_SECURITY,
                level: "warn",
                event_type: "auth.step_up_required".into(),
                outcome: "blocked",
                summary: "密码修改因缺少近期二次验证被拦截".into(),
                detail: Some(json!({ "error_code": error.code })),
                ..Default::default()
            }
            .with_context(&event_ctx),
        )
        .await;
        return Err(error);
    }
    if input.new_password.chars().count() < 12 || input.new_password.len() > 256 {
        return Err(ApiError::bad_request("new password must be 12-256 bytes"));
    }
    let user = users::Entity::find_by_id(ctx.user_id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    if !verify_password(&input.current_password, &user.password_hash) {
        record(
            &state,
            NewEvent {
                category: CATEGORY_SECURITY,
                level: "warn",
                event_type: "auth.password_change_failed".into(),
                outcome: "failure",
                summary: "管理员密码修改验证失败".into(),
                detail: Some(json!({ "error_code": "invalid_credentials" })),
                ..Default::default()
            }
            .with_context(&event_ctx),
        )
        .await;
        return Err(ApiError::unauthorized());
    }
    let mut model: users::ActiveModel = user.into();
    model.password_hash = Set(hash_password(&input.new_password).map_err(ApiError::from)?);
    model.password_changed_at = Set(Some(Utc::now().into()));
    model.update(&state.db()).await?;
    sessions::Entity::delete_many()
        .filter(sessions::Column::UserId.eq(ctx.user_id))
        .filter(sessions::Column::Id.ne(ctx.session_hash))
        .exec(&state.db())
        .await?;
    record(
        &state,
        NewEvent {
            category: CATEGORY_SECURITY,
            level: "warn",
            event_type: "auth.password_changed".into(),
            outcome: "success",
            summary: "管理员密码已修改，其他会话已撤销".into(),
            ..Default::default()
        }
        .with_context(&event_ctx),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn regenerate_recovery_codes(
    State(state): State<AppState>,
    Extension(ctx): Extension<AdminContext>,
    Extension(event_ctx): Extension<EventContext>,
) -> ApiResult<impl IntoResponse> {
    if let Err(error) = require_step_up(&ctx) {
        record(
            &state,
            NewEvent {
                category: CATEGORY_SECURITY,
                level: "warn",
                event_type: "auth.step_up_required".into(),
                outcome: "blocked",
                summary: "恢复码重生成因缺少近期二次验证被拦截".into(),
                detail: Some(json!({ "error_code": error.code })),
                ..Default::default()
            }
            .with_context(&event_ctx),
        )
        .await;
        return Err(error);
    }
    let now = Utc::now();
    mfa_recovery_codes::Entity::delete_many()
        .filter(mfa_recovery_codes::Column::UserId.eq(ctx.user_id))
        .exec(&state.db())
        .await?;
    let codes = generate_recovery_codes();
    for code in &codes {
        mfa_recovery_codes::ActiveModel {
            user_id: Set(ctx.user_id),
            code_hash: Set(recovery_hash(&state.cfg.mfa_encryption_key, code)),
            used_at: Set(None),
            created_at: Set(now.into()),
            ..Default::default()
        }
        .insert(&state.db())
        .await?;
    }
    record(
        &state,
        NewEvent {
            category: CATEGORY_SECURITY,
            level: "warn",
            event_type: "auth.recovery_codes_regenerated".into(),
            outcome: "success",
            actor_user_id: Some(ctx.user_id),
            actor_name: Some(ctx.username),
            summary: "管理员重新生成了 MFA 恢复码".into(),
            ..Default::default()
        }
        .with_context(&event_ctx),
    )
    .await;
    Ok(Json(json!({ "recovery_codes": codes })))
}
