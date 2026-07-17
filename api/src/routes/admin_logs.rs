use std::collections::{BTreeMap, HashMap};

use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::{routing::get, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use futures_util::future::join_all;
use futures_util::StreamExt;
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{require_step_up, AdminContext};
use crate::entities::event_logs;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/logs/summary", get(summary))
        .route("/logs/export", get(export_logs))
        .route("/logs/{id}", get(detail))
        .route("/logs", get(list))
}

#[derive(Clone, Deserialize)]
struct LogQuery {
    cursor: Option<String>,
    limit: Option<u64>,
    category: Option<String>,
    level: Option<String>,
    outcome: Option<String>,
    q: Option<String>,
    from: Option<String>,
    to: Option<String>,
    format: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CursorValue {
    at: String,
    id: i64,
}

fn decode_cursor(raw: &str) -> ApiResult<(DateTime<Utc>, i64)> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| ApiError::bad_request("invalid cursor"))?;
    let value: CursorValue =
        serde_json::from_slice(&bytes).map_err(|_| ApiError::bad_request("invalid cursor"))?;
    let at = DateTime::parse_from_rfc3339(&value.at)
        .map_err(|_| ApiError::bad_request("invalid cursor"))?
        .with_timezone(&Utc);
    Ok((at, value.id))
}

fn encode_cursor(row: &event_logs::Model) -> String {
    URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&CursorValue {
            at: row.occurred_at.to_rfc3339(),
            id: row.id,
        })
        .unwrap_or_default(),
    )
}

fn filtered(q: &LogQuery) -> ApiResult<sea_orm::Select<event_logs::Entity>> {
    let mut cond = Condition::all();
    if let Some(v) = q.category.as_deref().filter(|v| !v.is_empty()) {
        cond = cond.add(event_logs::Column::Category.eq(v));
    }
    if let Some(v) = q.level.as_deref().filter(|v| !v.is_empty()) {
        cond = cond.add(event_logs::Column::Level.eq(v));
    }
    if let Some(v) = q.outcome.as_deref().filter(|v| !v.is_empty()) {
        cond = cond.add(event_logs::Column::Outcome.eq(v));
    }
    if let Some(v) = q.q.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        if v.len() > 100 {
            return Err(ApiError::bad_request("query too long"));
        }
        cond = cond.add(
            Condition::any()
                .add(event_logs::Column::Summary.contains(v))
                .add(event_logs::Column::EventType.contains(v))
                .add(event_logs::Column::RequestId.eq(v))
                .add(event_logs::Column::SourceIp.eq(v)),
        );
    }
    if let Some(v) = &q.from {
        let at = DateTime::parse_from_rfc3339(v)
            .map_err(|_| ApiError::bad_request("invalid from"))?
            .with_timezone(&Utc);
        cond = cond.add(event_logs::Column::OccurredAt.gte(at));
    }
    if let Some(v) = &q.to {
        let at = DateTime::parse_from_rfc3339(v)
            .map_err(|_| ApiError::bad_request("invalid to"))?
            .with_timezone(&Utc);
        cond = cond.add(event_logs::Column::OccurredAt.lte(at));
    }
    if let Some(raw) = &q.cursor {
        let (at, id) = decode_cursor(raw)?;
        cond = cond.add(
            Condition::any()
                .add(event_logs::Column::OccurredAt.lt(at))
                .add(
                    Condition::all()
                        .add(event_logs::Column::OccurredAt.eq(at))
                        .add(event_logs::Column::Id.lt(id)),
                ),
        );
    }
    Ok(event_logs::Entity::find().filter(cond))
}

fn no_store<T: Into<Body>>(body: T, content_type: &'static str) -> Response<Body> {
    Response::builder()
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_TYPE, content_type)
        .body(body.into())
        .unwrap()
}

