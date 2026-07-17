use std::collections::HashMap;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{routing::get, routing::post, Json, Router};
use chrono::{FixedOffset, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::entities::{
    categories, digest_jobs, friends, page_views, post_tags, posts, series, settings, tags,
};
use crate::error::{ApiError, ApiResult};
use crate::news::llm::DigestDoc;
use crate::render::{render_markdown, tokenize_query};
use crate::routes::dto::{hydrate_posts, validate_lang, CategoryOut, SeriesOut, TagOut};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/posts", get(list_posts))
        .route("/posts/{lang}/{slug}", get(post_detail))
        .route("/archive", get(archive))
        .route("/taxonomy", get(taxonomy))
        .route("/search", get(search))
        .route("/friends", get(list_friends))
        .route("/settings/public", get(public_settings))
        .route("/about", get(about_page))
        .route("/sitemap", get(sitemap_data))
        .route("/news/latest", get(latest_digest))
        .route("/track", post(track))
}

fn lang_of(query_lang: Option<&str>) -> ApiResult<String> {
    let lang = query_lang.unwrap_or("zh");
    if !validate_lang(lang) {
        return Err(ApiError::bad_request("invalid lang"));
    }
    Ok(lang.to_string())
}

fn published_filter(lang: &str) -> Condition {
    Condition::all()
        .add(posts::Column::Lang.eq(lang))
        .add(posts::Column::Status.eq(posts::STATUS_PUBLISHED))
}

// ---------- 文章列表 ----------

#[derive(Deserialize)]
struct ListQuery {
    lang: Option<String>,
    page: Option<u64>,
    page_size: Option<u64>,
    category: Option<String>,
    tag: Option<String>,
    series: Option<String>,
}

