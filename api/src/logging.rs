use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, Method};
use axum::middleware::Next;
use axum::response::Response;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::Value;
use uuid::Uuid;

use crate::auth::{client_ip, AdminContext};
use crate::entities::{event_logs, page_views};
use crate::error::SafeErrorCode;
use crate::state::AppState;

pub const CATEGORY_AUTH: &str = "auth";
pub const CATEGORY_SECURITY: &str = "security";
pub const CATEGORY_AUDIT: &str = "audit";
pub const CATEGORY_ACCESS: &str = "access";
pub const CATEGORY_SYSTEM: &str = "system";
pub const CATEGORY_JOB: &str = "job";
pub const CATEGORY_BACKUP: &str = "backup";

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

#[derive(Clone, Debug, Default)]
pub struct EventContext {
    pub actor_user_id: Option<i32>,
    pub actor_name: Option<String>,
    pub source_ip: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Default, Debug)]
pub struct NewEvent {
    pub category: &'static str,
    pub level: &'static str,
    pub event_type: String,
    pub outcome: &'static str,
    pub actor_user_id: Option<i32>,
    pub actor_name: Option<String>,
    pub source_ip: Option<String>,
    pub request_id: Option<String>,
    pub method: Option<String>,
    pub route: Option<String>,
    pub status_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub summary: String,
    pub detail: Option<Value>,
}

impl NewEvent {
    pub fn with_context(mut self, context: &EventContext) -> Self {
        self.actor_user_id = context.actor_user_id;
        self.actor_name.clone_from(&context.actor_name);
        self.source_ip.clone_from(&context.source_ip);
        self.request_id.clone_from(&context.request_id);
        self
    }
}

static LOG_WRITE_FAILURES: AtomicU64 = AtomicU64::new(0);
static LOG_QUEUE_DROPS: AtomicU64 = AtomicU64::new(0);

fn clean(input: String, max: usize) -> String {
    input.replace(['\r', '\n'], " ").chars().take(max).collect()
}

fn sensitive_detail_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "csrf",
        "password",
        "totp",
        "recovery_code",
        "database_url",
        "api_key",
        "secret",
        "prompt",
        "raw_output",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