async fn list(
    State(state): State<AppState>,
    Query(q): Query<LogQuery>,
) -> ApiResult<Response<Body>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 100);
    let mut rows = filtered(&q)?
        .order_by_desc(event_logs::Column::OccurredAt)
        .order_by_desc(event_logs::Column::Id)
        .limit(limit + 1)
        .all(&state.db())
        .await?;
    let has_more = rows.len() as u64 > limit;
    if has_more {
        rows.pop();
    }
    let next_cursor = has_more.then(|| rows.last().map(encode_cursor)).flatten();
    let items: Vec<_> = rows.into_iter().map(|row| json!({
        "id": row.id, "occurred_at": row.occurred_at, "category": row.category,
        "level": row.level, "event_type": row.event_type, "outcome": row.outcome,
        "actor_name": row.actor_name, "source_ip": row.source_ip, "request_id": row.request_id,
        "method": row.method, "route": row.route, "status_code": row.status_code,
        "duration_ms": row.duration_ms, "resource_type": row.resource_type,
        "resource_id": row.resource_id, "summary": row.summary,
    })).collect();
    Ok(no_store(
        serde_json::to_vec(
            &json!({ "items": items, "next_cursor": next_cursor, "has_more": has_more }),
        )
        .unwrap(),
        "application/json",
    ))
}

