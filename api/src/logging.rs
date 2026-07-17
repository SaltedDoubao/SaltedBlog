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

fn clean(input: String, max: usize) -> String {
    input.replace(['\r', '\n'], " ").chars().take(max).collect()
}

fn into_model(event: NewEvent) -> event_logs::ActiveModel {
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

pub async fn record(state: &AppState, event: NewEvent) {
    if is_priority(&event) {
        if let Err(err) = into_model(event).insert(&state.db()).await {
            tracing::error!(error = %err, "priority event log insert failed");
        }
        return;
    }
    let fallback = format!("{}: {}", event.event_type, event.summary);
    if let Err(err) = state.log_tx.try_send(event) {
        tracing::error!(error = %err, event = %clean(fallback, 600), "event log queue unavailable");
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
            if let Err(err) = event_logs::Entity::insert_many(models)
                .exec(&state.db())
                .await
            {
                tracing::error!(error = %err, "batched event log insert failed");
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
    let ctx = request.extensions().get::<AdminContext>().cloned();
    let request_id = request.extensions().get::<RequestId>().map(|v| v.0.clone());
    let addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|v| &v.0);
    let ip = client_ip(request.headers(), addr, &state.cfg).to_string();
    let response = next.run(request).await;
    let status = response.status();
    if method != Method::GET || status.is_client_error() || status.is_server_error() {
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
            ..Default::default()
        };
        record(&state, event).await;
    }
    response
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
                (access, normal, old, pv) => {
                    tracing::error!(
                        ?access,
                        ?normal,
                        ?old,
                        ?pv,
                        "event log retention cleanup failed"
                    );
                }
            }
        }
    });
}