fn sanitize_detail(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if sensitive_detail_key(key) {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    sanitize_detail(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(sanitize_detail),
        _ => {}
    }
}

fn into_model(mut event: NewEvent) -> event_logs::ActiveModel {
    if let Some(detail) = event.detail.as_mut() {
        sanitize_detail(detail);
    }
    event_logs::ActiveModel {
        occurred_at: Set(Utc::now().into()),
        category: Set(event.category.to_string()),
        level: Set(event.level.to_string()),
        event_type: Set(clean(event.event_type, 96)),
        outcome: Set(event.outcome.to_string()),
        actor_user_id: Set(event.actor_user_id),
        actor_name: Set(event.actor_name.map(|v| clean(v, 64))),
        source_ip: Set(event.source_ip.map(|v| clean(v, 64))),
        request_id: Set(event.request_id.map(|v| clean(v, 64))),
        method: Set(event.method.map(|v| clean(v, 12))),
        route: Set(event.route.map(|v| clean(v, 512))),
        status_code: Set(event.status_code),
        duration_ms: Set(event.duration_ms),
        resource_type: Set(event.resource_type.map(|v| clean(v, 64))),
        resource_id: Set(event.resource_id.map(|v| clean(v, 128))),
        summary: Set(clean(event.summary, 500)),
        detail_json: Set(event
            .detail
            .and_then(|v| serde_json::to_string(&v).ok())
            .map(|v| clean(v, 8000))),
        ..Default::default()
    }
}

fn is_priority(event: &NewEvent) -> bool {
    matches!(
        event.category,
        CATEGORY_AUTH | CATEGORY_SECURITY | CATEGORY_AUDIT | CATEGORY_BACKUP
    ) || matches!(event.level, "error" | "critical")
}

async fn record_recovery(state: &AppState) {
    let write_failures = LOG_WRITE_FAILURES.swap(0, Ordering::AcqRel);
    let queue_drops = LOG_QUEUE_DROPS.swap(0, Ordering::AcqRel);
    if write_failures == 0 && queue_drops == 0 {
        return;
    }
    let event = NewEvent {
        category: CATEGORY_SYSTEM,
        level: "warn",
        event_type: "logging.recovered".into(),
        outcome: "success",
        summary: "日志写入通道已恢复".into(),
        detail: Some(serde_json::json!({
            "write_failures": write_failures,
            "queue_drops": queue_drops,
        })),
        ..Default::default()
    };
    if into_model(event).insert(&state.db()).await.is_err() {
        LOG_WRITE_FAILURES.fetch_add(write_failures + 1, Ordering::Relaxed);
        LOG_QUEUE_DROPS.fetch_add(queue_drops, Ordering::Relaxed);
        tracing::error!(
            error_code = "database_error",
            "logging recovery event insert failed"
        );
    }
}

pub async fn record(state: &AppState, event: NewEvent) {
    if is_priority(&event) {
        if into_model(event).insert(&state.db()).await.is_err() {
            LOG_WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
            tracing::error!(
                error_code = "database_error",
                "priority event log insert failed"
            );
        } else {
            record_recovery(state).await;
        }
        return;
    }
    let event_type = clean(event.event_type.clone(), 96);
    if state.log_tx.try_send(event).is_err() {
        LOG_QUEUE_DROPS.fetch_add(1, Ordering::Relaxed);
        tracing::error!(
            event_type,
            error_code = "queue_unavailable",
            "event log queue unavailable"
        );
    }
}

pub async fn spawn_writer(state: AppState) {
    let Some(mut receiver) = state.take_log_receiver().await else {
        return;
    };
    tokio::spawn(async move {
        while let Some(first) = receiver.recv().await {
            let mut batch = Vec::with_capacity(100);
            batch.push(first);
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
            while batch.len() < 100 {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, receiver.recv()).await {
                    Ok(Some(event)) => batch.push(event),
                    _ => break,
                }
            }
            let models = batch.into_iter().map(into_model).collect::<Vec<_>>();
            if event_logs::Entity::insert_many(models)
                .exec(&state.db())
                .await
                .is_err()
            {
                LOG_WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
                tracing::error!(
                    error_code = "database_error",
                    "batched event log insert failed"
                );
            } else {
                record_recovery(&state).await;
            }
        }
    });
}

pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let id = Uuid::new_v4().to_string();
    request.extensions_mut().insert(RequestId(id.clone()));
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

