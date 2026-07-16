use std::collections::{HashMap, HashSet};

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{routing::get, routing::put, Json, Router};
use chrono::{Duration, FixedOffset, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::entities::{friends, page_views, posts, settings, uploads};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/friends", get(list_friends).post(create_friend))
        .route("/friends/{id}", put(update_friend).delete(delete_friend))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/uploads", get(list_uploads).post(upload_file))
        .route("/uploads/{id}", axum::routing::delete(delete_upload))
        .route("/stats/dashboard", get(stats_dashboard))
}

// ---------- 友链 ----------

#[derive(Deserialize)]
struct FriendInput {
    name: String,
    url: String,
    avatar: Option<String>,
    description: Option<String>,
    sort_order: Option<i32>,
}

async fn list_friends(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let items = friends::Entity::find()
        .order_by_asc(friends::Column::SortOrder)
        .order_by_asc(friends::Column::Id)
        .all(&state.db)
        .await?;
    Ok(Json(json!({ "items": items })))
}

async fn create_friend(
    State(state): State<AppState>,
    Json(input): Json<FriendInput>,
) -> ApiResult<impl IntoResponse> {
    if input.name.trim().is_empty() || input.url.trim().is_empty() {
        return Err(ApiError::bad_request("name and url are required"));
    }
    let model = friends::ActiveModel {
        name: Set(input.name.trim().to_string()),
        url: Set(input.url.trim().to_string()),
        avatar: Set(input.avatar.clone().filter(|s| !s.is_empty())),
        description: Set(input.description.clone().filter(|s| !s.is_empty())),
        sort_order: Set(input.sort_order.unwrap_or(0)),
        created_at: Set(Utc::now().into()),
        ..Default::default()
    };
    let created = model.insert(&state.db).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": created.id }))))
}

