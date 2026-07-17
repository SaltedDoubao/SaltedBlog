use std::net::{IpAddr, SocketAddr};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::password_hash::{
    rand_core::OsRng, rand_core::RngCore, PasswordHash, PasswordHasher, PasswordVerifier,
    SaltString,
};
use argon2::Argon2;
use axum::extract::{Request, State};
use axum::http::header::{COOKIE, ORIGIN};
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::Response;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::{Duration, Utc};
use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::entities::{sessions, users};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub const SESSION_COOKIE_DEV: &str = "sb_session";
pub const SESSION_COOKIE_PROD: &str = "__Host-sb_session";
pub const PREAUTH_COOKIE_DEV: &str = "sb_preauth";
pub const PREAUTH_COOKIE_PROD: &str = "__Host-sb_preauth";
pub const CSRF_COOKIE_DEV: &str = "sb_csrf";
pub const CSRF_COOKIE_PROD: &str = "__Host-sb_csrf";

#[derive(Clone, Debug)]
pub struct AdminContext {
    pub user_id: i32,
    pub username: String,
    pub session_hash: String,
    pub csrf_token: String,
    pub expires_at: chrono::DateTime<Utc>,
    pub elevated_until: Option<chrono::DateTime<Utc>>,
}

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

pub fn random_token(bytes: usize) -> String {
    let mut raw = vec![0u8; bytes];
    OsRng.fill_bytes(&mut raw);
    URL_SAFE_NO_PAD.encode(raw)
}

pub fn sha256_hex(value: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(value.as_ref()))
}

fn secret_key(configured: &str) -> [u8; 32] {
    let source = if configured.is_empty() {
        "saltedblog-development-key"
    } else {
        configured
    };
    Sha256::digest(source.as_bytes()).into()
}

pub fn encrypt_secret(configured_key: &str, plaintext: &[u8]) -> ApiResult<String> {
    let cipher = Aes256Gcm::new_from_slice(&secret_key(configured_key))
        .map_err(|_| ApiError::internal("invalid MFA encryption key"))?;
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| ApiError::internal("MFA encryption failed"))?;
    let mut packed = nonce.to_vec();
    packed.extend(encrypted);
    Ok(STANDARD.encode(packed))
}

pub fn decrypt_secret(configured_key: &str, encoded: &str) -> ApiResult<Vec<u8>> {
    let packed = STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::internal("invalid MFA secret"))?;
    if packed.len() < 13 {
        return Err(ApiError::internal("invalid MFA secret"));
    }
    let cipher = Aes256Gcm::new_from_slice(&secret_key(configured_key))
        .map_err(|_| ApiError::internal("invalid MFA encryption key"))?;
    cipher
        .decrypt(Nonce::from_slice(&packed[..12]), &packed[12..])
        .map_err(|_| ApiError::internal("MFA secret decryption failed"))
}

pub fn generate_totp_secret() -> Vec<u8> {
    let mut secret = vec![0u8; 20];
    OsRng.fill_bytes(&mut secret);
    secret
}

fn totp_code(secret: &[u8], step: i64) -> String {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(secret).expect("HMAC accepts key");
    mac.update(&(step as u64).to_be_bytes());
    let bytes = mac.finalize().into_bytes();
    let offset = (bytes[19] & 0x0f) as usize;
    let value = ((u32::from(bytes[offset]) & 0x7f) << 24)
        | (u32::from(bytes[offset + 1]) << 16)
        | (u32::from(bytes[offset + 2]) << 8)
        | u32::from(bytes[offset + 3]);
    format!("{:06}", value % 1_000_000)
}

pub fn verify_totp(secret: &[u8], code: &str, last_step: Option<i64>) -> Option<i64> {
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let now_step = Utc::now().timestamp() / 30;
    for step in [now_step, now_step - 1, now_step + 1] {
        if last_step.is_some_and(|last| step <= last) {
            continue;
        }
        if totp_code(secret, step).as_bytes() == code.as_bytes() {
            return Some(step);
        }
    }
    None
}

pub fn totp_setup(secret: &[u8], username: &str) -> (String, String) {
    let encoded = BASE32_NOPAD.encode(secret);
    let account: String = url::form_urlencoded::byte_serialize(username.as_bytes()).collect();
    let uri = format!("otpauth://totp/SaltedBlog:{account}?secret={encoded}&issuer=SaltedBlog&algorithm=SHA1&digits=6&period=30");
    (encoded, uri)
}