pub async fn admin_request_log(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|v| v.as_str().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    let path = request.uri().path().to_string();
    let ctx = request.extensions().get::<AdminContext>().cloned();
    let event_context = request.extensions().get::<EventContext>().cloned();
    let request_id = request.extensions().get::<RequestId>().map(|v| v.0.clone());
    let addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|v| &v.0);
    let ip = client_ip(request.headers(), addr, &state.cfg).to_string();
    let response = next.run(request).await;
    let status = response.status();
    let safe_error_code = response
        .extensions()
        .get::<SafeErrorCode>()
        .map(|value| value.0);
    let sensitive_get = method == Method::GET
        && (route.ends_with("/logs/export") || route.ends_with("/backups/{name}/download"));
    if method != Method::GET
        || status.is_client_error()
        || status.is_server_error()
        || sensitive_get
    {
        let category = if method == Method::GET {
            CATEGORY_ACCESS
        } else {
            CATEGORY_AUDIT
        };
        let outcome = if status.is_success() {
            "success"
        } else if status.is_client_error() {
            "blocked"
        } else {
            "failure"
        };
        let event = NewEvent {
            category,
            level: if status.is_server_error() {
                "error"
            } else if status.is_client_error() {
                "warn"
            } else {
                "info"
            },
            event_type: format!("admin.{}", method.as_str().to_ascii_lowercase()),
            outcome,
            actor_user_id: ctx.as_ref().map(|v| v.user_id),
            actor_name: ctx.as_ref().map(|v| v.username.clone()),
            source_ip: Some(ip),
            request_id,
            method: Some(method.to_string()),
            route: Some(route.clone()),
            status_code: Some(status.as_u16() as i32),
            duration_ms: Some(started.elapsed().as_millis() as i64),
            summary: format!("{} {} -> {}", method, route, status.as_u16()),
            detail: safe_error_code.map(|code| serde_json::json!({ "error_code": code })),
            ..Default::default()
        };
        record(&state, event).await;
    }
    if let Some((event_type, resource_type)) = semantic_admin_event(&method, &route) {
        let outcome = if status.is_success() {
            "success"
        } else if status.is_client_error() {
            "blocked"
        } else {
            "failure"
        };
        let mut detail = serde_json::Map::new();
        if let Some(code) = safe_error_code {
            detail.insert("error_code".into(), serde_json::json!(code));
        }
        let event = NewEvent {
            category: if event_type.starts_with("backup.") {
                CATEGORY_BACKUP
            } else {
                CATEGORY_AUDIT
            },
            level: if status.is_server_error() {
                "error"
            } else if status.is_client_error() {
                "warn"
            } else {
                "info"
            },
            event_type: event_type.into(),
            outcome,
            method: Some(method.to_string()),
            route: Some(route.clone()),
            status_code: Some(status.as_u16() as i32),
            duration_ms: Some(started.elapsed().as_millis() as i64),
            resource_type: Some(resource_type.into()),
            resource_id: resource_id_from_path(&route, &path),
            summary: format!(
                "后台操作 {}",
                if status.is_success() {
                    "成功"
                } else {
                    "未完成"
                }
            ),
            detail: (!detail.is_empty()).then_some(serde_json::Value::Object(detail)),
            ..Default::default()
        };
        let event = if let Some(context) = event_context.as_ref() {
            event.with_context(context)
        } else {
            event
        };
        record(&state, event).await;
    }
    response
}