async fn update_friend(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(input): Json<FriendInput>,
) -> ApiResult<impl IntoResponse> {
    let existing = friends::Entity::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let mut model: friends::ActiveModel = existing.into();
    model.name = Set(input.name.trim().to_string());
    model.url = Set(input.url.trim().to_string());
    model.avatar = Set(input.avatar.clone().filter(|s| !s.is_empty()));
    model.description = Set(input.description.clone().filter(|s| !s.is_empty()));
    model.sort_order = Set(input.sort_order.unwrap_or(0));
    model.update(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_friend(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    friends::Entity::delete_by_id(id).exec(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- 站点设置 ----------

async fn get_settings(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let map: HashMap<String, String> = settings::Entity::find()
        .all(&state.db)
        .await?
        .into_iter()
        .map(|s| (s.key, s.value))
        .collect();
    Ok(Json(json!(map)))
}

async fn put_settings(
    State(state): State<AppState>,
    Json(input): Json<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    for (key, value) in input {
        if key.len() > 128 {
            continue;
        }
        let existing = settings::Entity::find_by_id(key.clone()).one(&state.db).await?;
        match existing {
            Some(row) => {
                let mut model: settings::ActiveModel = row.into();
                model.value = Set(value);
                model.update(&state.db).await?;
            }
            None => {
                let model = settings::ActiveModel {
                    key: Set(key),
                    value: Set(value),
                };
                model.insert(&state.db).await?;
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- 图片上传 ----------

const ALLOWED_EXT: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "svg", "avif", "ico"];

async fn upload_file(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> ApiResult<impl IntoResponse> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("multipart error: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let original_name = field.file_name().unwrap_or("file").to_string();
        let ext = original_name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase();
        if !ALLOWED_EXT.contains(&ext.as_str()) {
            return Err(ApiError::bad_request(format!(
                "file type .{ext} not allowed (allowed: {})",
                ALLOWED_EXT.join(", ")
            )));
        }
        let mime = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let data = field
            .bytes()
            .await
            .map_err(|e| ApiError::bad_request(format!("read file error: {e}")))?;
        if data.is_empty() {
            return Err(ApiError::bad_request("empty file"));
        }

        let now = Utc::now();
        let rel_dir = now.format("%Y/%m").to_string();
        let filename = format!("{}.{}", Uuid::new_v4().simple(), ext);
        let rel_path = format!("{rel_dir}/{filename}");

        let abs_dir = state.cfg.upload_dir.join(&rel_dir);
        tokio::fs::create_dir_all(&abs_dir)
            .await
            .map_err(|e| ApiError::internal(format!("mkdir failed: {e}")))?;
        tokio::fs::write(abs_dir.join(&filename), &data)
            .await
            .map_err(|e| ApiError::internal(format!("write failed: {e}")))?;

        let model = uploads::ActiveModel {
            path: Set(rel_path.clone()),
            original_name: Set(original_name),
            mime: Set(mime),
            size_bytes: Set(data.len() as i64),
            created_at: Set(now.into()),
            ..Default::default()
        };
        let created = model.insert(&state.db).await?;

        return Ok((
            StatusCode::CREATED,
            Json(json!({
                "id": created.id,
                "path": rel_path,
                "url": format!("/uploads/{rel_path}"),
            })),
        ));
    }
    Err(ApiError::bad_request("missing 'file' field"))
}

#[derive(Deserialize)]
struct PageQuery {
    page: Option<u64>,
    page_size: Option<u64>,
}

async fn list_uploads(
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
) -> ApiResult<impl IntoResponse> {
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(24).clamp(1, 100);
    let base = uploads::Entity::find();
    let total = base.clone().count(&state.db).await?;
    let items: Vec<serde_json::Value> = base
        .order_by_desc(uploads::Column::Id)
        .offset((page - 1) * page_size)
        .limit(page_size)
        .all(&state.db)
        .await?
        .iter()
        .map(|u| {
            let mut v = serde_json::to_value(u).unwrap();
            v["url"] = json!(format!("/uploads/{}", u.path));
            v
        })
        .collect();
    Ok(Json(json!({ "items": items, "total": total, "page": page, "page_size": page_size })))
}

async fn delete_upload(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    let row = uploads::Entity::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let abs = state.cfg.upload_dir.join(&row.path);
    let _ = tokio::fs::remove_file(abs).await;
    uploads::Entity::delete_by_id(id).exec(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- 统计面板 ----------

async fn stats_dashboard(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let total_posts = posts::Entity::find().count(&state.db).await?;
    let total_published = posts::Entity::find()
        .filter(posts::Column::Status.eq(posts::STATUS_PUBLISHED))
        .count(&state.db)
        .await?;
    let total_pv = page_views::Entity::find().count(&state.db).await?;

    let offset = FixedOffset::east_opt(state.cfg.stats_tz_offset_hours * 3600)
        .unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    let today = Utc::now().with_timezone(&offset).date_naive();
    let cutoff = (today - Duration::days(29)).format("%Y-%m-%d").to_string();

    let rows = page_views::Entity::find()
        .filter(page_views::Column::Date.gte(cutoff.clone()))
        .all(&state.db)
        .await?;

    // 按天聚合 PV / UV
    let mut day_pv: HashMap<String, u64> = HashMap::new();
    let mut day_visitors: HashMap<String, HashSet<String>> = HashMap::new();
    let mut path_pv: HashMap<String, u64> = HashMap::new();
    for row in &rows {
        *day_pv.entry(row.date.clone()).or_default() += 1;
        day_visitors
            .entry(row.date.clone())
            .or_default()
            .insert(row.visitor_hash.clone());
        *path_pv.entry(row.path.clone()).or_default() += 1;
    }

    let mut days = Vec::with_capacity(30);
    for i in (0..30).rev() {
        let date = (today - Duration::days(i)).format("%Y-%m-%d").to_string();
        days.push(json!({
            "date": date,
            "pv": day_pv.get(&date).copied().unwrap_or(0),
            "uv": day_visitors.get(&date).map(|s| s.len()).unwrap_or(0),
        }));
    }

    let mut top_paths: Vec<(String, u64)> = path_pv.into_iter().collect();
    top_paths.sort_by(|a, b| b.1.cmp(&a.1));
    top_paths.truncate(10);
    let top_paths: Vec<serde_json::Value> = top_paths
        .into_iter()
        .map(|(path, pv)| json!({ "path": path, "pv": pv }))
        .collect();

    let today_str = today.format("%Y-%m-%d").to_string();
    Ok(Json(json!({
        "total_posts": total_posts,
        "total_published": total_published,
        "total_pv": total_pv,
        "today_pv": day_pv.get(&today_str).copied().unwrap_or(0),
        "today_uv": day_visitors.get(&today_str).map(|s| s.len()).unwrap_or(0),
        "days": days,
        "top_paths": top_paths,
    })))
}
