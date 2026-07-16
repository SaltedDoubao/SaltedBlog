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
use uuid::Uuid;

use crate::entities::{post_tags, posts, tags};
use crate::error::{ApiError, ApiResult};
use crate::render::{build_search_text, render_markdown, sanitize_slug};
use crate::routes::dto::{hydrate_posts, validate_lang};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/posts", get(list_posts).post(create_post))
        .route(
            "/posts/{id}",
            get(get_post).put(update_post).delete(delete_post),
        )
        .route("/render", post(preview_render))
}

// ---------- 列表 ----------

#[derive(Deserialize)]
struct AdminListQuery {
    lang: Option<String>,
    status: Option<String>,
    q: Option<String>,
    page: Option<u64>,
    page_size: Option<u64>,
}

async fn list_posts(
    State(state): State<AppState>,
    Query(q): Query<AdminListQuery>,
) -> ApiResult<impl IntoResponse> {
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(20).clamp(1, 100);

    let mut cond = Condition::all();
    if let Some(lang) = q.lang.as_deref().filter(|s| !s.is_empty()) {
        cond = cond.add(posts::Column::Lang.eq(lang));
    }
    if let Some(status) = q.status.as_deref().filter(|s| !s.is_empty()) {
        cond = cond.add(posts::Column::Status.eq(status));
    }
    if let Some(kw) = q.q.as_deref().filter(|s| !s.is_empty()) {
        cond = cond.add(posts::Column::Title.contains(kw));
    }

    let base = posts::Entity::find().filter(cond);
    let total = base.clone().count(&state.db()).await?;
    let items = base
        .order_by_desc(posts::Column::UpdatedAt)
        .offset((page - 1) * page_size)
        .limit(page_size)
        .all(&state.db())
        .await?;
    let items = hydrate_posts(&state.db(), items).await?;

    Ok(Json(json!({
        "items": items, "total": total, "page": page, "page_size": page_size
    })))
}

// ---------- 详情（含 Markdown 源码） ----------

async fn get_post(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    let post = posts::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;

    let tag_ids: Vec<i32> = post_tags::Entity::find()
        .filter(post_tags::Column::PostId.eq(post.id))
        .all(&state.db())
        .await?
        .into_iter()
        .map(|r| r.tag_id)
        .collect();

    // 同组的其它语言版本（便于后台关联展示）
    let siblings: Vec<serde_json::Value> = posts::Entity::find()
        .filter(
            Condition::all()
                .add(posts::Column::GroupId.eq(post.group_id.clone()))
                .add(posts::Column::Id.ne(post.id)),
        )
        .all(&state.db())
        .await?
        .iter()
        .map(|p| json!({ "id": p.id, "lang": p.lang, "title": p.title, "status": p.status }))
        .collect();

    let content_md = post.content_md.clone();
    let mut value = serde_json::to_value(&post).map_err(|e| ApiError::internal(e.to_string()))?;
    value["content_md"] = json!(content_md);
    value["tag_ids"] = json!(tag_ids);
    value["siblings"] = json!(siblings);
    Ok(Json(value))
}

// ---------- 创建 / 更新 ----------

#[derive(Deserialize)]
struct PostInput {
    lang: String,
    slug: Option<String>,
    title: String,
    summary: Option<String>,
    cover: Option<String>,
    content_md: String,
    status: String,
    category_id: Option<i32>,
    series_id: Option<i32>,
    series_order: Option<i32>,
    #[serde(default)]
    tag_ids: Vec<i32>,
    /// 关联已有文章组（翻译版本）；为空则新建组
    group_id: Option<String>,
}

struct PreparedPost {
    slug: String,
    html: String,
    toc_json: String,
    search_text: String,
}

async fn prepare(state: &AppState, input: &PostInput) -> ApiResult<PreparedPost> {
    if !validate_lang(&input.lang) {
        return Err(ApiError::bad_request("invalid lang"));
    }
    if input.title.trim().is_empty() {
        return Err(ApiError::bad_request("title required"));
    }
    if !matches!(
        input.status.as_str(),
        posts::STATUS_DRAFT | posts::STATUS_PUBLISHED
    ) {
        return Err(ApiError::bad_request("invalid status"));
    }

    let mut slug = sanitize_slug(input.slug.as_deref().unwrap_or(""));
    if slug.is_empty() {
        slug = sanitize_slug(&input.title);
    }
    if slug.is_empty() {
        slug = Uuid::new_v4().simple().to_string()[..8].to_string();
    }

    let rendered = render_markdown(&input.content_md);
    let toc_json =
        serde_json::to_string(&rendered.toc).map_err(|e| ApiError::internal(e.to_string()))?;

    // 标签名参与搜索
    let tag_names: Vec<String> = if input.tag_ids.is_empty() {
        Vec::new()
    } else {
        tags::Entity::find()
            .filter(tags::Column::Id.is_in(input.tag_ids.clone()))
            .all(&state.db())
            .await?
            .into_iter()
            .flat_map(|t| [t.name_zh, t.name_en])
            .collect()
    };
    let mut parts: Vec<&str> = vec![input.title.as_str()];
    parts.extend(tag_names.iter().map(|s| s.as_str()));
    parts.push(rendered.plain.as_str());
    let search_text = build_search_text(&state.jieba, &parts);

    Ok(PreparedPost {
        slug,
        html: rendered.html,
        toc_json,
        search_text,
    })
}