pub fn recovery_hash(configured_key: &str, code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret_key(configured_key));
    hasher.update(code.trim().to_ascii_uppercase().replace('-', "").as_bytes());
    hex::encode(hasher.finalize())
}

pub fn generate_recovery_codes() -> Vec<String> {
    (0..10)
        .map(|_| {
            let raw = random_token(8).replace(['-', '_'], "").to_ascii_uppercase();
            let short: String = raw.chars().take(10).collect();
            format!("{}-{}", &short[..5], &short[5..])
        })
        .collect()
}

fn cookie_name(state: &AppState, preauth: bool) -> &'static str {
    match (state.cfg.cookie_secure, preauth) {
        (true, true) => PREAUTH_COOKIE_PROD,
        (true, false) => SESSION_COOKIE_PROD,
        (false, true) => PREAUTH_COOKIE_DEV,
        (false, false) => SESSION_COOKIE_DEV,
    }
}

pub fn auth_cookie(state: &AppState, value: &str, max_age_secs: i64, preauth: bool) -> String {
    let secure = if state.cfg.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}{secure}",
        cookie_name(state, preauth)
    )
}

pub fn csrf_cookie(state: &AppState, value: &str, max_age_secs: i64) -> String {
    let secure = if state.cfg.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    let name = if state.cfg.cookie_secure {
        CSRF_COOKIE_PROD
    } else {
        CSRF_COOKIE_DEV
    };
    format!("{name}={value}; Path=/; SameSite=Strict; Max-Age={max_age_secs}{secure}")
}

pub fn read_cookie(headers: &HeaderMap, state: &AppState, preauth: bool) -> Option<String> {
    let name = cookie_name(state, preauth);
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    raw.split(';').map(str::trim).find_map(|pair| {
        pair.strip_prefix(&format!("{name}="))
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    })
}

pub async fn create_session(
    state: &AppState,
    user_id: i32,
    ip: Option<String>,
    user_agent: Option<&str>,
) -> ApiResult<(String, String)> {
    let token = random_token(32);
    let csrf = random_token(32);
    let now = Utc::now();
    sessions::ActiveModel {
        id: Set(sha256_hex(&token)),
        user_id: Set(user_id),
        expires_at: Set((now + Duration::hours(state.cfg.session_absolute_hours)).into()),
        created_at: Set(now.into()),
        csrf_hash: Set(sha256_hex(&csrf)),
        last_seen_at: Set(Some(now.into())),
        elevated_until: Set(None),
        ip: Set(ip),
        user_agent_hash: Set(user_agent.map(sha256_hex)),
    }
    .insert(&state.db())
    .await?;
    let _ = sessions::Entity::delete_many()
        .filter(sessions::Column::ExpiresAt.lt(now))
        .exec(&state.db())
        .await;
    Ok((token, csrf))
}

pub fn client_ip(headers: &HeaderMap, addr: Option<&SocketAddr>, cfg: &Config) -> IpAddr {
    let peer = addr
        .map(SocketAddr::ip)
        .unwrap_or_else(|| IpAddr::from([127, 0, 0, 1]));
    let trusted = cfg
        .trusted_proxy_cidrs
        .iter()
        .any(|net| net.contains(&peer));
    if trusted {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            let entries: Vec<_> = xff
                .split(',')
                .filter_map(|v| v.trim().parse::<IpAddr>().ok())
                .collect();
            for ip in entries.into_iter().rev() {
                if !cfg.trusted_proxy_cidrs.iter().any(|net| net.contains(&ip)) {
                    return ip;
                }
            }
        }
    }
    peer
}

pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Option<AdminContext> {
    let token = read_cookie(headers, state, false)?;
    let token_hash = sha256_hex(token);
    let row = sessions::Entity::find_by_id(token_hash.clone())
        .one(&state.db())
        .await
        .ok()??;
    let now = Utc::now();
    if row.expires_at < now {
        return None;
    }
    let last_seen = row
        .last_seen_at
        .unwrap_or(row.created_at)
        .with_timezone(&Utc);
    if now - last_seen > Duration::minutes(state.cfg.session_idle_minutes) {
        return None;
    }
    let user = users::Entity::find_by_id(row.user_id)
        .one(&state.db())
        .await
        .ok()??;
    if now - last_seen > Duration::seconds(60) {
        let mut model: sessions::ActiveModel = row.clone().into();
        model.last_seen_at = Set(Some(now.into()));
        let _ = model.update(&state.db()).await;
    }
    Some(AdminContext {
        user_id: user.id,
        username: user.username,
        session_hash: token_hash,
        csrf_token: String::new(),
        expires_at: row.expires_at.with_timezone(&Utc),
        elevated_until: row.elevated_until.map(|v| v.with_timezone(&Utc)),
    })
}

