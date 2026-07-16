//! 后台情报管理：信源 CRUD、试抓、立即采集、条目审计、采集日志、日报任务
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use serde::Deserialize;
use serde_json::json;

use crate::entities::{digest_jobs, news_fetch_logs, news_items, news_sources};
use crate::error::{ApiError, ApiResult};
use crate::news::{digest, fetch, fetcher, filter, local_date_string};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/news/sources", get(list_sources).post(create_source))
        .route(
            "/news/sources/{id}",
            axum::routing::put(update_source).delete(delete_source),
        )
        .route("/news/sources/{id}/test", post(test_source))
        .route("/news/sources/{id}/fetch", post(fetch_one))
        .route("/news/fetch-all", post(fetch_all_now))
        .route("/news/items", get(list_items))
        .route("/news/logs", get(list_logs))
        .route("/news/jobs", get(list_jobs))
        .route("/news/digest/generate", post(generate_digest))
}

// ---------- 信源 CRUD ----------

async fn list_sources(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let items = news_sources::Entity::find()
        .order_by_asc(news_sources::Column::Id)
        .all(&state.db())
        .await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
struct SourceInput {
    name: String,
    source_type: String,
    url: String,
    category: Option<String>,
    language: Option<String>,
    include_keywords: Option<String>,
    exclude_keywords: Option<String>,
    max_items: Option<i32>,
    enabled: Option<bool>,
    send_to_llm: Option<bool>,
    weight: Option<f64>,
    github_language: Option<String>,
    github_since: Option<String>,
    min_stars: Option<i32>,
}

struct ValidatedSource {
    name: String,
    source_type: String,
    url: String,
    category: Option<String>,
    language: String,
    include_keywords: Option<String>,
    exclude_keywords: Option<String>,
    max_items: i32,
    enabled: bool,
    send_to_llm: bool,
    weight: f64,
    github_language: Option<String>,
    github_since: Option<String>,
    min_stars: Option<i32>,
}

fn validate_source(input: &SourceInput) -> Result<ValidatedSource, ApiError> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("name required"));
    }
    let url = input.url.trim().to_string();
    if url.is_empty() {
        return Err(ApiError::bad_request("url required"));
    }
    if !matches!(
        input.source_type.as_str(),
        news_sources::TYPE_RSS | news_sources::TYPE_GITHUB_TRENDING
    ) {
        return Err(ApiError::bad_request("invalid source_type"));
    }
    let language = input.language.clone().unwrap_or_else(|| "zh".into());
    if !matches!(language.as_str(), "zh" | "en") {
        return Err(ApiError::bad_request("invalid language"));
    }
    let github_since = input
        .github_since
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(since) = &github_since {
        if !matches!(since.as_str(), "daily" | "weekly" | "monthly") {
            return Err(ApiError::bad_request("invalid github_since"));
        }
    }
    let none_if_empty = |v: &Option<String>| {
        v.clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Ok(ValidatedSource {
        name,
        source_type: input.source_type.clone(),
        url,
        category: none_if_empty(&input.category),
        language,
        include_keywords: none_if_empty(&input.include_keywords),
        exclude_keywords: none_if_empty(&input.exclude_keywords),
        max_items: input.max_items.unwrap_or(30).clamp(1, 100),
        enabled: input.enabled.unwrap_or(true),
        send_to_llm: input.send_to_llm.unwrap_or(true),
        weight: input.weight.unwrap_or(1.0).clamp(0.0, 2.0),
        github_language: none_if_empty(&input.github_language),
        github_since,
        min_stars: input.min_stars.filter(|n| *n > 0),
    })
}

async fn create_source(
    State(state): State<AppState>,
    Json(input): Json<SourceInput>,
) -> ApiResult<impl IntoResponse> {
    let v = validate_source(&input)?;
    let now = Utc::now();
    let model = news_sources::ActiveModel {
        name: Set(v.name),
        source_type: Set(v.source_type),
        url: Set(v.url),
        category: Set(v.category),
        language: Set(v.language),
        include_keywords: Set(v.include_keywords),
        exclude_keywords: Set(v.exclude_keywords),
        max_items: Set(v.max_items),
        enabled: Set(v.enabled),
        send_to_llm: Set(v.send_to_llm),
        weight: Set(v.weight),
        github_language: Set(v.github_language),
        github_since: Set(v.github_since),
        min_stars: Set(v.min_stars),
        created_at: Set(now.into()),
        updated_at: Set(now.into()),
        ..Default::default()
    };
    let row = model.insert(&state.db()).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": row.id }))))
}