async fn slug_conflict(
    state: &AppState,
    lang: &str,
    slug: &str,
    exclude_id: Option<i32>,
) -> ApiResult<bool> {
    let mut cond = Condition::all()
        .add(posts::Column::Lang.eq(lang))
        .add(posts::Column::Slug.eq(slug));
    if let Some(id) = exclude_id {
        cond = cond.add(posts::Column::Id.ne(id));
    }
    Ok(posts::Entity::find().filter(cond).count(&state.db()).await? > 0)
}

async fn replace_tags(state: &AppState, post_id: i32, tag_ids: &[i32]) -> ApiResult<()> {
    post_tags::Entity::delete_many()
        .filter(post_tags::Column::PostId.eq(post_id))
        .exec(&state.db())
        .await?;
    for tag_id in tag_ids {
        let rel = post_tags::ActiveModel {
            post_id: Set(post_id),
            tag_id: Set(*tag_id),
        };
        // 忽略重复插入
        let _ = post_tags::Entity::insert(rel).exec(&state.db()).await;
    }
    Ok(())
}

async fn create_post(
    State(state): State<AppState>,
    Json(input): Json<PostInput>,
) -> ApiResult<impl IntoResponse> {
    let prepared = prepare(&state, &input).await?;
    if slug_conflict(&state, &input.lang, &prepared.slug, None).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("slug '{}' already exists for lang {}", prepared.slug, input.lang),
        ));
    }

    let now = Utc::now();
    let group_id = input
        .group_id
        .clone()
        .filter(|g| !g.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let published_at = if input.status == posts::STATUS_PUBLISHED {
        Some(now.into())
    } else {
        None
    };

    let model = posts::ActiveModel {
        group_id: Set(group_id),
        lang: Set(input.lang.clone()),
        slug: Set(prepared.slug),
        title: Set(input.title.trim().to_string()),
        summary: Set(input.summary.clone().filter(|s| !s.is_empty())),
        cover: Set(input.cover.clone().filter(|s| !s.is_empty())),
        content_md: Set(input.content_md.clone()),
        content_html: Set(prepared.html),
        toc_json: Set(Some(prepared.toc_json)),
        search_text: Set(prepared.search_text),
        status: Set(input.status.clone()),
        category_id: Set(input.category_id),
        series_id: Set(input.series_id),
        series_order: Set(input.series_order),
        view_count: Set(0),
        created_at: Set(now.into()),
        updated_at: Set(now.into()),
        published_at: Set(published_at),
        ..Default::default()
    };
    let post = model.insert(&state.db()).await?;
    replace_tags(&state, post.id, &input.tag_ids).await?;

    Ok((StatusCode::CREATED, Json(json!({ "id": post.id, "slug": post.slug }))))
}

async fn update_post(
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(input): Json<PostInput>,
) -> ApiResult<impl IntoResponse> {
    let existing = posts::Entity::find_by_id(id)
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;

    let prepared = prepare(&state, &input).await?;
    if slug_conflict(&state, &input.lang, &prepared.slug, Some(id)).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("slug '{}' already exists for lang {}", prepared.slug, input.lang),
        ));
    }

    let now = Utc::now();
    // 首次发布时记录发布时间；重新发布保留原时间
    let published_at = if input.status == posts::STATUS_PUBLISHED {
        existing.published_at.or_else(|| Some(now.into()))
    } else {
        existing.published_at
    };
    let group_id = input
        .group_id
        .clone()
        .filter(|g| !g.is_empty())
        .unwrap_or(existing.group_id.clone());

    let mut model: posts::ActiveModel = existing.into();
    model.group_id = Set(group_id);
    model.lang = Set(input.lang.clone());
    model.slug = Set(prepared.slug);
    model.title = Set(input.title.trim().to_string());
    model.summary = Set(input.summary.clone().filter(|s| !s.is_empty()));
    model.cover = Set(input.cover.clone().filter(|s| !s.is_empty()));
    model.content_md = Set(input.content_md.clone());
    model.content_html = Set(prepared.html);
    model.toc_json = Set(Some(prepared.toc_json));
    model.search_text = Set(prepared.search_text);
    model.status = Set(input.status.clone());
    model.category_id = Set(input.category_id);
    model.series_id = Set(input.series_id);
    model.series_order = Set(input.series_order);
    model.updated_at = Set(now.into());
    model.published_at = Set(published_at);
    let post = model.update(&state.db()).await?;

    replace_tags(&state, post.id, &input.tag_ids).await?;

    Ok(Json(json!({ "id": post.id, "slug": post.slug })))
}

async fn delete_post(
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<impl IntoResponse> {
    post_tags::Entity::delete_many()
        .filter(post_tags::Column::PostId.eq(id))
        .exec(&state.db())
        .await?;
    let res = posts::Entity::delete_by_id(id).exec(&state.db()).await?;
    if res.rows_affected == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- Markdown 预览 ----------

#[derive(Deserialize)]
struct RenderInput {
    markdown: String,
}

async fn preview_render(Json(input): Json<RenderInput>) -> ApiResult<impl IntoResponse> {
    let rendered = render_markdown(&input.markdown);
    Ok(Json(json!({ "html": rendered.html, "toc": rendered.toc })))
}
