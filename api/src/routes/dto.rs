use std::collections::{HashMap, HashSet};

use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::Serialize;

use crate::entities::{categories, post_tags, posts, series, tags};
use crate::error::ApiResult;

#[derive(Serialize, Clone)]
pub struct CategoryOut {
    pub id: i32,
    pub slug: String,
    pub name_zh: String,
    pub name_en: String,
}

impl From<&categories::Model> for CategoryOut {
    fn from(m: &categories::Model) -> Self {
        Self {
            id: m.id,
            slug: m.slug.clone(),
            name_zh: m.name_zh.clone(),
            name_en: m.name_en.clone(),
        }
    }
}

#[derive(Serialize, Clone)]
pub struct TagOut {
    pub id: i32,
    pub slug: String,
    pub name_zh: String,
    pub name_en: String,
}

impl From<&tags::Model> for TagOut {
    fn from(m: &tags::Model) -> Self {
        Self {
            id: m.id,
            slug: m.slug.clone(),
            name_zh: m.name_zh.clone(),
            name_en: m.name_en.clone(),
        }
    }
}

#[derive(Serialize, Clone)]
pub struct SeriesOut {
    pub id: i32,
    pub slug: String,
    pub name_zh: String,
    pub name_en: String,
}

impl From<&series::Model> for SeriesOut {
    fn from(m: &series::Model) -> Self {
        Self {
            id: m.id,
            slug: m.slug.clone(),
            name_zh: m.name_zh.clone(),
            name_en: m.name_en.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct PostListItem {
    pub id: i32,
    pub group_id: String,
    pub lang: String,
    pub slug: String,
    pub title: String,
    pub summary: Option<String>,
    pub cover: Option<String>,
    pub status: String,
    pub category: Option<CategoryOut>,
    pub tags: Vec<TagOut>,
    pub series: Option<SeriesOut>,
    pub series_order: Option<i32>,
    pub view_count: i32,
    pub published_at: Option<DateTimeWithTimeZone>,
    pub updated_at: DateTimeWithTimeZone,
}

/// 批量补全文章的分类 / 标签 / 系列信息
pub async fn hydrate_posts(
    db: &DatabaseConnection,
    items: Vec<posts::Model>,
) -> ApiResult<Vec<PostListItem>> {
    let post_ids: Vec<i32> = items.iter().map(|p| p.id).collect();
    let category_ids: HashSet<i32> = items.iter().filter_map(|p| p.category_id).collect();
    let series_ids: HashSet<i32> = items.iter().filter_map(|p| p.series_id).collect();

    let category_map: HashMap<i32, categories::Model> = if category_ids.is_empty() {
        HashMap::new()
    } else {
        categories::Entity::find()
            .filter(categories::Column::Id.is_in(category_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|c| (c.id, c))
            .collect()
    };

    let series_map: HashMap<i32, series::Model> = if series_ids.is_empty() {
        HashMap::new()
    } else {
        series::Entity::find()
            .filter(series::Column::Id.is_in(series_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|s| (s.id, s))
            .collect()
    };

    let relations: Vec<post_tags::Model> = if post_ids.is_empty() {
        Vec::new()
    } else {
        post_tags::Entity::find()
            .filter(post_tags::Column::PostId.is_in(post_ids))
            .all(db)
            .await?
    };
    let tag_ids: HashSet<i32> = relations.iter().map(|r| r.tag_id).collect();
    let tag_map: HashMap<i32, tags::Model> = if tag_ids.is_empty() {
        HashMap::new()
    } else {
        tags::Entity::find()
            .filter(tags::Column::Id.is_in(tag_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|t| (t.id, t))
            .collect()
    };
    let mut post_tag_map: HashMap<i32, Vec<TagOut>> = HashMap::new();
    for rel in &relations {
        if let Some(tag) = tag_map.get(&rel.tag_id) {
            post_tag_map
                .entry(rel.post_id)
                .or_default()
                .push(TagOut::from(tag));
        }
    }

    Ok(items
        .into_iter()
        .map(|p| PostListItem {
            category: p
                .category_id
                .and_then(|id| category_map.get(&id))
                .map(CategoryOut::from),
            series: p
                .series_id
                .and_then(|id| series_map.get(&id))
                .map(SeriesOut::from),
            tags: post_tag_map.remove(&p.id).unwrap_or_default(),
            id: p.id,
            group_id: p.group_id,
            lang: p.lang,
            slug: p.slug,
            title: p.title,
            summary: p.summary,
            cover: p.cover,
            status: p.status,
            series_order: p.series_order,
            view_count: p.view_count,
            published_at: p.published_at,
            updated_at: p.updated_at,
        })
        .collect())
}

pub fn validate_lang(lang: &str) -> bool {
    matches!(lang, "zh" | "en")
}