fn semantic_admin_event(method: &Method, route: &str) -> Option<(&'static str, &'static str)> {
    let route = route.strip_prefix("/api/admin").unwrap_or(route);
    match (method, route) {
        (&Method::POST, "/posts") => Some(("content.post.create", "post")),
        (&Method::PUT, "/posts/{id}") => Some(("content.post.update", "post")),
        (&Method::DELETE, "/posts/{id}") => Some(("content.post.delete", "post")),
        (&Method::POST, "/categories") => Some(("content.category.create", "category")),
        (&Method::PUT, "/categories/{id}") => Some(("content.category.update", "category")),
        (&Method::DELETE, "/categories/{id}") => Some(("content.category.delete", "category")),
        (&Method::POST, "/tags") => Some(("content.tag.create", "tag")),
        (&Method::PUT, "/tags/{id}") => Some(("content.tag.update", "tag")),
        (&Method::DELETE, "/tags/{id}") => Some(("content.tag.delete", "tag")),
        (&Method::POST, "/series") => Some(("content.series.create", "series")),
        (&Method::PUT, "/series/{id}") => Some(("content.series.update", "series")),
        (&Method::DELETE, "/series/{id}") => Some(("content.series.delete", "series")),
        (&Method::POST, "/friends") => Some(("content.friend.create", "friend")),
        (&Method::PUT, "/friends/{id}") => Some(("content.friend.update", "friend")),
        (&Method::DELETE, "/friends/{id}") => Some(("content.friend.delete", "friend")),
        (&Method::POST, "/uploads") => Some(("media.upload.create", "upload")),
        (&Method::DELETE, "/uploads/{id}") => Some(("media.upload.delete", "upload")),
        (&Method::POST, "/site-icons") => Some(("media.site_icon.upload", "site_icon")),
        (&Method::PUT, "/site-icons/{id}/activate") => {
            Some(("media.site_icon.activate", "site_icon"))
        }
        (&Method::DELETE, "/site-icons/{id}") => Some(("media.site_icon.delete", "site_icon")),
        (&Method::POST, "/news/sources") => Some(("news.source.create", "news_source")),
        (&Method::PUT, "/news/sources/{id}") => Some(("news.source.update", "news_source")),
        (&Method::DELETE, "/news/sources/{id}") => Some(("news.source.delete", "news_source")),
        (&Method::POST, "/news/sources/{id}/test") => Some(("news.source.test", "news_source")),
        (&Method::POST, "/news/sources/{id}/fetch") => Some(("news.source.fetch", "news_source")),
        (&Method::POST, "/news/fetch-all") => Some(("news.fetch.run", "news_fetch")),
        (&Method::POST, "/news/tasks") => Some(("news.task.create", "news_task")),
        (&Method::PUT, "/news/tasks/{id}") => Some(("news.task.update", "news_task")),
        (&Method::DELETE, "/news/tasks/{id}") => Some(("news.task.delete", "news_task")),
        (&Method::PUT, "/news/tasks/{id}/toggle") => Some(("news.task.toggle", "news_task")),
        (&Method::POST, "/news/tasks/{id}/run") => Some(("news.task.run_accepted", "news_task")),
        (&Method::POST, "/backups") => Some(("backup.job_accepted", "backup_job")),
        (&Method::POST, "/backups/upload") => Some(("backup.upload", "backup")),
        (&Method::POST, "/backups/{name}/restore") => Some(("backup.restore_accepted", "backup")),
        (&Method::DELETE, "/backups/{name}") => Some(("backup.delete", "backup")),
        (&Method::GET, "/backups/{name}/download") => Some(("backup.download", "backup")),
        (&Method::GET, "/logs/export") => Some(("logs.export", "event_log")),
        _ => None,
    }
}

fn resource_id_from_path(route: &str, path: &str) -> Option<String> {
    if !route.contains("{id}") && !route.contains("{name}") {
        return None;
    }
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    let suffix = route.rsplit('/').next().unwrap_or_default();
    let index = if suffix.starts_with('{') {
        parts.len().checked_sub(1)?
    } else {
        parts.len().checked_sub(2)?
    };
    parts
        .get(index)
        .map(|value| clean((*value).to_string(), 128))
}

pub async fn public_request_log(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let method = request.method().clone();
    let request_id = request.extensions().get::<RequestId>().map(|v| v.0.clone());
    let addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|v| &v.0);
    let ip = client_ip(request.headers(), addr, &state.cfg).to_string();
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status();
    let sampled = request_id
        .as_deref()
        .is_some_and(|id| id.as_bytes().first().is_some_and(|b| b % 100 == 0));
    if !path.starts_with("/api/admin")
        && !path.starts_with("/api/auth")
        && (status.is_server_error() || status.is_client_error() || sampled)
    {
        let event = NewEvent {
            category: if status.is_server_error() {
                CATEGORY_SYSTEM
            } else {
                CATEGORY_ACCESS
            },
            level: if status.is_server_error() {
                "error"
            } else if status.is_client_error() {
                "warn"
            } else {
                "info"
            },
            event_type: if status.is_server_error() {
                "http.server_error".into()
            } else {
                "http.public_request".into()
            },
            outcome: if status.is_success() {
                "success"
            } else {
                "failure"
            },
            source_ip: Some(ip),
            request_id,
            method: Some(method.to_string()),
            route: Some(path.clone()),
            status_code: Some(status.as_u16() as i32),
            duration_ms: Some(started.elapsed().as_millis() as i64),
            summary: format!("{} {} -> {}", method, path, status.as_u16()),
            ..Default::default()
        };
        record(&state, event).await;
    }
    response
}

