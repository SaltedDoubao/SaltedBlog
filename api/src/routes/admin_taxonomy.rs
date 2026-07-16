use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{routing::get, Json, Router};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use serde::Deserialize;
use serde_json::json;

use crate::entities::{categories, posts, series, tags};
use crate::error::{ApiError, ApiResult};
use crate::render::sanitize_slug;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/categories", get(list_categories).post(create_category))
        .route(
            "/categories/{id}",
            get(noop).put(update_category).delete(delete_category),
        )
        .route("/tags", get(list_tags).post(create_tag))
        .route("/tags/{id}", get(noop).put(update_tag).delete(delete_tag))
        .route("/series", get(list_series).post(create_series))
        .route(
            "/series/{id}",
            get(noop).put(update_series).delete(delete_series),
        )
}

async fn noop() -> StatusCode {
    StatusCode::NOT_FOUND
}

#[derive(Deserialize)]
struct TaxonomyInput {
    slug: Option<String>,
    name_zh: String,
    name_en: String,
    description_zh: Option<String>,
    description_en: Option<String>,
    sort_order: Option<i32>,
}

fn resolve_slug(input: &TaxonomyInput) -> ApiResult<String> {
    let mut slug = sanitize_slug(input.slug.as_deref().unwrap_or(""));
    if slug.is_empty() {
        slug = sanitize_slug(&input.name_en);
    }
    if slug.is_empty() {
        return Err(ApiError::bad_request("slug required (name_en not sluggable)"));
    }
    Ok(slug)
}

fn require_names(input: &TaxonomyInput) -> ApiResult<()> {
    if input.name_zh.trim().is_empty() || input.name_en.trim().is_empty() {
        return Err(ApiError::bad_request("name_zh and name_en are required"));
    }
    Ok(())
}

// ---------- categories ----------

async fn list_categories(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let items = categories::Entity::find()
        .order_by_asc(categories::Column::SortOrder)
        .order_by_asc(categories::Column::Id)
        .all(&state.db)
        .await?;
    let mut out = Vec::new();
    for c in items {
        let count = posts::Entity::find()
            .filter(posts::Column::CategoryId.eq(c.id))
            .count(&state.db)
            .await?;
        let mut v = serde_json::to_value(&c).unwrap();
        v["post_count"] = json!(count);
        out.push(v);
    }
    Ok(Json(json!({ "items": out })))
}

async fn create_category(
    State(state): State<AppState>,
    Json(input): Json<TaxonomyInput>,
) -> ApiResult<impl IntoResponse> {
    require_names(&input)?;
    let slug = resolve_slug(&input)?;
    let model = categories::ActiveModel {
        slug: Set(slug),
        name_zh: Set(input.name_zh.trim().to_string()),
        name_en: Set(input.name_en.trim().to_string()),
        sort_order: Set(input.sort_order.unwrap_or(0)),
        created_at: Set(Utc::now().into()),
        ..Default::default()
    };
    let created = model
        .insert(&state.db)
        .await
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, format!("create failed: {e}")))?;
    Ok((StatusCode::CREATED, Json(json!({ "id": created.id }))))
}

