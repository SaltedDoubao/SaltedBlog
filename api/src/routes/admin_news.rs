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
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::Deserialize;
use serde_json::json;

use crate::entities::{digest_jobs, news_fetch_logs, news_items, news_sources, news_tasks};
use crate::error::{ApiError, ApiResult};
use crate::news::{digest, fetch, fetcher, filter, local_date_string, tasks};
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
        .route("/news/tasks", get(list_tasks).post(create_task))
        .route(
            "/news/tasks/{id}",
            axum::routing::put(update_task).delete(delete_task),
        )
        .route("/news/tasks/{id}/toggle", axum::routing::put(toggle_task))
        .route("/news/tasks/{id}/run", post(run_task))
        .route("/news/items", get(list_items))
        .route("/news/logs", get(list_logs))
        .route("/news/jobs", get(list_jobs))
}

// ---------- 定时任务 CRUD ----------

#[derive(Deserialize)]
struct TaskInput {
    name: String,
    task_type: String,
    enabled: Option<bool>,
    start_time: Option<String>,
    interval_hours: Option<i32>,
    generation_time: Option<String>,
    publish_time: Option<String>,
    publish_mode: Option<String>,
}

struct ValidatedTask {
    name: String,
    task_type: String,
    enabled: bool,
    start_time: Option<String>,
    interval_hours: Option<i32>,
    generation_time: Option<String>,
    publish_time: Option<String>,
    publish_mode: Option<String>,
}

fn validate_task(input: &TaskInput) -> Result<ValidatedTask, ApiError> {
    let name = input.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 128 {
        return Err(ApiError::bad_request("任务名称长度应为 1-128 个字符"));
    }
    let valid_time = |value: &Option<String>, label: &str| -> Result<String, ApiError> {
        let value = value.as_deref().unwrap_or("").trim();
        if tasks::parse_hhmm(value).is_none() {
            return Err(ApiError::bad_request(format!("{label}格式应为 HH:MM")));
        }
        Ok(value.to_string())
    };
    let (start_time, interval_hours, generation_time, publish_time, publish_mode) =
        match input.task_type.as_str() {
            news_tasks::TYPE_FETCH => {
                if input
                    .generation_time
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                    || input
                        .publish_time
                        .as_deref()
                        .is_some_and(|value| !value.is_empty())
                    || input
                        .publish_mode
                        .as_deref()
                        .is_some_and(|value| !value.is_empty())
                {
                    return Err(ApiError::bad_request("采集任务不能包含整理发布配置"));
                }
                let interval = input.interval_hours.unwrap_or(0);
                if !(1..=24).contains(&interval) {
                    return Err(ApiError::bad_request("采集间隔应为 1-24 小时"));
                }
                (
                    Some(valid_time(&input.start_time, "起始时间")?),
                    Some(interval),
                    None,
                    None,
                    None,
                )
            }
            news_tasks::TYPE_DIGEST => {
                if input
                    .start_time
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                    || input.interval_hours.is_some()
                {
                    return Err(ApiError::bad_request("整理发布任务不能包含采集配置"));
                }
                let generation = valid_time(&input.generation_time, "生成时间")?;
                let mode = input.publish_mode.as_deref().unwrap_or("");
                let publish = match mode {
                    news_tasks::PUBLISH_MODE_DRAFT => {
                        if input
                            .publish_time
                            .as_deref()
                            .is_some_and(|value| !value.is_empty())
                        {
                            return Err(ApiError::bad_request("保存草稿模式不能设置发布时间"));
                        }
                        None
                    }
                    news_tasks::PUBLISH_MODE_SCHEDULED => {
                        let publish = valid_time(&input.publish_time, "发布时间")?;
                        if tasks::parse_hhmm(&publish) <= tasks::parse_hhmm(&generation) {
                            return Err(ApiError::bad_request("发布时间必须晚于当天生成时间"));
                        }
                        Some(publish)
                    }
                    _ => return Err(ApiError::bad_request("无效的生成后处理方式")),
                };
                (
                    None,
                    None,
                    Some(generation),
                    publish,
                    Some(mode.to_string()),
                )
            }
            _ => return Err(ApiError::bad_request("无效的任务类型")),
        };
    Ok(ValidatedTask {
        name,
        task_type: input.task_type.clone(),
        enabled: input.enabled.unwrap_or(false),
        start_time,
        interval_hours,
        generation_time,
        publish_time,
        publish_mode,
    })
}

async fn list_tasks(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let items = news_tasks::Entity::find()
        .order_by_asc(news_tasks::Column::Id)
        .all(&state.db())
        .await?;
    Ok(Json(json!({ "items": items })))
}

async fn create_task(
    State(state): State<AppState>,
    Json(input): Json<TaskInput>,
) -> ApiResult<impl IntoResponse> {
    let task = validate_task(&input)?;
    let now = Utc::now();
    let row = news_tasks::ActiveModel {
        name: Set(task.name),
        task_type: Set(task.task_type),
        enabled: Set(task.enabled),
        start_time: Set(task.start_time),
        interval_hours: Set(task.interval_hours),
        generation_time: Set(task.generation_time),
        publish_time: Set(task.publish_time),
        publish_mode: Set(task.publish_mode),
        last_scheduled_at: Set(None),
        created_at: Set(now.into()),
        updated_at: Set(now.into()),
        ..Default::default()
    }
    .insert(&state.db())
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": row.id }))))
}