pub fn spawn_cleanup(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
        loop {
            ticker.tick().await;
            let now = Utc::now();
            let access_cutoff = now - Duration::days(30);
            let normal_cutoff = now - Duration::days(90);
            let security_cutoff = now - Duration::days(180);
            let pv_cutoff = (now - Duration::days(365)).format("%Y-%m-%d").to_string();
            let db = state.db();
            let access = event_logs::Entity::delete_many()
                .filter(event_logs::Column::Category.eq(CATEGORY_ACCESS))
                .filter(event_logs::Column::OccurredAt.lt(access_cutoff))
                .exec(&db)
                .await;
            let normal = event_logs::Entity::delete_many()
                .filter(event_logs::Column::Category.is_in([
                    CATEGORY_SYSTEM,
                    CATEGORY_JOB,
                    CATEGORY_BACKUP,
                ]))
                .filter(event_logs::Column::OccurredAt.lt(normal_cutoff))
                .exec(&db)
                .await;
            let old = event_logs::Entity::delete_many()
                .filter(event_logs::Column::OccurredAt.lt(security_cutoff))
                .exec(&db)
                .await;
            let pv = page_views::Entity::delete_many()
                .filter(page_views::Column::Date.lt(pv_cutoff))
                .exec(&db)
                .await;
            match (access, normal, old, pv) {
                (Ok(access), Ok(normal), Ok(old), Ok(pv)) => {
                    let removed = access.rows_affected
                        + normal.rows_affected
                        + old.rows_affected
                        + pv.rows_affected;
                    record(
                        &state,
                        NewEvent {
                            category: CATEGORY_AUDIT,
                            level: "info",
                            event_type: "logs.retention_cleanup".into(),
                            outcome: "success",
                            summary: format!("日志保留策略清理完成，共删除 {removed} 条"),
                            detail: Some(serde_json::json!({ "removed": removed })),
                            ..Default::default()
                        },
                    )
                    .await;
                }
                _ => {
                    tracing::error!(
                        error_code = "database_error",
                        "event log retention cleanup failed"
                    );
                    record(
                        &state,
                        NewEvent {
                            category: CATEGORY_AUDIT,
                            level: "error",
                            event_type: "logs.retention_cleanup".into(),
                            outcome: "failure",
                            summary: "日志保留策略清理失败".into(),
                            detail: Some(serde_json::json!({ "error_code": "database_error" })),
                            ..Default::default()
                        },
                    )
                    .await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_fields_are_cleaned_and_sensitive_detail_is_redacted() {
        let model = into_model(NewEvent {
            category: CATEGORY_AUDIT,
            level: "info",
            event_type: "settings.update\r\ninjected".into(),
            outcome: "success",
            summary: "safe\nsummary".into(),
            detail: Some(serde_json::json!({
                "changed_keys": ["site_title_zh"],
                "database_url": "postgres://sentinel",
                "nested": { "llm_api_key": "sentinel-key", "count": 2 },
            })),
            ..Default::default()
        });
        assert_eq!(model.event_type.unwrap(), "settings.update  injected");
        assert_eq!(model.summary.unwrap(), "safe summary");
        let detail = model.detail_json.unwrap().unwrap();
        assert!(!detail.contains("postgres://sentinel"));
        assert!(!detail.contains("sentinel-key"));
        assert!(detail.contains("[REDACTED]"));
    }

    #[test]
    fn semantic_routes_cover_sensitive_reads_and_mutations() {
        assert_eq!(
            semantic_admin_event(&Method::PUT, "/api/admin/posts/{id}"),
            Some(("content.post.update", "post"))
        );
        assert_eq!(
            semantic_admin_event(&Method::GET, "/api/admin/logs/export"),
            Some(("logs.export", "event_log"))
        );
        assert_eq!(
            resource_id_from_path(
                "/api/admin/backups/{name}/download",
                "/api/admin/backups/saltedblog_sqlite_20260721_000000.zip/download",
            )
            .as_deref(),
            Some("saltedblog_sqlite_20260721_000000.zip")
        );
    }
}