async fn list_posts(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<impl IntoResponse> {
    let lang = lang_of(q.lang.as_deref())?;
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(10).clamp(1, 50);

    let mut cond = published_filter(&lang);

    if let Some(cat_slug) = q.category.as_deref().filter(|s| !s.is_empty()) {
        let cat = categories::Entity::find()
            .filter(categories::Column::Slug.eq(cat_slug))
            .one(&state.db())
            .await?
            .ok_or_else(ApiError::not_found)?;
        cond = cond.add(posts::Column::CategoryId.eq(cat.id));
    }

    if let Some(series_slug) = q.series.as_deref().filter(|s| !s.is_empty()) {
        let sr = series::Entity::find()
            .filter(series::Column::Slug.eq(series_slug))
            .one(&state.db())
            .await?
            .ok_or_else(ApiError::not_found)?;
        cond = cond.add(posts::Column::SeriesId.eq(sr.id));
    }

    if let Some(tag_slug) = q.tag.as_deref().filter(|s| !s.is_empty()) {
        let tag = tags::Entity::find()
            .filter(tags::Column::Slug.eq(tag_slug))
            .one(&state.db())
            .await?
            .ok_or_else(ApiError::not_found)?;
        let ids: Vec<i32> = post_tags::Entity::find()
            .filter(post_tags::Column::TagId.eq(tag.id))
            .all(&state.db())
            .await?
            .into_iter()
            .map(|r| r.post_id)
            .collect();
        if ids.is_empty() {
            return Ok(Json(json!({
                "items": [], "total": 0, "page": page, "page_size": page_size
            })));
        }
        cond = cond.add(posts::Column::Id.is_in(ids));
    }

    let base = posts::Entity::find().filter(cond);
    let total = base.clone().count(&state.db()).await?;
    let items = base
        .order_by_desc(posts::Column::PublishedAt)
        .offset((page - 1) * page_size)
        .limit(page_size)
        .all(&state.db())
        .await?;
    let items = hydrate_posts(&state.db(), items).await?;

    Ok(Json(json!({
        "items": items, "total": total, "page": page, "page_size": page_size
    })))
}

// ---------- 文章详情 ----------

async fn post_detail(
    State(state): State<AppState>,
    Path((lang, slug)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    if !validate_lang(&lang) {
        return Err(ApiError::bad_request("invalid lang"));
    }
    let post = posts::Entity::find()
        .filter(published_filter(&lang).add(posts::Column::Slug.eq(slug)))
        .one(&state.db())
        .await?
        .ok_or_else(ApiError::not_found)?;

    let toc: serde_json::Value = post
        .toc_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!([]));

    // 同组翻译版本
    let translations: Vec<serde_json::Value> = posts::Entity::find()
        .filter(
            Condition::all()
                .add(posts::Column::GroupId.eq(post.group_id.clone()))
                .add(posts::Column::Id.ne(post.id))
                .add(posts::Column::Status.eq(posts::STATUS_PUBLISHED)),
        )
        .all(&state.db())
        .await?
        .iter()
        .map(|p| json!({ "lang": p.lang, "slug": p.slug, "title": p.title }))
        .collect();

    // 系列内文章
    let series_posts: Vec<serde_json::Value> = if let Some(series_id) = post.series_id {
        posts::Entity::find()
            .filter(published_filter(&post.lang).add(posts::Column::SeriesId.eq(series_id)))
            .order_by_asc(posts::Column::SeriesOrder)
            .order_by_asc(posts::Column::PublishedAt)
            .all(&state.db())
            .await?
            .iter()
            .map(|p| {
                json!({
                    "id": p.id, "slug": p.slug, "title": p.title,
                    "series_order": p.series_order, "current": p.id == post.id
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    // 上一篇 / 下一篇（按发布时间）
    let (prev, next) = if let Some(published_at) = post.published_at {
        let prev = posts::Entity::find()
            .filter(
                published_filter(&post.lang)
                    .add(posts::Column::PublishedAt.lt(published_at))
                    .add(posts::Column::Id.ne(post.id)),
            )
            .order_by_desc(posts::Column::PublishedAt)
            .one(&state.db())
            .await?;
        let next = posts::Entity::find()
            .filter(
                published_filter(&post.lang)
                    .add(posts::Column::PublishedAt.gt(published_at))
                    .add(posts::Column::Id.ne(post.id)),
            )
            .order_by_asc(posts::Column::PublishedAt)
            .one(&state.db())
            .await?;
        (prev, next)
    } else {
        (None, None)
    };
    let nav_json = |p: Option<posts::Model>| {
        p.map(|p| json!({ "slug": p.slug, "title": p.title }))
            .unwrap_or(serde_json::Value::Null)
    };

    let content_html = post.content_html.clone();
    let hydrated = hydrate_posts(&state.db(), vec![post]).await?;
    let item = hydrated
        .into_iter()
        .next()
        .ok_or_else(ApiError::not_found)?;

    Ok(Json(json!({
        "post": item,
        "content_html": content_html,
        "toc": toc,
        "translations": translations,
        "series_posts": series_posts,
        "prev": nav_json(prev),
        "next": nav_json(next),
    })))
}

// ---------- 归档 ----------

#[derive(Deserialize)]
struct LangQuery {
    lang: Option<String>,
}

async fn archive(
    State(state): State<AppState>,
    Query(q): Query<LangQuery>,
) -> ApiResult<impl IntoResponse> {
    let lang = lang_of(q.lang.as_deref())?;
    let items: Vec<serde_json::Value> = posts::Entity::find()
        .filter(published_filter(&lang))
        .order_by_desc(posts::Column::PublishedAt)
        .all(&state.db())
        .await?
        .iter()
        .map(|p| {
            json!({
                "slug": p.slug, "title": p.title,
                "published_at": p.published_at, "view_count": p.view_count
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

// ---------- 分类法总览 ----------

async fn taxonomy(
    State(state): State<AppState>,
    Query(q): Query<LangQuery>,
) -> ApiResult<impl IntoResponse> {
    let lang = lang_of(q.lang.as_deref())?;
    let published = posts::Entity::find()
        .filter(published_filter(&lang))
        .all(&state.db())
        .await?;
    let post_ids: Vec<i32> = published.iter().map(|p| p.id).collect();

    let mut cat_count: HashMap<i32, usize> = HashMap::new();
    let mut series_count: HashMap<i32, usize> = HashMap::new();
    for p in &published {
        if let Some(id) = p.category_id {
            *cat_count.entry(id).or_default() += 1;
        }
        if let Some(id) = p.series_id {
            *series_count.entry(id).or_default() += 1;
        }
    }

    let mut tag_count: HashMap<i32, usize> = HashMap::new();
    if !post_ids.is_empty() {
        for rel in post_tags::Entity::find()
            .filter(post_tags::Column::PostId.is_in(post_ids))
            .all(&state.db())
            .await?
        {
            *tag_count.entry(rel.tag_id).or_default() += 1;
        }
    }

    let categories_out: Vec<serde_json::Value> = categories::Entity::find()
        .order_by_asc(categories::Column::SortOrder)
        .all(&state.db())
        .await?
        .iter()
        .map(|c| {
            let mut v = serde_json::to_value(CategoryOut::from(c)).unwrap();
            v["count"] = json!(cat_count.get(&c.id).copied().unwrap_or(0));
            v
        })
        .collect();

    let tags_out: Vec<serde_json::Value> = tags::Entity::find()
        .all(&state.db())
        .await?
        .iter()
        .map(|t| {
            let mut v = serde_json::to_value(TagOut::from(t)).unwrap();
            v["count"] = json!(tag_count.get(&t.id).copied().unwrap_or(0));
            v
        })
        .collect();

    let series_out: Vec<serde_json::Value> = series::Entity::find()
        .all(&state.db())
        .await?
        .iter()
        .map(|s| {
            let mut v = serde_json::to_value(SeriesOut::from(s)).unwrap();
            v["count"] = json!(series_count.get(&s.id).copied().unwrap_or(0));
            v["description_zh"] = json!(s.description_zh);
            v["description_en"] = json!(s.description_en);
            v
        })
        .collect();

    Ok(Json(json!({
        "categories": categories_out,
        "tags": tags_out,
        "series": series_out,
    })))
}

// ---------- 搜索 ----------

#[derive(Deserialize)]
struct SearchQuery {
    lang: Option<String>,
    q: Option<String>,
}

async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<impl IntoResponse> {
    let lang = lang_of(query.lang.as_deref())?;
    let raw = query.q.unwrap_or_default().trim().to_string();
    if raw.is_empty() {
        return Ok(Json(json!({ "items": [], "q": raw })));
    }

    let terms = tokenize_query(&state.jieba, &raw);
    let mut term_cond = Condition::all();
    for term in &terms {
        term_cond = term_cond.add(posts::Column::SearchText.contains(term));
    }
    let cond = published_filter(&lang).add(
        Condition::any()
            .add(term_cond)
            .add(posts::Column::Title.contains(&raw)),
    );

    let items = posts::Entity::find()
        .filter(cond)
        .order_by_desc(posts::Column::PublishedAt)
        .limit(30)
        .all(&state.db())
        .await?;
    let items = hydrate_posts(&state.db(), items).await?;

    Ok(Json(json!({ "items": items, "q": raw })))
}

// ---------- 友链 / 设置 / 关于 ----------

async fn list_friends(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let items = friends::Entity::find()
        .order_by_asc(friends::Column::SortOrder)
        .order_by_asc(friends::Column::Id)
        .all(&state.db())
        .await?;
    Ok(Json(json!({ "items": items })))
}

/// 公开设置白名单：仅前台展示所需的键，避免暴露 LLM/采集等内部配置
const PUBLIC_SETTING_PREFIXES: &[&str] = &["site_", "home_", "description_", "giscus_", "social_"];
const PUBLIC_SETTING_KEYS: &[&str] = &["author", "icp"];

fn is_public_setting(key: &str) -> bool {
    PUBLIC_SETTING_KEYS.contains(&key)
        || PUBLIC_SETTING_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

async fn public_settings(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let map: HashMap<String, String> = settings::Entity::find()
        .all(&state.db())
        .await?
        .into_iter()
        .filter(|s| is_public_setting(&s.key))
        .map(|s| (s.key, s.value))
        .collect();
    Ok(Json(json!(map)))
}

async fn about_page(
    State(state): State<AppState>,
    Query(q): Query<LangQuery>,
) -> ApiResult<impl IntoResponse> {
    let lang = lang_of(q.lang.as_deref())?;
    let key = format!("about_{lang}");
    let md = settings::Entity::find_by_id(key)
        .one(&state.db())
        .await?
        .map(|s| s.value)
        .unwrap_or_default();
    let rendered = render_markdown(&md);
    Ok(Json(json!({ "html": rendered.html, "toc": rendered.toc })))
}

// ---------- 最新日报（主页「最新情报」与滚动字幕数据源） ----------

/// 返回最新一期「生成成功且中文文章已发布」的日报；不存在时 digest 为 null
async fn latest_digest(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let jobs = digest_jobs::Entity::find()
        .filter(digest_jobs::Column::Status.eq(digest_jobs::STATUS_SUCCESS))
        .order_by_desc(digest_jobs::Column::DigestDate)
        .order_by_desc(digest_jobs::Column::Id)
        .limit(10)
        .all(&state.db())
        .await?;

    for job in jobs {
        let Some(raw) = job.result_json.as_deref() else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<DigestDoc>(raw) else {
            continue;
        };
        let Some(post_id) = job.post_id_zh else {
            continue;
        };
        let Some(post) = posts::Entity::find_by_id(post_id).one(&state.db()).await? else {
            continue;
        };
        if post.status != posts::STATUS_PUBLISHED {
            continue;
        }
        let items = doc.items_by_importance();
        return Ok(Json(json!({
            "digest": {
                "date": job.digest_date,
                "slug": post.slug,
                "title_zh": doc.title_zh,
                "title_en": doc.title_en,
                "summary_zh": doc.summary_zh,
                "summary_en": doc.summary_en,
                "item_count": doc.item_count(),
                "generated_at": job.finished_at,
                "items": items,
            }
        })));
    }
    Ok(Json(json!({ "digest": null })))
}

// ---------- sitemap 数据 ----------

async fn sitemap_data(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let all_posts: Vec<serde_json::Value> = posts::Entity::find()
        .filter(posts::Column::Status.eq(posts::STATUS_PUBLISHED))
        .all(&state.db())
        .await?
        .iter()
        .map(|p| {
            json!({
                "lang": p.lang, "slug": p.slug,
                "updated_at": p.updated_at, "published_at": p.published_at
            })
        })
        .collect();
    let cats: Vec<String> = categories::Entity::find()
        .all(&state.db())
        .await?
        .into_iter()
        .map(|c| c.slug)
        .collect();
    let tag_slugs: Vec<String> = tags::Entity::find()
        .all(&state.db())
        .await?
        .into_iter()
        .map(|t| t.slug)
        .collect();
    let series_slugs: Vec<String> = series::Entity::find()
        .all(&state.db())
        .await?
        .into_iter()
        .map(|s| s.slug)
        .collect();

    Ok(Json(json!({
        "posts": all_posts,
        "categories": cats,
        "tags": tag_slugs,
        "series": series_slugs,
    })))
}

// ---------- 访问统计上报 ----------

#[derive(Deserialize)]
struct TrackInput {
    path: String,
    post_id: Option<i32>,
    referrer: Option<String>,
}

async fn track(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<TrackInput>,
) -> ApiResult<impl IntoResponse> {
    let path = input.path.trim();
    if path.is_empty() || path.len() > 400 || !path.starts_with('/') {
        return Err(ApiError::bad_request("invalid path"));
    }
    // 后台页面不计入统计
    if path.starts_with("/admin") {
        return Ok(StatusCode::NO_CONTENT);
    }

    let ip = crate::auth::client_ip(&headers, Some(&addr), &state.cfg);
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let offset = FixedOffset::east_opt(state.cfg.stats_tz_offset_hours * 3600)
        .unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    let now_local = Utc::now().with_timezone(&offset);
    let date = now_local.format("%Y-%m-%d").to_string();

    let mut hasher = Sha256::new();
    hasher.update(format!("{ip}|{ua}|{date}|{}", state.track_salt));
    let visitor_hash = hex::encode(hasher.finalize())[..32].to_string();

    // 同一访客同一天同一路径只记一次
    let exists = page_views::Entity::find()
        .filter(
            Condition::all()
                .add(page_views::Column::VisitorHash.eq(visitor_hash.clone()))
                .add(page_views::Column::Path.eq(path))
                .add(page_views::Column::Date.eq(date.clone())),
        )
        .count(&state.db())
        .await?;
    if exists > 0 {
        return Ok(StatusCode::NO_CONTENT);
    }

    let referrer = input
        .referrer
        .filter(|r| !r.is_empty())
        .map(|r| r.chars().take(400).collect::<String>());

    let row = page_views::ActiveModel {
        path: Set(path.to_string()),
        referrer: Set(referrer),
        visitor_hash: Set(visitor_hash),
        date: Set(date),
        created_at: Set(Utc::now().into()),
        ..Default::default()
    };
    page_views::Entity::insert(row).exec(&state.db()).await?;

    if let Some(post_id) = input.post_id {
        let _ = posts::Entity::update_many()
            .col_expr(
                posts::Column::ViewCount,
                Expr::col(posts::Column::ViewCount).add(1),
            )
            .filter(posts::Column::Id.eq(post_id))
            .exec(&state.db())
            .await;
    }

    Ok(StatusCode::NO_CONTENT)
}
