use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
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

use crate::entities::{friends, page_views, posts, settings, site_icons, uploads};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn router(upload_max_mb: usize) -> Router<AppState> {
    let upload_limit = upload_max_mb.saturating_mul(1024 * 1024).max(1024 * 1024);
    Router::new()
        .route("/friends", get(list_friends).post(create_friend))
        .route("/friends/{id}", put(update_friend).delete(delete_friend))
        .route("/settings", get(get_settings).put(put_settings))
        .route(
            "/uploads",
            get(list_uploads)
                .post(upload_file)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route("/uploads/{id}", axum::routing::delete(delete_upload))
        .route(
            "/site-icons",
            get(list_site_icons)
                .post(upload_site_icon)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route("/site-icons/{id}/activate", put(activate_site_icon))
        .route("/site-icons/{id}", axum::routing::delete(delete_site_icon))
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
        .all(&state.db())
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
    let created = model.insert(&state.db()).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": created.id }))))
}

async fn update_friend(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(input): Json<FriendInput>,
) -> ApiResult<impl IntoResponse> {
    let existing = friends::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    let mut model: friends::ActiveModel = existing.into();
    model.name = Set(input.name.trim().to_string());
    model.url = Set(input.url.trim().to_string());
    model.avatar = Set(input.avatar.clone().filter(|s| !s.is_empty()));
    model.description = Set(input.description.clone().filter(|s| !s.is_empty()));
    model.sort_order = Set(input.sort_order.unwrap_or(0));
    model.update(&state.db()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_friend(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    friends::Entity::delete_by_id(id).exec(&state.db()).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- 站点设置 ----------

async fn get_settings(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let map: HashMap<String, String> = settings::Entity::find()
        .all(&state.db())
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
    const ALLOWED: &[&str] = &[
        "author",
        "icp",
        "site_title_zh",
        "site_title_en",
        "description_zh",
        "description_en",
        "home_eyebrow_zh",
        "home_eyebrow_en",
        "home_news_title_zh",
        "home_news_title_en",
        "home_news_description_zh",
        "home_news_description_en",
        "home_world_title_zh",
        "home_world_title_en",
        "home_world_description_zh",
        "home_world_description_en",
        "home_system_title_zh",
        "home_system_title_en",
        "home_system_description_zh",
        "home_system_description_en",
        "home_operator_title_zh",
        "home_operator_title_en",
        "home_operator_description_zh",
        "home_operator_description_en",
        "home_protocol_title_zh",
        "home_protocol_title_en",
        "home_protocol_description_zh",
        "home_protocol_description_en",
        "giscus_repo",
        "giscus_repo_id",
        "giscus_category",
        "giscus_category_id",
        "social_github",
        "social_email",
        "about_zh",
        "about_en",
        "news_enabled",
        "news_fetch_interval_hours",
        "news_digest_time",
        "news_digest_auto_publish",
        "news_retention_days",
        "news_log_retention_days",
        "news_llm_base_url",
        "news_llm_model",
        "news_llm_extra_prompt",
    ];
    for (key, value) in input {
        if !ALLOWED.contains(&key.as_str()) {
            return Err(ApiError::bad_request("unknown setting key"));
        }
        let max_len = if matches!(key.as_str(), "about_zh" | "about_en") {
            100_000
        } else if key == "news_llm_extra_prompt" {
            20_000
        } else {
            10_000
        };
        if value.len() > max_len || value.contains('\0') {
            return Err(ApiError::bad_request("setting value too large"));
        }
        if key == "news_llm_base_url" && !value.trim().is_empty() {
            crate::outbound::validate_public_https(value.trim())
                .await
                .map_err(|_| {
                    ApiError::bad_request("LLM Base URL must resolve to public HTTPS port 443")
                })?;
        }
        if key == "social_github" && !value.trim().is_empty() {
            let parsed = url::Url::parse(value.trim())
                .map_err(|_| ApiError::bad_request("GitHub URL is invalid"))?;
            if parsed.scheme() != "https"
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.port().is_some_and(|port| port != 443)
            {
                return Err(ApiError::bad_request("GitHub URL must use public HTTPS"));
            }
        }
        if key == "social_email"
            && (!value.is_empty()
                && (value.len() > 254 || value.contains(['\r', '\n']) || !value.contains('@')))
        {
            return Err(ApiError::bad_request("email setting is invalid"));
        }
        let existing = settings::Entity::find_by_id(key.clone())
            .one(&state.db())
            .await?;
        match existing {
            Some(row) => {
                let mut model: settings::ActiveModel = row.into();
                model.value = Set(value);
                model.update(&state.db()).await?;
            }
            None => {
                let model = settings::ActiveModel {
                    key: Set(key),
                    value: Set(value),
                };
                model.insert(&state.db()).await?;
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- 图片上传 ----------

fn process_image(data: &[u8]) -> ApiResult<(Vec<u8>, &'static str, &'static str)> {
    if data.is_empty() {
        return Err(ApiError::bad_request("empty file"));
    }
    let format = image::guess_format(data)
        .map_err(|_| ApiError::bad_request("unsupported or invalid image"))?;
    if !matches!(
        format,
        image::ImageFormat::Jpeg
            | image::ImageFormat::Png
            | image::ImageFormat::Gif
            | image::ImageFormat::WebP
            | image::ImageFormat::Avif
    ) {
        return Err(ApiError::bad_request(
            "only JPEG, PNG, GIF, WebP and AVIF raster images are allowed",
        ));
    }
    let mut reader = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|_| ApiError::bad_request("invalid image"))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| ApiError::bad_request("image decoding failed"))?;
    let (width, height) = (image.width(), image.height());
    if width == 0
        || height == 0
        || width > 12_000
        || height > 12_000
        || u64::from(width) * u64::from(height) > 40_000_000
    {
        return Err(ApiError::bad_request(
            "image dimensions exceed security limit",
        ));
    }
    let mut output = Cursor::new(Vec::new());
    let (ext, mime) = if image.color().has_alpha() {
        image
            .write_to(&mut output, image::ImageFormat::Png)
            .map_err(|_| ApiError::internal("image encoding failed"))?;
        ("png", "image/png")
    } else {
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 88);
        encoder
            .encode_image(&image)
            .map_err(|_| ApiError::internal("image encoding failed"))?;
        ("jpg", "image/jpeg")
    };
    let output = output.into_inner();
    if output.len() > 40 * 1024 * 1024 {
        return Err(ApiError::bad_request("encoded image too large"));
    }
    Ok((output, ext, mime))
}

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
        let data = field
            .bytes()
            .await
            .map_err(|e| ApiError::bad_request(format!("read file error: {e}")))?;
        let (data, ext, mime) = tokio::task::spawn_blocking(move || process_image(&data))
            .await
            .map_err(|_| ApiError::internal("image worker failed"))??;

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
            mime: Set(mime.to_string()),
            size_bytes: Set(data.len() as i64),
            created_at: Set(now.into()),
            ..Default::default()
        };
        let created = model.insert(&state.db()).await?;

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
    let total = base.clone().count(&state.db()).await?;
    let items: Vec<serde_json::Value> = base
        .order_by_desc(uploads::Column::Id)
        .offset((page - 1) * page_size)
        .limit(page_size)
        .all(&state.db())
        .await?
        .iter()
        .map(|u| {
            let mut v = serde_json::to_value(u).unwrap();
            v["url"] = json!(format!("/uploads/{}", u.path));
            let ext = std::path::Path::new(&u.path)
                .extension()
                .and_then(|v| v.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            v["blocked"] = json!(!matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp"));
            v
        })
        .collect();
    Ok(Json(
        json!({ "items": items, "total": total, "page": page, "page_size": page_size }),
    ))
}

async fn delete_upload(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    let row = uploads::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    let abs = state.cfg.upload_dir.join(&row.path);
    let _ = tokio::fs::remove_file(abs).await;
    uploads::Entity::delete_by_id(id).exec(&state.db()).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- 站点图标 ----------

const SITE_ICON_URL_KEY: &str = "site_icon_url";

async fn upsert_setting(
    db: &sea_orm::DatabaseConnection,
    key: &str,
    value: String,
) -> ApiResult<()> {
    let existing = settings::Entity::find_by_id(key.to_string())
        .one(db)
        .await?;
    match existing {
        Some(row) => {
            let mut model: settings::ActiveModel = row.into();
            model.value = Set(value);
            model.update(db).await?;
        }
        None => {
            settings::ActiveModel {
                key: Set(key.to_string()),
                value: Set(value),
            }
            .insert(db)
            .await?;
        }
    }
    Ok(())
}

async fn clear_setting(db: &sea_orm::DatabaseConnection, key: &str) -> ApiResult<()> {
    settings::Entity::delete_by_id(key.to_string())
        .exec(db)
        .await?;
    Ok(())
}

async fn deactivate_all_site_icons(db: &sea_orm::DatabaseConnection) -> ApiResult<()> {
    let active = site_icons::Entity::find()
        .filter(site_icons::Column::IsActive.eq(true))
        .all(db)
        .await?;
    for row in active {
        let mut model: site_icons::ActiveModel = row.into();
        model.is_active = Set(false);
        model.update(db).await?;
    }
    Ok(())
}

async fn set_active_site_icon(
    db: &sea_orm::DatabaseConnection,
    icon: &site_icons::Model,
) -> ApiResult<String> {
    let upload = uploads::Entity::find_by_id(icon.upload_id)
        .one(db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let url = format!("/uploads/{}", upload.path);
    deactivate_all_site_icons(db).await?;
    let mut model: site_icons::ActiveModel = icon.clone().into();
    model.is_active = Set(true);
    model.update(db).await?;
    upsert_setting(db, SITE_ICON_URL_KEY, url.clone()).await?;
    Ok(url)
}

async fn list_site_icons(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let rows = site_icons::Entity::find()
        .order_by_desc(site_icons::Column::Id)
        .all(&state.db())
        .await?;

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let upload = uploads::Entity::find_by_id(row.upload_id)
            .one(&state.db())
            .await?;
        let Some(upload) = upload else {
            continue;
        };
        items.push(json!({
            "id": row.id,
            "upload_id": row.upload_id,
            "url": format!("/uploads/{}", upload.path),
            "mime": upload.mime,
            "original_name": upload.original_name,
            "is_active": row.is_active,
            "created_at": row.created_at,
        }));
    }
    Ok(Json(json!({ "items": items })))
}

async fn upload_site_icon(
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
        let data = field
            .bytes()
            .await
            .map_err(|e| ApiError::bad_request(format!("read file error: {e}")))?;
        let (data, ext, mime) = tokio::task::spawn_blocking(move || process_image(&data))
            .await
            .map_err(|_| ApiError::internal("image worker failed"))??;

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

        let upload = uploads::ActiveModel {
            path: Set(rel_path.clone()),
            original_name: Set(original_name.clone()),
            mime: Set(mime.to_string()),
            size_bytes: Set(data.len() as i64),
            created_at: Set(now.into()),
            ..Default::default()
        }
        .insert(&state.db())
        .await?;

        deactivate_all_site_icons(&state.db()).await?;

        let icon = site_icons::ActiveModel {
            upload_id: Set(upload.id),
            is_active: Set(true),
            created_at: Set(now.into()),
            ..Default::default()
        }
        .insert(&state.db())
        .await?;

        let url = format!("/uploads/{rel_path}");
        upsert_setting(&state.db(), SITE_ICON_URL_KEY, url.clone()).await?;

        return Ok((
            StatusCode::CREATED,
            Json(json!({
                "id": icon.id,
                "upload_id": upload.id,
                "url": url,
                "mime": mime,
                "original_name": original_name,
                "is_active": true,
                "created_at": icon.created_at,
            })),
        ));
    }
    Err(ApiError::bad_request("missing 'file' field"))
}

async fn activate_site_icon(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    let icon = site_icons::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    let url = set_active_site_icon(&state.db(), &icon).await?;
    Ok(Json(
        json!({ "id": icon.id, "url": url, "is_active": true }),
    ))
}

async fn delete_site_icon(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    let icon = site_icons::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;
    let was_active = icon.is_active;
    let upload_id = icon.upload_id;

    site_icons::Entity::delete_by_id(id)
        .exec(&state.db())
        .await?;

    if let Some(upload) = uploads::Entity::find_by_id(upload_id)
        .one(&state.db())
        .await?
    {
        let abs = state.cfg.upload_dir.join(&upload.path);
        let _ = tokio::fs::remove_file(abs).await;
        uploads::Entity::delete_by_id(upload_id)
            .exec(&state.db())
            .await?;
    }

    if was_active {
        clear_setting(&state.db(), SITE_ICON_URL_KEY).await?;
    }

    Ok(StatusCode::NO_CONTENT)
}

// ---------- 统计面板 ----------

async fn stats_dashboard(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let total_posts = posts::Entity::find().count(&state.db()).await?;
    let total_published = posts::Entity::find()
        .filter(posts::Column::Status.eq(posts::STATUS_PUBLISHED))
        .count(&state.db())
        .await?;
    let total_pv = page_views::Entity::find().count(&state.db()).await?;

    let offset = FixedOffset::east_opt(state.cfg.stats_tz_offset_hours * 3600)
        .unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    let today = Utc::now().with_timezone(&offset).date_naive();
    let cutoff = (today - Duration::days(29)).format("%Y-%m-%d").to_string();

    let rows = page_views::Entity::find()
        .filter(page_views::Column::Date.gte(cutoff.clone()))
        .all(&state.db())
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