async fn update_category(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(input): Json<TaxonomyInput>,
) -> ApiResult<impl IntoResponse> {
    require_names(&input)?;
    let existing = categories::Entity::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let slug = resolve_slug(&input)?;
    let mut model: categories::ActiveModel = existing.into();
    model.slug = Set(slug);
    model.name_zh = Set(input.name_zh.trim().to_string());
    model.name_en = Set(input.name_en.trim().to_string());
    model.sort_order = Set(input.sort_order.unwrap_or(0));
    model.update(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_category(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    // 解除文章引用
    posts::Entity::update_many()
        .col_expr(posts::Column::CategoryId, sea_orm::sea_query::Expr::value(sea_orm::Value::Int(None)))
        .filter(posts::Column::CategoryId.eq(id))
        .exec(&state.db)
        .await?;
    categories::Entity::delete_by_id(id).exec(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- tags ----------

async fn list_tags(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let items = tags::Entity::find()
        .order_by_asc(tags::Column::Id)
        .all(&state.db)
        .await?;
    let mut out = Vec::new();
    for t in items {
        let count = crate::entities::post_tags::Entity::find()
            .filter(crate::entities::post_tags::Column::TagId.eq(t.id))
            .count(&state.db)
            .await?;
        let mut v = serde_json::to_value(&t).unwrap();
        v["post_count"] = json!(count);
        out.push(v);
    }
    Ok(Json(json!({ "items": out })))
}

async fn create_tag(
    State(state): State<AppState>,
    Json(input): Json<TaxonomyInput>,
) -> ApiResult<impl IntoResponse> {
    require_names(&input)?;
    let slug = resolve_slug(&input)?;
    let model = tags::ActiveModel {
        slug: Set(slug),
        name_zh: Set(input.name_zh.trim().to_string()),
        name_en: Set(input.name_en.trim().to_string()),
        created_at: Set(Utc::now().into()),
        ..Default::default()
    };
    let created = model
        .insert(&state.db)
        .await
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, format!("create failed: {e}")))?;
    Ok((StatusCode::CREATED, Json(json!({ "id": created.id }))))
}

async fn update_tag(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(input): Json<TaxonomyInput>,
) -> ApiResult<impl IntoResponse> {
    require_names(&input)?;
    let existing = tags::Entity::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let slug = resolve_slug(&input)?;
    let mut model: tags::ActiveModel = existing.into();
    model.slug = Set(slug);
    model.name_zh = Set(input.name_zh.trim().to_string());
    model.name_en = Set(input.name_en.trim().to_string());
    model.update(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_tag(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    crate::entities::post_tags::Entity::delete_many()
        .filter(crate::entities::post_tags::Column::TagId.eq(id))
        .exec(&state.db)
        .await?;
    tags::Entity::delete_by_id(id).exec(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- series ----------

async fn list_series(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let items = series::Entity::find()
        .order_by_asc(series::Column::Id)
        .all(&state.db)
        .await?;
    let mut out = Vec::new();
    for s in items {
        let count = posts::Entity::find()
            .filter(posts::Column::SeriesId.eq(s.id))
            .count(&state.db)
            .await?;
        let mut v = serde_json::to_value(&s).unwrap();
        v["post_count"] = json!(count);
        out.push(v);
    }
    Ok(Json(json!({ "items": out })))
}

async fn create_series(
    State(state): State<AppState>,
    Json(input): Json<TaxonomyInput>,
) -> ApiResult<impl IntoResponse> {
    require_names(&input)?;
    let slug = resolve_slug(&input)?;
    let model = series::ActiveModel {
        slug: Set(slug),
        name_zh: Set(input.name_zh.trim().to_string()),
        name_en: Set(input.name_en.trim().to_string()),
        description_zh: Set(input.description_zh.clone().filter(|s| !s.is_empty())),
        description_en: Set(input.description_en.clone().filter(|s| !s.is_empty())),
        created_at: Set(Utc::now().into()),
        ..Default::default()
    };
    let created = model
        .insert(&state.db)
        .await
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, format!("create failed: {e}")))?;
    Ok((StatusCode::CREATED, Json(json!({ "id": created.id }))))
}

async fn update_series(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(input): Json<TaxonomyInput>,
) -> ApiResult<impl IntoResponse> {
    require_names(&input)?;
    let existing = series::Entity::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let slug = resolve_slug(&input)?;
    let mut model: series::ActiveModel = existing.into();
    model.slug = Set(slug);
    model.name_zh = Set(input.name_zh.trim().to_string());
    model.name_en = Set(input.name_en.trim().to_string());
    model.description_zh = Set(input.description_zh.clone().filter(|s| !s.is_empty()));
    model.description_en = Set(input.description_en.clone().filter(|s| !s.is_empty()));
    model.update(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_series(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    posts::Entity::update_many()
        .col_expr(posts::Column::SeriesId, sea_orm::sea_query::Expr::value(sea_orm::Value::Int(None)))
        .col_expr(posts::Column::SeriesOrder, sea_orm::sea_query::Expr::value(sea_orm::Value::Int(None)))
        .filter(posts::Column::SeriesId.eq(id))
        .exec(&state.db)
        .await?;
    series::Entity::delete_by_id(id).exec(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}