async fn update_task(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(input): Json<TaskInput>,
) -> ApiResult<impl IntoResponse> {
    let row = news_tasks::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    if row.task_type != input.task_type {
        return Err(ApiError::bad_request("任务创建后不能修改类型"));
    }
    let task = validate_task(&input)?;
    let schedule_changed =
        row.start_time != task.start_time || row.interval_hours != task.interval_hours;
    let mut model: news_tasks::ActiveModel = row.into();
    model.name = Set(task.name);
    model.task_type = Set(task.task_type);
    model.enabled = Set(task.enabled);
    model.start_time = Set(task.start_time);
    model.interval_hours = Set(task.interval_hours);
    model.generation_time = Set(task.generation_time);
    model.publish_time = Set(task.publish_time);
    model.publish_mode = Set(task.publish_mode);
    if schedule_changed {
        model.last_scheduled_at = Set(None);
    }
    model.updated_at = Set(Utc::now().into());
    model.update(&state.db()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_task(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    let result = news_tasks::Entity::delete_by_id(id)
        .exec(&state.db())
        .await?;
    if result.rows_affected == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ToggleInput {
    enabled: bool,
}

async fn toggle_task(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(input): Json<ToggleInput>,
) -> ApiResult<impl IntoResponse> {
    let row = news_tasks::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    let mut model: news_tasks::ActiveModel = row.into();
    model.enabled = Set(input.enabled);
    model.updated_at = Set(Utc::now().into());
    model.update(&state.db()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
struct RunTaskInput {
    #[serde(default)]
    force: bool,
}

async fn run_task(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    input: Option<Json<RunTaskInput>>,
) -> ApiResult<impl IntoResponse> {
    let task = news_tasks::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    let force = input.map(|Json(value)| value.force).unwrap_or(false);
    if task.task_type == news_tasks::TYPE_DIGEST && !force {
        let date = local_date_string(state.cfg.stats_tz_offset_hours);
        let existing = digest_jobs::Entity::find()
            .filter(
                Condition::all()
                    .add(digest_jobs::Column::NewsTaskId.eq(task.id))
                    .add(digest_jobs::Column::DigestDate.eq(date)),
            )
            .count(&state.db())
            .await?;
        if existing > 0 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "该任务今天已有执行记录，强制执行将覆盖当天文章",
            ));
        }
    }
    let task_type = task.task_type.clone();
    let state_clone = state.clone();
    tokio::spawn(async move {
        if task_type == news_tasks::TYPE_FETCH {
            let summaries = fetch::fetch_all(&state_clone.db()).await;
            tracing::info!(
                task_id = task.id,
                sources = summaries.len(),
                "manual fetch task finished"
            );
        } else if let Err(error) = digest::generate(
            &state_clone,
            &task,
            digest_jobs::TRIGGER_MANUAL,
            force,
            None,
        )
        .await
        {
            tracing::warn!(task_id = task.id, "manual digest task rejected: {error}");
        }
    });
    Ok((StatusCode::ACCEPTED, Json(json!({ "accepted": true }))))
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
    let res = news_sources::Entity::delete_by_id(id)
        .exec(&state.db())
        .await?;
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
                "post_id": job.post_id,
                "started_at": job.started_at,
                "finished_at": job.finished_at,
                "news_task_id": job.news_task_id,
                "task_name": job.task_name,
                "scheduled_publish_at": job.scheduled_publish_at,
                "published_at": job.published_at,
                "publish_error": job.publish_error,
            })
        })
        .collect();
    Ok(Json(json!({
        "items": jobs, "total": total, "page": page, "page_size": page_size
    })))
}

async fn source_name_map(
    state: &AppState,
) -> Result<std::collections::HashMap<i32, String>, ApiError> {
    let sources = news_sources::Entity::find().all(&state.db()).await?;
    Ok(sources.into_iter().map(|s| (s.id, s.name)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_fetch_task_fields() {
        let input = TaskInput {
            name: "采集".into(),
            task_type: news_tasks::TYPE_FETCH.into(),
            enabled: Some(true),
            start_time: Some("08:00".into()),
            interval_hours: Some(2),
            generation_time: None,
            publish_time: None,
            publish_mode: None,
        };
        let task = validate_task(&input).unwrap();
        assert_eq!(task.start_time.as_deref(), Some("08:00"));
        assert!(task.generation_time.is_none());
    }

    #[test]
    fn rejects_cross_day_digest_schedule() {
        let input = TaskInput {
            name: "日报".into(),
            task_type: news_tasks::TYPE_DIGEST.into(),
            enabled: Some(false),
            start_time: None,
            interval_hours: None,
            generation_time: Some("20:00".into()),
            publish_time: Some("08:00".into()),
            publish_mode: Some(news_tasks::PUBLISH_MODE_SCHEDULED.into()),
        };
        assert!(validate_task(&input).is_err());
    }

    #[test]
    fn draft_digest_has_no_publish_time() {
        let input = TaskInput {
            name: "日报".into(),
            task_type: news_tasks::TYPE_DIGEST.into(),
            enabled: Some(false),
            start_time: None,
            interval_hours: None,
            generation_time: Some("08:00".into()),
            publish_time: None,
            publish_mode: Some(news_tasks::PUBLISH_MODE_DRAFT.into()),
        };
        let task = validate_task(&input).unwrap();
        assert!(task.publish_time.is_none());
    }
}
