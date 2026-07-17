use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "posts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub slug: String,
    pub title: String,
    pub summary: Option<String>,
    pub cover: Option<String>,
    pub content_md: String,
    pub content_html: String,
    pub toc_json: Option<String>,
    #[serde(skip_serializing)]
    pub search_text: String,
    pub status: String,
    pub category_id: Option<i32>,
    pub series_id: Option<i32>,
    pub series_order: Option<i32>,
    pub view_count: i32,
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    pub published_at: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub const STATUS_DRAFT: &str = "draft";
pub const STATUS_PUBLISHED: &str = "published";