fn validate_unsafe_request(
    state: &AppState,
    headers: &HeaderMap,
    ctx: &mut AdminContext,
) -> ApiResult<()> {
    let origin = headers
        .get(ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if origin != state.cfg.admin_origin {
        return Err(ApiError::forbidden(
            "origin_rejected",
            "request origin rejected",
        ));
    }
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if site != "same-origin" {
            return Err(ApiError::forbidden(
                "cross_site_rejected",
                "cross-site request rejected",
            ));
        }
    }
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if csrf.is_empty() {
        return Err(ApiError::forbidden("csrf_required", "CSRF token required"));
    }
    ctx.csrf_token = csrf.to_string();
    Ok(())
}

pub async fn require_auth_origin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        let origin = request
            .headers()
            .get(ORIGIN)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let same_site = request
            .headers()
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_none_or(|v| v == "same-origin");
        if origin != state.cfg.admin_origin || !same_site {
            crate::logging::record(
                &state,
                crate::logging::NewEvent {
                    category: crate::logging::CATEGORY_SECURITY,
                    level: "warn",
                    event_type: "security.auth_origin_rejected".into(),
                    outcome: "blocked",
                    summary: "认证请求 Origin 或 Fetch Metadata 校验失败".into(),
                    ..Default::default()
                },
            )
            .await;
            return Err(ApiError::forbidden(
                "origin_rejected",
                "request origin rejected",
            ));
        }
    }
    Ok(next.run(request).await)
}

pub async fn require_admin(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|v| &v.0);
    let ip = client_ip(request.headers(), addr, &state.cfg).to_string();
    let Some(mut ctx) = authenticate(&state, request.headers()).await else {
        crate::logging::record(
            &state,
            crate::logging::NewEvent {
                category: crate::logging::CATEGORY_SECURITY,
                level: "warn",
                event_type: "auth.session_rejected".into(),
                outcome: "blocked",
                source_ip: Some(ip),
                summary: "无效或过期的管理会话".into(),
                ..Default::default()
            },
        )
        .await;
        return Err(ApiError::unauthorized());
    };
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        if let Err(err) = validate_unsafe_request(&state, request.headers(), &mut ctx) {
            crate::logging::record(
                &state,
                crate::logging::NewEvent {
                    category: crate::logging::CATEGORY_SECURITY,
                    level: "warn",
                    event_type: "security.request_origin_or_csrf".into(),
                    outcome: "blocked",
                    actor_user_id: Some(ctx.user_id),
                    actor_name: Some(ctx.username.clone()),
                    source_ip: Some(ip.clone()),
                    summary: err.message.clone(),
                    ..Default::default()
                },
            )
            .await;
            return Err(err);
        }
        let row = sessions::Entity::find_by_id(ctx.session_hash.clone())
            .one(&state.db())
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        if sha256_hex(&ctx.csrf_token) != row.csrf_hash {
            crate::logging::record(
                &state,
                crate::logging::NewEvent {
                    category: crate::logging::CATEGORY_SECURITY,
                    level: "warn",
                    event_type: "security.csrf_invalid".into(),
                    outcome: "blocked",
                    actor_user_id: Some(ctx.user_id),
                    actor_name: Some(ctx.username.clone()),
                    source_ip: Some(ip),
                    summary: "CSRF token 校验失败".into(),
                    ..Default::default()
                },
            )
            .await;
            return Err(ApiError::forbidden("csrf_invalid", "invalid CSRF token"));
        }
    }
    request.extensions_mut().insert(ctx);
    Ok(next.run(request).await)
}

pub fn require_step_up(ctx: &AdminContext) -> ApiResult<()> {
    if ctx.elevated_until.is_some_and(|until| until > Utc::now()) {
        return Ok(());
    }
    Err(ApiError::forbidden(
        "step_up_required",
        "recent password and TOTP verification required",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_roundtrip() {
        let encrypted = encrypt_secret("test-key", b"secret").unwrap();
        assert_eq!(decrypt_secret("test-key", &encrypted).unwrap(), b"secret");
    }

    #[test]
    fn rfc_totp_vector() {
        let secret = b"12345678901234567890";
        assert_eq!(&totp_code(secret, 59 / 30)[..], "287082");
    }

    #[test]
    fn recovery_normalization() {
        assert_eq!(
            recovery_hash("k", "ABCDE-12345"),
            recovery_hash("k", "abcde12345")
        );
    }
}