async fn update_source(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(input): Json<SourceInput>,
) -> ApiResult<impl IntoResponse> {
    let existing = news_sources::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    let v = validate_source(&input)?;
    let mut model: news_sources::ActiveModel = existing.into();
    model.name = Set(v.name);
    model.source_type = Set(v.source_type);
    model.url = Set(v.url);
    model.category = Set(v.category);
    model.language = Set(v.language);
    model.include_keywords = Set(v.include_keywords);
    model.exclude_keywords = Set(v.exclude_keywords);
    model.max_items = Set(v.max_items);
    model.enabled = Set(v.enabled);
    model.send_to_llm = Set(v.send_to_llm);
    model.weight = Set(v.weight);
    model.github_language = Set(v.github_language);
    model.github_since = Set(v.github_since);
    model.min_stars = Set(v.min_stars);
    model.updated_at = Set(Utc::now().into());
    let row = model.update(&state.db()).await?;
    Ok(Json(json!({ "id": row.id })))
}

/// 删除信源并级联清理其条目与日志（历史日报文章不受影响）
async fn delete_source(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    news_items::Entity::delete_many()
        .filter(news_items::Column::SourceId.eq(id))
        .exec(&state.db())
        .await?;
    news_fetch_logs::Entity::delete_many()
        .filter(news_fetch_logs::Column::SourceId.eq(id))
        .exec(&state.db())
        .await?;
    let res = news_sources::Entity::delete_by_id(id).exec(&state.db()).await?;
    if res.rows_affected == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- 试抓与采集 ----------

/// 试抓：调用采集器返回前 5 条样本与关键词过滤预判，不写库
async fn test_source(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    let source = news_sources::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    let outcome = fetcher::fetch_source_items(&source).await;
    let samples: Vec<serde_json::Value> = outcome
        .items
        .iter()
        .take(5)
        .map(|item| {
            let eval = filter::evaluate(
                &item.title,
                item.summary.as_deref(),
                item.content.as_deref(),
                source.include_keywords.as_deref(),
                source.exclude_keywords.as_deref(),
            );
            json!({
                "title": item.title,
                "url": item.url,
                "summary": item.summary,
                "published_at": item.published_at,
                "author": item.author,
                "extra": item.extra,
                "would_accept": eval.accepted,
                "filter_reason": eval.reason,
                "matched_keywords": eval.matched_keywords,
            })
        })
        .collect();
    Ok(Json(json!({
        "total": outcome.items.len(),
        "http_status": outcome.http_status,
        "error": outcome.error,
        "samples": samples,
    })))
}

async fn fetch_one(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    let source = news_sources::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    let summary = fetch::fetch_source(&state.db(), &source).await;
    Ok(Json(json!({ "summary": summary })))
}

async fn fetch_all_now(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let summaries = fetch::fetch_all(&state.db()).await;
    Ok(Json(json!({ "summaries": summaries })))
}

// ---------- 条目审计 / 日志 / 任务 ----------

#[derive(Deserialize)]
struct ItemsQuery {
    status: Option<String>,
    source_id: Option<i32>,
    page: Option<u64>,
    page_size: Option<u64>,
}

async fn list_items(
    State(state): State<AppState>,
    Query(q): Query<ItemsQuery>,
) -> ApiResult<impl IntoResponse> {
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(20).clamp(1, 100);

    let mut cond = Condition::all();
    if let Some(status) = q.status.as_deref().filter(|s| !s.is_empty()) {
        cond = cond.add(news_items::Column::Status.eq(status));
    }
    if let Some(source_id) = q.source_id {
        cond = cond.add(news_items::Column::SourceId.eq(source_id));
    }

    let base = news_items::Entity::find().filter(cond);
    let total = base.clone().count(&state.db()).await?;
    let items = base
        .order_by_desc(news_items::Column::FetchedAt)
        .order_by_desc(news_items::Column::Id)
        .offset((page - 1) * page_size)
        .limit(page_size)
        .all(&state.db())
        .await?;

    let source_names = source_name_map(&state).await?;
    let items: Vec<serde_json::Value> = items
        .into_iter()
        .map(|item| {
            let mut value = serde_json::to_value(&item).unwrap_or_else(|_| json!({}));
            value["source_name"] = json!(source_names.get(&item.source_id));
            // 审计列表不需要正文与指纹细节
            if let Some(obj) = value.as_object_mut() {
                obj.remove("content");
                obj.remove("dedup_key");
                obj.remove("url_hash");
                obj.remove("title_hash");
                obj.remove("content_hash");
            }
            value
        })
        .collect();

    Ok(Json(json!({
        "items": items, "total": total, "page": page, "page_size": page_size
    })))
}

#[derive(Deserialize)]
struct PageQuery {
    page: Option<u64>,
    page_size: Option<u64>,
}

async fn list_logs(
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
) -> ApiResult<impl IntoResponse> {
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(20).clamp(1, 100);
    let base = news_fetch_logs::Entity::find();
    let total = base.clone().count(&state.db()).await?;
    let logs = base
        .order_by_desc(news_fetch_logs::Column::Id)
        .offset((page - 1) * page_size)
        .limit(page_size)
        .all(&state.db())
        .await?;
    let source_names = source_name_map(&state).await?;
    let logs: Vec<serde_json::Value> = logs
        .into_iter()
        .map(|log| {
            let mut value = serde_json::to_value(&log).unwrap_or_else(|_| json!({}));
            value["source_name"] = json!(source_names.get(&log.source_id));
            value
        })
        .collect();
    Ok(Json(json!({
        "items": logs, "total": total, "page": page, "page_size": page_size
    })))
}

async fn list_jobs(
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
) -> ApiResult<impl IntoResponse> {
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(20).clamp(1, 100);
    let base = digest_jobs::Entity::find();
    let total = base.clone().count(&state.db()).await?;
    let jobs = base
        .order_by_desc(digest_jobs::Column::Id)
        .offset((page - 1) * page_size)
        .limit(page_size)
        .all(&state.db())
        .await?;
    // 列表不携带 result_json（体积大），仅保留状态与计数
    let jobs: Vec<serde_json::Value> = jobs
        .into_iter()
        .map(|job| {
            json!({
                "id": job.id,
                "digest_date": job.digest_date,
                "trigger": job.trigger,
                "status": job.status,
                "raw_count": job.raw_count,
                "selected_count": job.selected_count,
                "error_message": job.error_message,
                "llm_model": job.llm_model,
                "post_id_zh": job.post_id_zh,
                "post_id_en": job.post_id_en,
                "started_at": job.started_at,
                "finished_at": job.finished_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "items": jobs, "total": total, "page": page, "page_size": page_size
    })))
}

// ---------- 手动生成日报 ----------

#[derive(Deserialize, Default)]
struct GenerateInput {
    #[serde(default)]
    force: bool,
}

/// 生成在后台任务中执行（避免浏览器断开导致中断），前端轮询任务列表查看进度
async fn generate_digest(
    State(state): State<AppState>,
    input: Option<Json<GenerateInput>>,
) -> ApiResult<impl IntoResponse> {
    let force = input.map(|Json(v)| v.force).unwrap_or(false);
    let date = local_date_string(state.cfg.stats_tz_offset_hours);

    if !force {
        let existing = digest_jobs::Entity::find()
            .filter(digest_jobs::Column::DigestDate.eq(&date))
            .count(&state.db())
            .await?;
        if existing > 0 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "当日已存在日报任务，勾选「强制重新生成」可覆盖当日文章",
            ));
        }
    }

    let state_clone = state.clone();
    tokio::spawn(async move {
        match digest::generate(&state_clone, digest_jobs::TRIGGER_MANUAL, force).await {
            Ok(job) => tracing::info!("manual digest job {} -> {}", job.id, job.status),
            Err(e) => tracing::warn!("manual digest generation rejected: {e}"),
        }
    });

    Ok((StatusCode::ACCEPTED, Json(json!({ "accepted": true, "date": date }))))
}

async fn source_name_map(
    state: &AppState,
) -> Result<std::collections::HashMap<i32, String>, ApiError> {
    let sources = news_sources::Entity::find().all(&state.db()).await?;
    Ok(sources.into_iter().map(|s| (s.id, s.name)).collect())
}