async fn detail(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response<Body>> {
    let row = event_logs::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(no_store(
        serde_json::to_vec(&row).unwrap(),
        "application/json",
    ))
}

async fn summary(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response<Body>> {
    let since = Utc::now() - Duration::days(7);
    let db = state.db();
    let newest = event_logs::Entity::find()
        .order_by_desc(event_logs::Column::Id)
        .one(&db)
        .await?;
    let newest = newest.map(|v| v.id).unwrap_or(0);
    let etag = format!("\"logs-{newest}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok(Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::empty())
            .unwrap());
    }
    let categories = join_all(
        [
            "auth", "security", "audit", "access", "system", "job", "backup",
        ]
        .into_iter()
        .map(|name| {
            let db = db.clone();
            async move {
                let count = event_logs::Entity::find()
                    .filter(event_logs::Column::OccurredAt.gte(since))
                    .filter(event_logs::Column::Category.eq(name))
                    .count(&db)
                    .await?;
                Ok::<_, sea_orm::DbErr>((name.to_string(), count))
            }
        }),
    )
    .await
    .into_iter()
    .collect::<Result<HashMap<_, _>, _>>()?;
    let levels = join_all(
        ["info", "warn", "error", "critical"]
            .into_iter()
            .map(|name| {
                let db = db.clone();
                async move {
                    let count = event_logs::Entity::find()
                        .filter(event_logs::Column::OccurredAt.gte(since))
                        .filter(event_logs::Column::Level.eq(name))
                        .count(&db)
                        .await?;
                    Ok::<_, sea_orm::DbErr>((name.to_string(), count))
                }
            }),
    )
    .await
    .into_iter()
    .collect::<Result<HashMap<_, _>, _>>()?;
    let today = Utc::now().date_naive();
    let days = join_all((0..7).rev().map(|offset| {
        let db = db.clone();
        async move {
            let date = today - Duration::days(offset);
            let start = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
            let end = start + Duration::days(1);
            let count = event_logs::Entity::find()
                .filter(event_logs::Column::OccurredAt.gte(start))
                .filter(event_logs::Column::OccurredAt.lt(end))
                .count(&db)
                .await?;
            Ok::<_, sea_orm::DbErr>((date.to_string(), count))
        }
    }))
    .await
    .into_iter()
    .collect::<Result<BTreeMap<_, _>, _>>()?;
    let cutoff24 = Utc::now() - Duration::hours(24);
    let last24h = event_logs::Entity::find()
        .filter(event_logs::Column::OccurredAt.gte(cutoff24))
        .count(&db)
        .await?;
    let blocked24h = event_logs::Entity::find()
        .filter(event_logs::Column::OccurredAt.gte(cutoff24))
        .filter(event_logs::Column::Outcome.eq("blocked"))
        .count(&db)
        .await?;
    let body = serde_json::to_vec(&json!({ "last_24h": last24h, "blocked_24h": blocked24h, "categories": categories, "levels": levels, "days": days, "newest_id": newest })).unwrap();
    Ok(Response::builder()
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap())
}

fn csv_cell(value: &str) -> String {
    let protected = if value.starts_with(['=', '+', '-', '@']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", protected.replace('"', "\"\""))
}

fn csv_row(v: &event_logs::Model) -> String {
    format!(
        "{}\n",
        [
            v.id.to_string(),
            csv_cell(&v.occurred_at.to_rfc3339()),
            csv_cell(&v.category),
            csv_cell(&v.level),
            csv_cell(&v.event_type),
            csv_cell(&v.outcome),
            csv_cell(v.actor_name.as_deref().unwrap_or("")),
            csv_cell(v.source_ip.as_deref().unwrap_or("")),
            csv_cell(v.request_id.as_deref().unwrap_or("")),
            csv_cell(v.method.as_deref().unwrap_or("")),
            csv_cell(v.route.as_deref().unwrap_or("")),
            v.status_code.map(|v| v.to_string()).unwrap_or_default(),
            v.duration_ms.map(|v| v.to_string()).unwrap_or_default(),
            csv_cell(&v.summary),
        ]
        .join(",")
    )
}

async fn export_logs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AdminContext>,
    Query(q): Query<LogQuery>,
) -> ApiResult<Response<Body>> {
    require_step_up(&ctx)?;
    if !matches!(q.format.as_deref(), None | Some("csv") | Some("jsonl")) {
        return Err(ApiError::bad_request("format must be csv or jsonl"));
    }
    let to =
        q.to.as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| ApiError::bad_request("invalid to"))?
            .map(|v| v.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
    let from = q
        .from
        .as_deref()
        .map(DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| ApiError::bad_request("invalid from"))?
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or(to - Duration::days(31));
    if to < from || to - from > Duration::days(31) {
        return Err(ApiError::bad_request("export range must be within 31 days"));
    }
    let jsonl = q.format.as_deref() == Some("jsonl");
    let content_type = if jsonl {
        "application/x-ndjson"
    } else {
        "text/csv; charset=utf-8"
    };
    let ext = if jsonl { "jsonl" } else { "csv" };
    let db = state.db();
    let mut query = q.clone();
    let stream = async_stream::try_stream! {
        if !jsonl {
            yield Bytes::from_static(b"id,occurred_at,category,level,event_type,outcome,actor,source_ip,request_id,method,route,status,duration_ms,summary\n");
        }
        let mut emitted = 0usize;
        loop {
            let rows = filtered(&query)
                .map_err(std::io::Error::other)?
                .filter(event_logs::Column::OccurredAt.gte(from))
                .filter(event_logs::Column::OccurredAt.lte(to))
                .order_by_desc(event_logs::Column::OccurredAt)
                .order_by_desc(event_logs::Column::Id)
                .limit(500.min((100_000usize - emitted) as u64))
                .all(&db)
                .await
                .map_err(std::io::Error::other)?;
            if rows.is_empty() {
                break;
            }
            query.cursor = rows.last().map(encode_cursor);
            let count = rows.len();
            for row in rows {
                let line = if jsonl {
                    let serialized =
                        serde_json::to_string(&row).map_err(std::io::Error::other)?;
                    format!("{serialized}\n")
                } else {
                    csv_row(&row)
                };
                yield Bytes::from(line);
            }
            emitted += count;
            if count < 500 || emitted >= 100_000 {
                break;
            }
        }
    };
    let stream = stream.map(|item: Result<Bytes, std::io::Error>| item);
    Ok(Response::builder()
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=event-logs.{ext}"),
        )
        .body(Body::from_stream(stream))
        .unwrap())
}

#[cfg(test)]
mod tests {
    use super::csv_cell;

    #[test]
    fn csv_cells_neutralize_formulas() {
        assert_eq!(csv_cell("=cmd()"), "\"'=cmd()\"");
        assert_eq!(csv_cell("normal"), "\"normal\"");
    }
}
